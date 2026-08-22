//! Auction UI (M5) — the crown jewel of the rewrite.
//!
//! Design note: buttons are **stateless**. Every component carries its
//! ledger auction id, so handling is a pure function of (custom id, ledger
//! state) — no in-memory collector has to survive for an auction to work,
//! and a restart mid-auction changes nothing (hazards B11/B12; audit #7/#40
//! where every crash erased live auctions).
//!
//! The only in-memory state is a message registry (auction → posted embed),
//! which is presentation, not truth: on boot, open auctions re-post.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context as _;
use poise::serenity_prelude as serenity;

use nocturnal_core::event::Flavor;
use nocturnal_core::state::{Auction, AuctionStatus};
use nocturnal_core::{Actor, Command, GuildId, PlayerId};

use crate::discord::{
    chrono_now_ms, discord_call, item_embed, officer_check, rejection_text, require_guild, ts_sec,
    Context, Error, EMBED_GREEN, EMBED_ORANGE,
};
use crate::driver::DriverHandle;

const EMBED_RED: u32 = 15_158_332;
/// Legacy grace period: long auctions finalize 20 minutes after their deadline.
pub const LONG_AUCTION_GRACE_MS: i64 = 20 * 60 * 1000;

// ---------------------------------------------------------------------------
// Component ids: "nb:<action>:<auction id>"
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Bid,
    BidAlt,
    Cancel,
    Confirm,
}

impl Action {
    fn tag(self) -> &'static str {
        match self {
            Action::Bid => "bid",
            Action::BidAlt => "alt",
            Action::Cancel => "cancel",
            Action::Confirm => "confirm",
        }
    }

    fn parse(tag: &str) -> Option<Action> {
        match tag {
            "bid" => Some(Action::Bid),
            "alt" => Some(Action::BidAlt),
            "cancel" => Some(Action::Cancel),
            "confirm" => Some(Action::Confirm),
            _ => None,
        }
    }
}

pub fn custom_id(action: Action, auction_id: &str) -> String {
    format!("nb:{}:{auction_id}", action.tag())
}

pub fn parse_custom_id(id: &str) -> Option<(Action, &str)> {
    let rest = id.strip_prefix("nb:")?;
    let (tag, auction_id) = rest.split_once(':')?;
    Some((Action::parse(tag)?, auction_id))
}

// ---------------------------------------------------------------------------
// Rendering (legacy formats)
// ---------------------------------------------------------------------------

fn winners_text(winners: &[nocturnal_core::event::Winner]) -> String {
    if winners.is_empty() {
        return "No winner".to_owned();
    }
    winners
        .iter()
        .map(|w| {
            format!(
                "<@{}>{} for {} dkp",
                w.player,
                if w.for_main { "" } else { " - alter" },
                w.amount
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Bids are shown anonymised (amount only), exactly like the legacy embeds.
fn bids_text(auction: &Auction) -> String {
    if auction.bids.is_empty() {
        return "No bids".to_owned();
    }
    let mut bids: Vec<&nocturnal_core::state::Bid> = auction.bids.iter().collect();
    bids.sort_by_key(|b| std::cmp::Reverse(b.amount));
    bids.iter()
        .map(|b| format!("- {}{}", b.amount, if b.for_main { "" } else { " - alter" }))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The live (bidding) embed plus its buttons.
pub fn live_message(
    auction_id: &str,
    auction: &Auction,
) -> (serenity::CreateEmbed, Vec<serenity::CreateActionRow>) {
    let embed = item_embed(&auction.item, EMBED_ORANGE).fields([
        (
            "Auction ends",
            format!("<t:{}:R>", ts_sec(auction.deadline_ts_ms)),
            true,
        ),
        ("Minimum bid", format!("{} DKP", auction.min_bid), true),
        ("Auction ID", format!("```{auction_id}```"), false),
    ]);
    let embed = if auction.num_items > 1 {
        embed.description(format!("Top **{}** bids win.", auction.num_items))
    } else {
        embed
    };
    let row = match auction.flavor {
        // Short auctions bid via buttons + DM (legacy UX, kept).
        Flavor::Short => serenity::CreateActionRow::Buttons(vec![
            serenity::CreateButton::new(custom_id(Action::Bid, auction_id))
                .label("I want to bid")
                .style(serenity::ButtonStyle::Primary),
            serenity::CreateButton::new(custom_id(Action::BidAlt, auction_id))
                .label("Bid for Alter")
                .style(serenity::ButtonStyle::Secondary),
            serenity::CreateButton::new(custom_id(Action::Cancel, auction_id))
                .label("Cancel")
                .style(serenity::ButtonStyle::Danger),
        ]),
        // Long auctions bid with /bid <auction id>.
        Flavor::Long => serenity::CreateActionRow::Buttons(vec![serenity::CreateButton::new(
            custom_id(Action::Cancel, auction_id),
        )
        .label("Cancel")
        .style(serenity::ButtonStyle::Danger)]),
    };
    (embed, vec![row])
}

/// Closed short auction: winners proposed, awaiting the officer's confirm.
pub fn closed_message(
    auction_id: &str,
    auction: &Auction,
    proposed: &[nocturnal_core::event::Winner],
) -> (serenity::CreateEmbed, Vec<serenity::CreateActionRow>) {
    let embed = item_embed(&auction.item, EMBED_GREEN).fields([
        ("Winner/s", winners_text(proposed), false),
        ("Bids", bids_text(auction), false),
        ("Auction ID", format!("```{auction_id}```"), false),
    ]);
    let rows = if proposed.is_empty() {
        Vec::new()
    } else {
        vec![serenity::CreateActionRow::Buttons(vec![
            serenity::CreateButton::new(custom_id(Action::Confirm, auction_id))
                .label("Confirm Winner/s")
                .style(serenity::ButtonStyle::Primary),
        ])]
    };
    (embed, rows)
}

/// Terminal states: finalized (charged) or cancelled.
pub fn settled_message(
    auction_id: &str,
    auction: &Auction,
) -> (serenity::CreateEmbed, Vec<serenity::CreateActionRow>) {
    let (color, status) = match auction.status {
        AuctionStatus::Finalized => (EMBED_GREEN, "Winner/s confirmed — DKP charged"),
        AuctionStatus::Cancelled => (EMBED_RED, "Auction cancelled"),
        _ => (EMBED_ORANGE, "…"),
    };
    let embed = item_embed(&auction.item, color).fields([
        ("Winner/s", winners_text(&auction.winners), false),
        ("Bids", bids_text(auction), false),
        ("Status", status.to_owned(), true),
        ("Auction ID", format!("```{auction_id}```"), false),
    ]);
    (embed, Vec::new())
}

// ---------------------------------------------------------------------------
// Message registry + rendering side effects
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct AuctionUi {
    /// auction id → (channel, message) of its posted embed. Presentation
    /// state: lost on restart, rebuilt by re-posting open auctions.
    messages: Mutex<HashMap<String, (u64, u64)>>,
    /// (user, auction) pairs with a DM prompt already open — stops the
    /// legacy behaviour of stacking a new collector per click (audit #39).
    dm_pending: Mutex<HashSet<(u64, String)>>,
}

impl AuctionUi {
    pub fn remember(&self, auction_id: &str, channel: u64, message: u64) {
        if let Ok(mut m) = self.messages.lock() {
            m.insert(auction_id.to_owned(), (channel, message));
        }
    }

    pub fn locate(&self, auction_id: &str) -> Option<(u64, u64)> {
        self.messages.lock().ok()?.get(auction_id).copied()
    }

    pub fn forget(&self, auction_id: &str) {
        if let Ok(mut m) = self.messages.lock() {
            m.remove(auction_id);
        }
    }

    fn dm_open(&self, user: u64, auction_id: &str) -> bool {
        self.dm_pending
            .lock()
            .map(|mut p| p.insert((user, auction_id.to_owned())))
            .unwrap_or(false)
    }

    fn dm_done(&self, user: u64, auction_id: &str) {
        if let Ok(mut p) = self.dm_pending.lock() {
            p.remove(&(user, auction_id.to_owned()));
        }
    }
}

/// Re-render an auction's embed to match ledger state. Never fatal.
pub async fn refresh(
    http: &serenity::Http,
    ui: &AuctionUi,
    driver: &DriverHandle,
    ledger_guild: GuildId,
    auction_id: &str,
) {
    let Some((channel, message)) = ui.locate(auction_id) else {
        return;
    };
    let aid = auction_id.to_owned();
    let Some(auction) = driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .and_then(|g| g.auctions.get(&aid).cloned())
        })
        .await
    else {
        return;
    };
    let (embed, rows) = match auction.status {
        AuctionStatus::Open => live_message(auction_id, &auction),
        AuctionStatus::Closed => {
            let aid = auction_id.to_owned();
            let proposed = driver
                .query(move |l| {
                    l.state()
                        .guild(ledger_guild)
                        .map(|g| nocturnal_core::compute_winners(g, &aid, 0))
                        .unwrap_or_default()
                })
                .await;
            closed_message(auction_id, &auction, &proposed)
        }
        AuctionStatus::Finalized | AuctionStatus::Cancelled => {
            ui.forget(auction_id);
            settled_message(auction_id, &auction)
        }
    };
    let result = discord_call("edit auction embed", async {
        serenity::ChannelId::new(channel)
            .edit_message(
                http,
                serenity::MessageId::new(message),
                serenity::EditMessage::new().embed(embed).components(rows),
            )
            .await
    })
    .await;
    if let Err(e) = result {
        tracing::warn!(auction_id, error = %e, "auction embed refresh failed");
    }
}

/// Post an auction's embed to its channel and remember where it went.
pub async fn post(
    http: &serenity::Http,
    ui: &AuctionUi,
    channel: u64,
    auction_id: &str,
    auction: &Auction,
) -> anyhow::Result<()> {
    let (embed, rows) = live_message(auction_id, auction);
    let content = format!(
        "Bid started - **{} DKP** minimum bid.{}",
        auction.min_bid,
        match auction.flavor {
            Flavor::Long => format!("  Bid with `/bid auctionid:{auction_id}`"),
            Flavor::Short => String::new(),
        }
    );
    let msg = discord_call("post auction embed", async {
        serenity::ChannelId::new(channel)
            .send_message(
                http,
                serenity::CreateMessage::new()
                    .content(content)
                    .embed(embed)
                    .components(rows),
            )
            .await
    })
    .await
    .context("posting auction embed")?;
    ui.remember(auction_id, channel, msg.id.get());
    Ok(())
}

/// Boot recovery (hazard B11): every auction still open in the ledger gets a
/// fresh embed, so its buttons work again after a restart.
pub async fn repost_open_auctions(
    http: &serenity::Http,
    ui: &AuctionUi,
    driver: &DriverHandle,
    ledger_guild: GuildId,
) {
    let open = driver
        .query(move |l| {
            let Some(g) = l.state().guild(ledger_guild) else {
                return Vec::new();
            };
            let short = g.config.auction_channel;
            let long = g.config.long_auction_channel.or(short);
            g.auctions
                .iter()
                .filter(|(_, a)| a.status == AuctionStatus::Open)
                .filter_map(|(id, a)| {
                    let channel = match a.flavor {
                        Flavor::Short => short,
                        Flavor::Long => long,
                    }?;
                    Some((id.clone(), a.clone(), channel))
                })
                .collect::<Vec<_>>()
        })
        .await;
    if open.is_empty() {
        return;
    }
    tracing::info!(
        count = open.len(),
        "re-posting auctions that survived the restart"
    );
    for (id, auction, channel) in open {
        if let Err(e) = post(http, ui, channel, &id, &auction).await {
            tracing::warn!(auction_id = %id, error = %e, "re-post failed");
        }
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Legacy item-picker flow shared by both auction commands.
async fn pick_item(
    ctx: &Context<'_>,
    search: &str,
    database: Option<String>,
) -> Result<Option<nocturnal_core::Item>, Error> {
    let Some(db) = crate::items::Database::parse(database.as_deref().unwrap_or("quarm")) else {
        ctx.say("Invalid database option. Must be quarm or takp")
            .await?;
        return Ok(None);
    };
    let outcome = match ctx.data().items.search(search, db).await {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "item search failed");
            ctx.say(format!(":no_entry: Item lookup failed: {e}"))
                .await?;
            return Ok(None);
        }
    };
    let refs = match outcome {
        crate::items::SearchOutcome::None => {
            ctx.say("No items found").await?;
            return Ok(None);
        }
        crate::items::SearchOutcome::One(item) => return Ok(Some(item)),
        crate::items::SearchOutcome::Many(refs) if refs.len() > 40 => {
            ctx.say(format!("List too long ({}), refine search", refs.len()))
                .await?;
            return Ok(None);
        }
        crate::items::SearchOutcome::Many(refs) if refs.len() > 25 => {
            let listing = refs
                .iter()
                .map(|r| format!("#{:<10} {}", r.id, r.name))
                .collect::<Vec<_>>()
                .join("\n");
            ctx.send(
                poise::CreateReply::default()
                    .embed(
                        serenity::CreateEmbed::new()
                            .title("Search Results")
                            .description(listing),
                    )
                    .ephemeral(true),
            )
            .await?;
            return Ok(None);
        }
        crate::items::SearchOutcome::Many(refs) => refs,
    };

    let ctx_id = ctx.id();
    let rows: Vec<serenity::CreateActionRow> = refs
        .chunks(5)
        .map(|chunk| {
            serenity::CreateActionRow::Buttons(
                chunk
                    .iter()
                    .map(|r| {
                        serenity::CreateButton::new(format!("{ctx_id}item{}", r.id))
                            .label(r.name.chars().take(80).collect::<String>())
                            .style(serenity::ButtonStyle::Secondary)
                    })
                    .collect(),
            )
        })
        .collect();
    let msg = ctx
        .send(
            poise::CreateReply::default()
                .content("Search Results")
                .components(rows)
                .ephemeral(true),
        )
        .await?;
    let press = serenity::collector::ComponentInteractionCollector::new(ctx)
        .filter(move |p| p.data.custom_id.starts_with(&format!("{ctx_id}item")))
        .timeout(Duration::from_secs(30))
        .await;
    let Some(press) = press else {
        msg.edit(
            *ctx,
            poise::CreateReply::default()
                .content("Time out")
                .components(vec![]),
        )
        .await?;
        return Ok(None);
    };
    press.defer(ctx.serenity_context()).await?;
    let id = press.data.custom_id[format!("{ctx_id}item").len()..].to_owned();
    let db = crate::items::Database::parse(database.as_deref().unwrap_or("quarm"))
        .unwrap_or(crate::items::Database::Quarm);
    Ok(ctx.data().items.by_id(&id, db).await.ok().flatten())
}

/// Ask the officer to confirm before the auction goes live (legacy: the
/// "Start Auction" button on the item preview, 30 s).
async fn confirm_start(ctx: &Context<'_>, item: &nocturnal_core::Item) -> Result<bool, Error> {
    let ctx_id = ctx.id();
    let start_id = format!("{ctx_id}start");
    let msg = ctx
        .send(
            poise::CreateReply::default()
                .embed(item_embed(item, EMBED_ORANGE))
                .components(vec![serenity::CreateActionRow::Buttons(vec![
                    serenity::CreateButton::new(&start_id)
                        .label("Start Auction")
                        .style(serenity::ButtonStyle::Primary),
                ])])
                .ephemeral(true),
        )
        .await?;
    let press = serenity::collector::ComponentInteractionCollector::new(ctx)
        .filter(move |p| p.data.custom_id == start_id)
        .timeout(Duration::from_secs(30))
        .await;
    match press {
        Some(press) => {
            press.defer(ctx.serenity_context()).await?;
            msg.edit(
                *ctx,
                poise::CreateReply::default()
                    .content("Bid started")
                    .components(vec![]),
            )
            .await?;
            Ok(true)
        }
        None => {
            msg.edit(
                *ctx,
                poise::CreateReply::default()
                    .content("Time out")
                    .components(vec![]),
            )
            .await?;
            Ok(false)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn open_auction(
    ctx: &Context<'_>,
    item: nocturnal_core::Item,
    flavor: Flavor,
    min_bid: Option<i64>,
    num_items: Option<u32>,
    duration_ms: i64,
) -> Result<(), Error> {
    let ledger_guild = require_guild(ctx)?;
    let (cfg_min_bid, lock, over, short_channel, long_channel) = ctx
        .data()
        .driver
        .query(move |l| {
            let g = l.state().guild(ledger_guild);
            (
                g.map_or(0, |g| g.config.min_bid),
                g.map_or(0, |g| g.config.min_bid_to_lock_for_main),
                g.map_or(0, |g| g.config.over_bid_to_win_main),
                g.and_then(|g| g.config.auction_channel),
                g.and_then(|g| g.config.long_auction_channel),
            )
        })
        .await;
    let channel = match flavor {
        Flavor::Short => short_channel,
        Flavor::Long => long_channel.or(short_channel),
    };
    let Some(channel) = channel else {
        ctx.say(":no_entry: Auction channel not set, use /configure to set it")
            .await?;
        return Ok(());
    };

    let auction_id = format!("au-{:x}", chrono_now_ms());
    let cmd = Command::OpenAuction {
        auction_id: auction_id.clone(),
        item,
        flavor,
        min_bid: min_bid.unwrap_or(cfg_min_bid),
        num_items: num_items.unwrap_or(1),
        min_bid_to_lock_for_main: lock,
        over_bid_to_win_main: over,
        duration_ms,
    };
    match ctx
        .data()
        .driver
        .execute(ledger_guild, Actor::User(ctx.author().id.get()), cmd)
        .await
    {
        Ok(_) => {
            let aid = auction_id.clone();
            let auction = ctx
                .data()
                .driver
                .query(move |l| {
                    l.state()
                        .guild(ledger_guild)
                        .and_then(|g| g.auctions.get(&aid).cloned())
                })
                .await;
            if let Some(auction) = auction {
                post(
                    ctx.serenity_context().http.as_ref(),
                    &ctx.data().auctions,
                    channel,
                    &auction_id,
                    &auction,
                )
                .await?;
            }
            tracing::info!(auction_id, "auction opened");
        }
        Err(e) => {
            ctx.say(rejection_text(&e)).await?;
        }
    }
    Ok(())
}

/// Start a short (live) auction for an item.
#[tracing::instrument(name = "command.startbid", skip_all, fields(otel.kind = "server"))]
#[poise::command(slash_command, ephemeral, rename = "startbid", check = "officer_check")]
pub async fn startbid(
    ctx: Context<'_>,
    #[description = "Item name or id"] search: String,
    #[description = "Minimum bid"]
    #[min = 0]
    minbid: Option<i64>,
    #[description = "Number of items"]
    #[min = 1]
    numitems: Option<u32>,
    #[description = "quarm | takp"] database: Option<String>,
) -> Result<(), Error> {
    let ledger_guild = require_guild(&ctx)?;
    ctx.defer_ephemeral().await?;
    let Some(item) = pick_item(&ctx, &search, database).await? else {
        return Ok(());
    };
    if !confirm_start(&ctx, &item).await? {
        return Ok(());
    }
    let bid_time_s = ctx
        .data()
        .driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .map_or(60, |g| g.config.bid_time_s)
        })
        .await;
    open_auction(
        &ctx,
        item,
        Flavor::Short,
        minbid,
        numitems,
        bid_time_s * 1000,
    )
    .await
}

/// Start a long auction (bids via /bid, default 48 hours).
#[tracing::instrument(name = "command.startlongbid", skip_all, fields(otel.kind = "server"))]
#[poise::command(
    slash_command,
    ephemeral,
    rename = "startlongbid",
    check = "officer_check"
)]
pub async fn startlongbid(
    ctx: Context<'_>,
    #[description = "Item name or id"] search: String,
    #[description = "Minimum bid"]
    #[min = 0]
    minbid: Option<i64>,
    #[description = "Number of items"]
    #[min = 1]
    numitems: Option<u32>,
    #[description = "Hours of bid (default 48)"]
    #[min = 1]
    duration: Option<i64>,
    #[description = "quarm | takp"] database: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(item) = pick_item(&ctx, &search, database).await? else {
        return Ok(());
    };
    if !confirm_start(&ctx, &item).await? {
        return Ok(());
    }
    let hours = duration.unwrap_or(48);
    open_auction(
        &ctx,
        item,
        Flavor::Long,
        minbid,
        numitems,
        hours * 3_600_000,
    )
    .await
}

/// Bid on a long auction (0 removes your bid).
#[tracing::instrument(name = "command.bid", skip_all, fields(otel.kind = "server"))]
#[poise::command(slash_command, ephemeral, rename = "bid")]
pub async fn bid(
    ctx: Context<'_>,
    #[description = "auctionid"] auctionid: String,
    #[description = "The amount of dkps"]
    #[min = 0]
    dkps: i64,
    #[description = "Bid for main (default true)"] bidformain: Option<bool>,
) -> Result<(), Error> {
    let ledger_guild = require_guild(&ctx)?;
    ctx.defer_ephemeral().await?;
    let player = ctx.author().id.get();
    let aid = auctionid.clone();
    let item_name = ctx
        .data()
        .driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .and_then(|g| g.auctions.get(&aid).map(|a| a.item.name.clone()))
        })
        .await;
    let Some(item_name) = item_name else {
        ctx.say(":no_entry: Auction not found").await?;
        return Ok(());
    };

    let cmd = if dkps == 0 {
        Command::RetractBid {
            auction_id: auctionid.clone(),
            player,
        }
    } else {
        Command::PlaceBid {
            auction_id: auctionid.clone(),
            player,
            amount: dkps,
            for_main: bidformain.unwrap_or(true),
        }
    };
    match ctx
        .data()
        .driver
        .execute(ledger_guild, Actor::User(player), cmd)
        .await
    {
        Ok(_) if dkps == 0 => {
            ctx.say(format!("Removed bid on {item_name}")).await?;
        }
        Ok(_) => {
            ctx.say(format!(
                "Bid {dkps} DKPs as {} on {item_name}",
                if bidformain.unwrap_or(true) {
                    "MAIN"
                } else {
                    "ALT"
                }
            ))
            .await?;
        }
        Err(e) => {
            ctx.say(rejection_text(&e)).await?;
        }
    }
    refresh(
        ctx.serenity_context().http.as_ref(),
        &ctx.data().auctions,
        &ctx.data().driver,
        ledger_guild,
        &auctionid,
    )
    .await;
    Ok(())
}

/// Show the details of an auction.
#[tracing::instrument(name = "command.auctiondetails", skip_all, fields(otel.kind = "server"))]
#[poise::command(
    slash_command,
    ephemeral,
    rename = "auctiondetails",
    check = "officer_check"
)]
pub async fn auctiondetails(
    ctx: Context<'_>,
    #[description = "The auction id"] auctionid: String,
) -> Result<(), Error> {
    let ledger_guild = require_guild(&ctx)?;
    ctx.defer_ephemeral().await?;
    let aid = auctionid.clone();
    let found = ctx
        .data()
        .driver
        .query(move |l| {
            l.state().guild(ledger_guild).and_then(|g| {
                g.auctions
                    .get(&aid)
                    .cloned()
                    .map(|a| (a, g.config.auction_channel))
            })
        })
        .await;
    let Some((auction, auction_channel)) = found else {
        ctx.say(":no_entry: Auction not found").await?;
        return Ok(());
    };

    // Legacy social feature (kept): peeking is announced publicly.
    if let Some(channel) = auction_channel {
        let who = ctx.author().id.get();
        let id_for_msg = auctionid.clone();
        let _ = discord_call("announce auctiondetails peek", async {
            serenity::ChannelId::new(channel)
                .say(
                    ctx.serenity_context(),
                    format!("<@{who}> used `/auctiondetails` to peek under the hood :eyes: `{id_for_msg}`"),
                )
                .await
        })
        .await;
    }

    let mut body = format!(
        "Auction details: {} - {auctionid}\nNumber of items: {}\nStatus: {:?}\nBids:\n",
        auction.item.name, auction.num_items, auction.status
    );
    for b in &auction.bids {
        body.push_str(&format!(
            "- <@{}> - {} - {}\n",
            b.player,
            b.amount,
            if b.for_main { "MAIN" } else { "ALT" }
        ));
    }
    body.push_str("Winners:\n");
    for w in &auction.winners {
        body.push_str(&format!(
            "- <@{}> - {} - {}\n",
            w.player,
            w.amount,
            if w.for_main { "MAIN" } else { "ALT" }
        ));
    }
    ctx.say(body.chars().take(1900).collect::<String>()).await?;
    Ok(())
}

pub fn commands() -> Vec<poise::Command<crate::discord::Data, Error>> {
    vec![startbid(), startlongbid(), bid(), auctiondetails()]
}

// ---------------------------------------------------------------------------
// Component handling — stateless, so buttons survive restarts (B11/B12)
// ---------------------------------------------------------------------------

use crate::discord::Data;

/// Officer gate for component clicks (same rule as commands: guild
/// Administrators bypass, otherwise the configured officer role).
async fn component_is_officer(
    interaction: &serenity::ComponentInteraction,
    driver: &DriverHandle,
    ledger_guild: GuildId,
) -> bool {
    let Some(member) = &interaction.member else {
        return false;
    };
    if member.permissions.is_some_and(|p| p.administrator()) {
        return true;
    }
    let admin_role = driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .and_then(|g| g.config.admin_role)
        })
        .await;
    admin_role.is_some_and(|r| member.roles.iter().any(|role| role.get() == r))
}

/// Acknowledge the click **immediately**. Discord allows 3 seconds; the
/// legacy bot died exactly here (audit #2/#33: `i.update()` as the first
/// response, after the handler had already done work). Everything this
/// handler does afterwards — ledger queries, DM round-trips — happens on
/// borrowed time we no longer owe Discord.
async fn ack(
    ctx: &serenity::Context,
    interaction: &serenity::ComponentInteraction,
) -> anyhow::Result<()> {
    interaction
        .create_response(
            ctx,
            serenity::CreateInteractionResponse::Defer(
                serenity::CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await
        .context("deferring component interaction")
}

/// Answer an already-acknowledged click. Never a second `create_response` —
/// that is the audit's #5 (reply-after-acknowledge throwing inside a catch).
async fn reply(
    ctx: &serenity::Context,
    interaction: &serenity::ComponentInteraction,
    text: impl Into<String>,
) -> anyhow::Result<()> {
    interaction
        .create_followup(
            ctx,
            serenity::CreateInteractionResponseFollowup::new()
                .content(text.into())
                .ephemeral(true),
        )
        .await
        .context("component follow-up")?;
    Ok(())
}

/// The DM bid flow (legacy UX, kept — hardened). Fixes from the audit:
/// the collector is bound to *this* auction (#50), a second click while a
/// prompt is open does not stack another collector (#39), and a closed DM
/// falls back to an ephemeral reply instead of throwing inside a catch (#5).
async fn dm_bid_flow(
    ctx: &serenity::Context,
    interaction: &serenity::ComponentInteraction,
    data: &Data,
    ledger_guild: GuildId,
    auction_id: &str,
    for_main: bool,
) -> anyhow::Result<()> {
    let user = interaction.user.id;
    let player: PlayerId = user.get();
    let aid = auction_id.to_owned();
    let (item_name, deadline_ts_ms) = data
        .driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .and_then(|g| g.auctions.get(&aid))
                .map(|a| (a.item.name.clone(), a.deadline_ts_ms))
                .unwrap_or_else(|| ("the item".to_owned(), 0))
        })
        .await;

    if !data.auctions.dm_open(player, auction_id) {
        return reply(
            ctx,
            interaction,
            "You already have a bid prompt open — check your DMs.",
        )
        .await;
    }
    // From here on every exit path must clear the guard, or the bidder can
    // never click this auction again.
    let result = dm_bid_inner(
        ctx,
        interaction,
        data,
        ledger_guild,
        auction_id,
        for_main,
        user,
        player,
        &item_name,
        deadline_ts_ms,
    )
    .await;
    data.auctions.dm_done(player, auction_id);
    result
}

#[allow(clippy::too_many_arguments)]
async fn dm_bid_inner(
    ctx: &serenity::Context,
    interaction: &serenity::ComponentInteraction,
    data: &Data,
    ledger_guild: GuildId,
    auction_id: &str,
    for_main: bool,
    user: serenity::UserId,
    player: PlayerId,
    item_name: &str,
    deadline_ts_ms: i64,
) -> anyhow::Result<()> {
    let dm = match user.create_dm_channel(ctx).await {
        Ok(dm) => dm,
        Err(e) => {
            tracing::info!(player, error = %e, "DM channel unavailable; ephemeral fallback");
            return reply(
                ctx,
                interaction,
                format!(
                    "Couldn't DM you (privacy settings). Bid here instead: `/controels-bid auctionid:{auction_id} dkps:<amount>`"
                ),
            )
            .await;
        }
    };
    let prompt = discord_call("dm bid prompt", async {
        dm.say(
            ctx,
            format!(
                "How much do you want to `{}` bid on **{item_name}**? Reply with a number (0 to cancel).\nBidding closes <t:{}:R>.",
                if for_main { "MAIN" } else { "ALT" },
                ts_sec(deadline_ts_ms)
            ),
        )
        .await
    })
    .await;
    if let Err(e) = prompt {
        tracing::info!(player, error = %e, "DM send failed; ephemeral fallback");
        return reply(
            ctx,
            interaction,
            format!("Couldn't DM you. Bid here instead: `/controels-bid auctionid:{auction_id} dkps:<amount>`"),
        )
        .await;
    }
    reply(ctx, interaction, "Sent — check your DMs. 📨").await?;
    tracing::info!(
        player,
        auction_id,
        for_main,
        "DM bid prompt sent; awaiting reply"
    );

    // Listen exactly as long as the auction lives (a 60-second auction gets a
    // 60-second window), plus a few seconds so a reply sent right at the bell
    // still arrives to be judged by the ledger rather than dropped here.
    let remaining_ms = (deadline_ts_ms - chrono_now_ms()).max(0) + 5_000;
    let reply_msg = serenity::collector::MessageCollector::new(ctx)
        .channel_id(dm.id)
        .author_id(user)
        .timeout(Duration::from_millis(remaining_ms as u64))
        .await;

    let Some(reply_msg) = reply_msg else {
        tracing::info!(player, auction_id, "DM bid prompt timed out");
        let _ = dm
            .say(ctx, "Bid timed out — click the button again to retry.")
            .await;
        return Ok(());
    };
    let raw = reply_msg.content.trim().to_owned();
    tracing::info!(player, auction_id, len = raw.len(), "DM bid reply received");
    let Ok(amount) = raw.parse::<i64>() else {
        let _ = dm
            .say(
                ctx,
                format!("`{raw}` is not a number — click the button again to retry."),
            )
            .await;
        return Ok(());
    };

    let cmd = if amount == 0 {
        Command::RetractBid {
            auction_id: auction_id.to_owned(),
            player,
        }
    } else {
        Command::PlaceBid {
            auction_id: auction_id.to_owned(),
            player,
            amount,
            for_main,
        }
    };
    let outcome = data
        .driver
        .execute(ledger_guild, Actor::User(player), cmd)
        .await;
    let text = match &outcome {
        Ok(_) if amount == 0 => "Bid cancelled".to_owned(),
        Ok(_) => format!(
            "Bid placed: **{amount}** DKP as {} on {item_name}",
            if for_main { "MAIN" } else { "ALT" }
        ),
        Err(e) => rejection_text(e),
    };
    tracing::info!(
        player,
        auction_id,
        amount,
        accepted = outcome.is_ok(),
        "DM bid resolved"
    );
    if let Err(e) = dm.say(ctx, &text).await {
        tracing::warn!(player, error = %e, "could not confirm bid in DM");
    }
    if outcome.is_ok() {
        refresh(
            ctx.http.as_ref(),
            &data.auctions,
            &data.driver,
            ledger_guild,
            auction_id,
        )
        .await;
    }
    Ok(())
}

/// Dispatch a component click. Everything needed is in the custom id and the
/// ledger, so a restart mid-auction changes nothing.
#[tracing::instrument(
    name = "component.auction",
    skip_all,
    fields(otel.kind = "server", nocturnal.auction.id = tracing::field::Empty)
)]
pub async fn handle_component(
    ctx: &serenity::Context,
    interaction: &serenity::ComponentInteraction,
    data: &Data,
) -> anyhow::Result<()> {
    let Some((action, auction_id)) = parse_custom_id(&interaction.data.custom_id) else {
        return Ok(()); // not ours (item pickers, pagination, …)
    };
    tracing::Span::current().record("nocturnal.auction.id", auction_id);
    // Defer-first: nothing below this line races the 3-second window.
    ack(ctx, interaction).await?;
    let Some(discord_guild) = interaction.guild_id.map(|g| g.get()) else {
        return Ok(());
    };
    let ledger_guild = match data.data_guild {
        Some((from, to)) if from == discord_guild => to,
        _ => discord_guild,
    };

    let aid = auction_id.to_owned();
    let status = data
        .driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .and_then(|g| g.auctions.get(&aid).map(|a| a.status))
        })
        .await;
    // Stale button from before a restart, or an auction already settled (B12).
    let Some(status) = status else {
        return reply(ctx, interaction, ":no_entry: This auction has ended.").await;
    };

    match action {
        Action::Bid | Action::BidAlt => {
            if status != AuctionStatus::Open {
                return reply(
                    ctx,
                    interaction,
                    ":no_entry: Bidding on this auction has closed.",
                )
                .await;
            }
            dm_bid_flow(
                ctx,
                interaction,
                data,
                ledger_guild,
                auction_id,
                action == Action::Bid,
            )
            .await
        }
        Action::Cancel => {
            if !component_is_officer(interaction, &data.driver, ledger_guild).await {
                return reply(
                    ctx,
                    interaction,
                    ":no_entry: You don't have permissions, what do you want your tombstone to say?",
                )
                .await;
            }
            let outcome = data
                .driver
                .execute(
                    ledger_guild,
                    Actor::User(interaction.user.id.get()),
                    Command::CancelAuction {
                        auction_id: auction_id.to_owned(),
                        reason: "officer".into(),
                    },
                )
                .await;
            let text = match &outcome {
                Ok(_) => "Auction cancelled.".to_owned(),
                Err(e) => rejection_text(e),
            };
            reply(ctx, interaction, text).await?;
            refresh(
                ctx.http.as_ref(),
                &data.auctions,
                &data.driver,
                ledger_guild,
                auction_id,
            )
            .await;
            Ok(())
        }
        Action::Confirm => {
            if !component_is_officer(interaction, &data.driver, ledger_guild).await {
                return reply(ctx, interaction, ":no_entry: Officers only.").await;
            }
            if status != AuctionStatus::Closed {
                return reply(
                    ctx,
                    interaction,
                    ":no_entry: This auction is not awaiting confirmation.",
                )
                .await;
            }
            // Finalization *is* the debit; the seed makes any tie-break draw
            // reproducible from the log.
            let outcome = data
                .driver
                .execute(
                    ledger_guild,
                    Actor::User(interaction.user.id.get()),
                    Command::FinalizeAuction {
                        auction_id: auction_id.to_owned(),
                        seed: chrono_now_ms() as u64,
                    },
                )
                .await;
            let text = match &outcome {
                Ok(envelopes) => {
                    let winners = envelopes
                        .iter()
                        .find_map(|e| match &e.event {
                            nocturnal_core::Event::AuctionFinalized { winners, .. } => {
                                Some(winners.clone())
                            }
                            _ => None,
                        })
                        .unwrap_or_default();
                    format!(
                        "Winner/s confirmed and charged:\n{}",
                        winners_text(&winners)
                    )
                }
                Err(e) => rejection_text(e),
            };
            reply(ctx, interaction, text).await?;
            refresh(
                ctx.http.as_ref(),
                &data.auctions,
                &data.driver,
                ledger_guild,
                auction_id,
            )
            .await;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{custom_id, parse_custom_id, Action};

    #[test]
    fn custom_ids_round_trip() {
        for action in [Action::Bid, Action::BidAlt, Action::Cancel, Action::Confirm] {
            let id = custom_id(action, "au-1234abcd");
            assert!(id.len() <= 100, "Discord custom_id limit");
            assert_eq!(parse_custom_id(&id), Some((action, "au-1234abcd")));
        }
    }

    #[test]
    fn foreign_custom_ids_are_ignored() {
        for id in [
            "",
            "nb:",
            "nb:bogus:x",
            "1234item99",
            "confirm_abc",
            "nb:bid",
        ] {
            assert_eq!(parse_custom_id(id), None, "{id}");
        }
    }
}
