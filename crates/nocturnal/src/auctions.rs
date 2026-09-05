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

use nocturnal_telemetry::attr;
use std::collections::HashMap;
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
const EMBED_BLUE: u32 = 3_447_003;
/// Legacy grace period: long auctions finalize 20 minutes after their deadline.
pub const LONG_AUCTION_GRACE_MS: i64 = 20 * 60 * 1000;

// ---------------------------------------------------------------------------
// Component ids: "nb:<action>:<auction id>[:<character>]"
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Bid,
    BidAlt,
    Cancel,
    Confirm,
    /// The character select shown when a member has more than one eligible
    /// character on a side (character bids). Its value is the character.
    PickMain,
    PickAlt,
}

impl Action {
    fn tag(self) -> &'static str {
        match self {
            Action::Bid => "bid",
            Action::BidAlt => "alt",
            Action::Cancel => "cancel",
            Action::Confirm => "confirm",
            Action::PickMain => "pickm",
            Action::PickAlt => "picka",
        }
    }

    fn parse(tag: &str) -> Option<Action> {
        match tag {
            "bid" => Some(Action::Bid),
            "alt" => Some(Action::BidAlt),
            "cancel" => Some(Action::Cancel),
            "confirm" => Some(Action::Confirm),
            "pickm" => Some(Action::PickMain),
            "picka" => Some(Action::PickAlt),
            _ => None,
        }
    }

    fn for_main(self) -> bool {
        matches!(self, Action::Bid | Action::PickMain)
    }
}

pub fn custom_id(action: Action, auction_id: &str) -> String {
    format!("nb:{}:{auction_id}", action.tag())
}

/// A bid modal's id names the character the bid is for, when there is one:
/// the modal submission is a separate interaction and carries nothing else.
/// Auction ids are `au-<hex>`, character names are letters, so the colon is
/// free to separate them.
pub fn bid_custom_id(for_main: bool, auction_id: &str, character: Option<&str>) -> String {
    let action = if for_main {
        Action::Bid
    } else {
        Action::BidAlt
    };
    match character {
        Some(c) => format!("nb:{}:{auction_id}:{c}", action.tag()),
        None => custom_id(action, auction_id),
    }
}

/// `(action, auction id, character)` — the character only on a bid modal.
pub fn parse_custom_id(id: &str) -> Option<(Action, &str, Option<&str>)> {
    let rest = id.strip_prefix("nb:")?;
    let (tag, rest) = rest.split_once(':')?;
    let (auction_id, character) = match rest.split_once(':') {
        Some((a, c)) if !c.is_empty() => (a, Some(c)),
        _ => (rest, None),
    };
    Some((Action::parse(tag)?, auction_id, character))
}

// ---------------------------------------------------------------------------
// Rendering (legacy formats)
// ---------------------------------------------------------------------------

/// Discord caps an embed field value at 1024 characters and rejects the whole
/// message above it — so a full raid bidding on one item does not produce a
/// long embed, it produces no embed at all, and the auction looks dead. Lines
/// are dropped from the end and the count is kept, because the top bids are
/// the ones anyone reads.
const FIELD_LIMIT: usize = 1024;

fn field_lines(lines: Vec<String>, empty: &str) -> String {
    if lines.is_empty() {
        return empty.to_owned();
    }
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let remaining = lines.len() - i;
        // Reserve room for the "and N more" line before committing to this one.
        let tail = format!("\n…and {remaining} more");
        if out.len() + 1 + line.len() + tail.len() > FIELD_LIMIT {
            out.push_str(&tail);
            return out;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    out
}

fn winners_text(winners: &[nocturnal_core::event::Winner]) -> String {
    field_lines(
        winners
            .iter()
            .map(|w| {
                format!(
                    "<@{}>{}{} for {} dkp",
                    w.player,
                    w.character
                        .as_deref()
                        .map(|c| format!(" ({c})"))
                        .unwrap_or_default(),
                    if w.for_main { "" } else { " - alter" },
                    w.amount
                )
            })
            .collect(),
        "No winner",
    )
}

/// Bids are shown anonymised (amount only), exactly like the legacy embeds.
fn bids_text(auction: &Auction) -> String {
    let mut bids: Vec<&nocturnal_core::state::Bid> = auction.bids.iter().collect();
    bids.sort_by_key(|b| std::cmp::Reverse(b.amount));
    field_lines(
        bids.iter()
            .map(|b| format!("- {}{}", b.amount, if b.for_main { "" } else { " - alter" }))
            .collect(),
        "No bids",
    )
}

/// The live (bidding) message — legacy `sendAuctionStartEmbed` /
/// `sendLongAuctionEmbed`: short auctions are orange with a single "Auction
/// ends" field and the three bid buttons; long auctions are blue, carry the
/// auction id to bid against, and have no buttons at all.
pub fn live_message(
    auction_id: &str,
    auction: &Auction,
) -> (
    String,
    serenity::CreateEmbed,
    Vec<serenity::CreateActionRow>,
) {
    match auction.flavor {
        Flavor::Short => {
            let content = format!(
                "Bid started - **{} DKP** minimum bid.{}",
                auction.min_bid,
                if auction.num_items > 1 {
                    format!(" Top **{}** bids win", auction.num_items)
                } else {
                    String::new()
                }
            );
            let embed = item_embed(&auction.item, EMBED_ORANGE).field(
                "Auction ends",
                format!("<t:{}:R>", ts_sec(auction.deadline_ts_ms)),
                true,
            );
            let row = serenity::CreateActionRow::Buttons(vec![
                serenity::CreateButton::new(custom_id(Action::Bid, auction_id))
                    .label("Main bid")
                    .style(serenity::ButtonStyle::Primary),
                serenity::CreateButton::new(custom_id(Action::BidAlt, auction_id))
                    .label("Alt bid")
                    .style(serenity::ButtonStyle::Secondary),
                serenity::CreateButton::new(custom_id(Action::Cancel, auction_id))
                    .label("Cancel")
                    .style(serenity::ButtonStyle::Danger),
            ]);
            (content, embed, vec![row])
        }
        Flavor::Long => {
            let content = format!(
                "Bid started - **{} DKP** minimum bid.{}",
                auction.min_bid,
                if auction.num_items > 1 {
                    format!(
                        " Top **{}** bids win. Should end at <t:{}:f>",
                        auction.num_items,
                        ts_sec(auction.deadline_ts_ms)
                    )
                } else {
                    String::new()
                }
            );
            let embed = item_embed(&auction.item, EMBED_BLUE)
                .field("Auction ID", format!("```{auction_id}```"), true)
                .field(
                    "Auction ends",
                    format!("<t:{}:R>", ts_sec(auction.deadline_ts_ms)),
                    true,
                );
            // The same buttons as a short auction. They keep working across a
            // restart because the auction id is in the custom id and the
            // auction itself is in the ledger — nothing here is a listener
            // that dies with the process (B11/B12).
            let row = serenity::CreateActionRow::Buttons(vec![
                serenity::CreateButton::new(custom_id(Action::Bid, auction_id))
                    .label("Main bid")
                    .style(serenity::ButtonStyle::Primary),
                serenity::CreateButton::new(custom_id(Action::BidAlt, auction_id))
                    .label("Alt bid")
                    .style(serenity::ButtonStyle::Secondary),
            ]);
            (content, embed, vec![row])
        }
    }
}

/// Closed short auction: winners proposed, awaiting the officer's confirm
/// (legacy `callback` in startbid.js — bids shown anonymised).
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

/// Terminal states. Legacy keeps the button as the status indicator: a
/// disabled green "Winner/s Confirmed", or a disabled red "Auction Cancelled".
pub fn settled_message(
    auction_id: &str,
    auction: &Auction,
) -> (serenity::CreateEmbed, Vec<serenity::CreateActionRow>) {
    match auction.status {
        AuctionStatus::Cancelled => {
            let embed = item_embed(&auction.item, EMBED_RED).field(
                "Auction ID",
                format!("```{auction_id}```"),
                false,
            );
            let row = serenity::CreateActionRow::Buttons(vec![serenity::CreateButton::new(
                custom_id(Action::Cancel, auction_id),
            )
            .label("Auction Cancelled")
            .style(serenity::ButtonStyle::Danger)
            .disabled(true)]);
            (embed, vec![row])
        }
        _ => {
            let mut embed = item_embed(&auction.item, EMBED_GREEN);
            if auction.flavor == Flavor::Long {
                embed = embed
                    .field("Auction ID", format!("```{auction_id}```"), true)
                    .field(
                        "Auction ends",
                        format!("<t:{}:R>", ts_sec(auction.deadline_ts_ms)),
                        true,
                    )
                    .field("Winner/s", winners_text(&auction.winners), false)
                    .field("Bids", bids_text(auction), false);
                return (embed, Vec::new());
            }
            embed = embed.fields([
                ("Winner/s", winners_text(&auction.winners), false),
                ("Bids", bids_text(auction), false),
                ("Auction ID", format!("```{auction_id}```"), false),
            ]);
            let row = serenity::CreateActionRow::Buttons(vec![serenity::CreateButton::new(
                custom_id(Action::Confirm, auction_id),
            )
            .label("Winner/s Confirmed")
            .style(serenity::ButtonStyle::Success)
            .disabled(true)]);
            (embed, vec![row])
        }
    }
}

// ---------------------------------------------------------------------------
// Message registry + rendering side effects
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct AuctionUi {
    /// auction id → (channel, message) of its posted embed. Presentation
    /// state: lost on restart, rebuilt by re-posting open auctions.
    messages: Mutex<HashMap<String, (u64, u64)>>,
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
}

/// Re-render an auction's embed to match ledger state. Never fatal.
/// Returns whether the auction's post now shows its current state. A `false`
/// means the officer was told something the channel was not: `/endauction`
/// warns on it, because a settled auction whose message still shows live bid
/// buttons is how someone bids on an item that is already gone.
pub async fn refresh(
    http: &serenity::Http,
    ui: &AuctionUi,
    driver: &DriverHandle,
    ledger_guild: GuildId,
    auction_id: &str,
) -> bool {
    let Some((channel, message)) = ui.locate(auction_id) else {
        return false;
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
        return false;
    };
    let (embed, rows) = match auction.status {
        AuctionStatus::Open => {
            let (_, embed, rows) = live_message(auction_id, &auction);
            (embed, rows)
        }
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
        tracing::warn!(
            { attr::NOCTURNAL_AUCTION_ID } = auction_id,
            { attr::NOCTURNAL_ERROR_MESSAGE } = %e,
            "auction embed refresh failed"
        );
        return false;
    }
    true
}

/// Post an auction's embed to its channel and remember where it went.
pub async fn post(
    http: &serenity::Http,
    ui: &AuctionUi,
    channel: u64,
    auction_id: &str,
    auction: &Auction,
) -> anyhow::Result<()> {
    let (content, embed, rows) = live_message(auction_id, auction);
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
        { attr::NOCTURNAL_AUCTION_OPEN_COUNT } = open.len(),
        "re-posting auctions that survived the restart"
    );
    for (id, auction, channel) in open {
        if let Err(e) = post(http, ui, channel, &id, &auction).await {
            tracing::warn!(
                { attr::NOCTURNAL_AUCTION_ID } = %id,
                { attr::NOCTURNAL_ERROR_MESSAGE } = %e,
                "re-post failed"
            );
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
            tracing::warn!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "item search failed");
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
    crate::discord::ack_component(ctx.serenity_context(), &press).await?;
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
            crate::discord::ack_component(ctx.serenity_context(), &press).await?;
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
    let item_id = item.id.clone();
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
    // The item row for the character picker, fetched now so a click later
    // reads it from disk. Fire and forget: a miss only costs the picker its
    // usability filter.
    if let Ok(id) = item_id.parse::<i64>() {
        let mirror = ctx.data().item_mirror.clone();
        tokio::spawn(async move {
            mirror.get(id).await;
        });
    }
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
            tracing::info!(
                { attr::NOCTURNAL_AUCTION_ID } = auction_id,
                "auction opened"
            );
            // The bell, as officers know it: both raid channels, right after
            // the auction embed goes up. Decorative and never fatal.
            if flavor == Flavor::Short && ctx.data().bell.enabled {
                let (raid_channel, second) = ctx
                    .data()
                    .driver
                    .query(move |l| {
                        let g = l.state().guild(ledger_guild);
                        (
                            g.and_then(|g| g.config.raid_channel),
                            g.and_then(|g| g.config.second_raid_channel),
                        )
                    })
                    .await;
                let channels: Vec<u64> = [raid_channel, second].into_iter().flatten().collect();
                crate::bell::ring(
                    ctx.serenity_context(),
                    ctx.guild_id().map(|g| g.get()).unwrap_or_default(),
                    channels,
                    ctx.data().bell.path.clone(),
                );
            }
        }
        Err(e) => {
            ctx.say(rejection_text(&e)).await?;
        }
    }
    Ok(())
}

/// Start a short (live) auction for an item.
#[tracing::instrument(name = "command.startbid", skip_all, err, fields(otel.kind = "server"))]
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
    crate::discord::ack_ephemeral(&ctx).await?;
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
#[tracing::instrument(name = "command.startlongbid", skip_all, err, fields(otel.kind = "server"))]
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
    crate::discord::ack_ephemeral(&ctx).await?;
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

/// Show the details of an auction.
#[tracing::instrument(name = "command.auctiondetails", skip_all, err, fields(otel.kind = "server"))]
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
    crate::discord::ack_ephemeral(&ctx).await?;
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

    // Officers bid too, so the standing bids of a live auction are shown to
    // nobody — reading them is worth an item. A cancelled auction is readable
    // at once: its bids are dead, and the officer asking is usually asking
    // why it was pulled.
    if !details_readable(auction.status) {
        ctx.say(format!(
            ":no_entry: `{auctionid}` is still running — the bids stay sealed until it closes."
        ))
        .await?;
        return Ok(());
    }

    // Legacy social feature (kept): peeking is announced publicly. Only on a
    // reading that actually happened — a refusal is not a peek.
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
        "Auction details: {} - {auctionid}\nNumber of items: {}\nStatus: {:?}\n",
        auction.item.name, auction.num_items, auction.status
    );
    if auction.status == AuctionStatus::Cancelled {
        body.push_str(&match (auction.cancelled_by, auction.cancelled_ts_ms) {
            (Some(who), Some(ts)) => format!("Cancelled by <@{who}> <t:{}:R>\n", ts / 1000),
            (None, Some(ts)) => format!("Cancelled by the bot <t:{}:R>\n", ts / 1000),
            _ => "Cancelled\n".to_owned(),
        });
    }
    body.push_str("Bids:\n");
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

/// Whether `/auctiondetails` may show an auction's bids.
///
/// Only a running auction is sealed. Officers bid too, so publishing the
/// standing bids of a live auction is worth an item to whoever reads them —
/// and unlike most leaks this one is invisible, because the command answers
/// ephemerally. Everything settled is readable, cancelled auctions included:
/// their bids are dead, and that is usually what the officer is asking about.
fn details_readable(status: AuctionStatus) -> bool {
    !matches!(status, AuctionStatus::Open)
}

/// Stricter than `officer_check`: these two ask for the officer role *itself*,
/// and an Administrator who was never given it is refused.
///
/// Pulling or force-closing a live auction moves DKP, and the guild already
/// said who may do that. With no officer role configured there is nothing to
/// ask for, so it falls back to Administrator — a server that has not run
/// `/configure` yet is not left unable to stop a 48-hour auction.
async fn officer_role_check(ctx: Context<'_>) -> Result<bool, Error> {
    let Some(member) = ctx.author_member().await else {
        return Ok(false);
    };
    let ledger_guild = require_guild(&ctx)?;
    let admin_role = ctx
        .data()
        .driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .and_then(|g| g.config.admin_role)
        })
        .await;
    let allowed = match admin_role {
        Some(role) => member.roles.iter().any(|r| r.get() == role),
        None => member.permissions.is_some_and(|p| p.administrator()),
    };
    if !allowed {
        ctx.say(match admin_role {
            Some(role) => format!(
                ":no_entry: This one needs the officer role itself (<@&{role}>) — \
                 Administrator alone is not enough for an auction that moves DKP."
            ),
            None => ":no_entry: You don't have the permission to use this command".to_owned(),
        })
        .await?;
    }
    Ok(allowed)
}

/// Look an auction up, or tell the caller it is not there.
async fn find_auction(ctx: &Context<'_>, auction_id: &str) -> Result<Option<Auction>, Error> {
    let ledger_guild = require_guild(ctx)?;
    let aid = auction_id.to_owned();
    let found = ctx
        .data()
        .driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .and_then(|g| g.auctions.get(&aid).cloned())
        })
        .await;
    if found.is_none() {
        ctx.say(":no_entry: Auction not found").await?;
    }
    Ok(found)
}

/// Void a running auction: bids stop, no winner is picked, no DKP moves.
#[tracing::instrument(name = "command.cancelauction", skip_all, err, fields(otel.kind = "server"))]
#[poise::command(
    slash_command,
    ephemeral,
    rename = "cancelauction",
    check = "officer_role_check"
)]
pub async fn cancelauction(
    ctx: Context<'_>,
    #[description = "The auction id"] auctionid: String,
) -> Result<(), Error> {
    let ledger_guild = require_guild(&ctx)?;
    crate::discord::ack_ephemeral(&ctx).await?;
    if find_auction(&ctx, &auctionid).await?.is_none() {
        return Ok(());
    }

    // The ledger's own rule decides what may still be cancelled: anything that
    // has not been finalized. Unlike the legacy bot, closing does not move
    // DKP here — finalizing does — so an auction awaiting its confirmation is
    // still safely voidable, and one already paid out is not (that is
    // /adddkp's job).
    let outcome = crate::discord::execute(
        &ctx,
        Command::CancelAuction {
            auction_id: auctionid.clone(),
            reason: "officer".into(),
        },
    )
    .await?;
    match outcome {
        Ok(_) => {
            // The bids stay in the ledger and /auctiondetails reads them back,
            // but they are not republished: a cancelled auction is usually
            // re-run, and reprinting everyone's first bid would hand the
            // second round to whoever scrolls up.
            let shown = refresh(
                ctx.serenity_context().http.as_ref(),
                &ctx.data().auctions,
                &ctx.data().driver,
                ledger_guild,
                &auctionid,
            )
            .await;
            ctx.say(if shown {
                format!("`{auctionid}` cancelled — no winner, no DKP moved. The bids are still readable with `/auctiondetails`.")
            } else {
                format!(
                    ":warning: `{auctionid}` cancelled, but its post could not be updated — \
                     it may still show bid buttons. No winner was picked and no DKP moved."
                )
            })
            .await?;
        }
        Err(e) => {
            ctx.say(rejection_text(&e)).await?;
        }
    }
    Ok(())
}

/// Close a running auction now and settle it, skipping the wait.
#[tracing::instrument(name = "command.endauction", skip_all, err, fields(otel.kind = "server"))]
#[poise::command(
    slash_command,
    ephemeral,
    rename = "endauction",
    check = "officer_role_check"
)]
pub async fn endauction(
    ctx: Context<'_>,
    #[description = "The auction id"] auctionid: String,
) -> Result<(), Error> {
    let ledger_guild = require_guild(&ctx)?;
    crate::discord::ack_ephemeral(&ctx).await?;
    if find_auction(&ctx, &auctionid).await?.is_none() {
        return Ok(());
    }
    let now = crate::discord::chrono_now_ms();

    // Bidding stops the instant this lands, before any winner is worked out,
    // and the deadline becomes this moment so the recap names when the
    // auction actually stopped rather than when it was scheduled to.
    let closed = crate::discord::execute(
        &ctx,
        Command::CloseAuction {
            auction_id: auctionid.clone(),
            ended_ts_ms: Some(now),
        },
    )
    .await?;
    if let Err(e) = closed {
        ctx.say(rejection_text(&e)).await?;
        return Ok(());
    }

    // Same command the scheduler would have run after the grace period: same
    // winners, same revalidation against current balances, same debit.
    // Skipping the wait publishes this auction's prices while any auction
    // running beside it is still taking bids — that is the officer's call.
    let finalized = crate::discord::execute(
        &ctx,
        Command::FinalizeAuction {
            auction_id: auctionid.clone(),
            seed: now as u64,
        },
    )
    .await?;
    if let Err(e) = finalized {
        // Closed but not settled: say so plainly, because bidding has already
        // stopped and nobody has been charged.
        ctx.say(format!(
            "{} — `{auctionid}` is closed and taking no more bids.",
            rejection_text(&e)
        ))
        .await?;
        return Ok(());
    }

    let shown = refresh(
        ctx.serenity_context().http.as_ref(),
        &ctx.data().auctions,
        &ctx.data().driver,
        ledger_guild,
        &auctionid,
    )
    .await;
    let auction = find_auction(&ctx, &auctionid).await?;
    let winners = auction
        .as_ref()
        .map(|a| winners_text(&a.winners))
        .unwrap_or_else(|| "none".to_owned());
    ctx.say(if shown {
        format!("`{auctionid}` closed and settled.\nWinner/s:\n{winners}")
    } else {
        format!(
            ":warning: `{auctionid}` closed and settled, but its post could not be updated — \
             nobody was told in the channel. The DKP has already moved.\nWinner/s:\n{winners}"
        )
    })
    .await?;
    Ok(())
}

pub fn commands() -> Vec<poise::Command<crate::discord::Data, Error>> {
    vec![
        startbid(),
        startlongbid(),
        auctiondetails(),
        cancelauction(),
        endauction(),
    ]
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
    let result = interaction
        .create_response(
            ctx,
            // Legacy `i.deferUpdate()`: acknowledge without showing anything.
            // Ephemeral follow-ups are sent only when there is something to
            // say (a refusal, a DM failure).
            serenity::CreateInteractionResponse::Acknowledge,
        )
        .await
        .context("deferring component interaction");
    // The bid-storm hot path: every click on a live auction embed lands here,
    // and it shares the slash commands' 3-second deadline.
    crate::discord::record_component_ack(interaction.id.get());
    result
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

/// Ask for the amount in a modal.
///
/// This replaces the DM prompt the legacy bot used, and closes the bug that
/// made it worth replacing upstream: a `MessageCollector` filters only on the
/// DM channel, so every prompt a bidder had open collected the *same* reply —
/// one number typed with two auctions running was registered as a bid on
/// both. A modal's value arrives on its own interaction, which belongs to one
/// auction and one button, so concurrent auctions cannot be confused.
///
/// It also removes the bidder with closed DMs as a special case: there is no
/// DM channel to fail to open.
async fn open_bid_modal(
    ctx: &serenity::Context,
    interaction: &serenity::ComponentInteraction,
    auction_id: &str,
    for_main: bool,
    character: Option<&crate::loot_fit::Candidate>,
) -> anyhow::Result<()> {
    let side = if for_main { "Main bid" } else { "Alt bid" };
    // With a character: the title names it (45 chars max) and the field's
    // placeholder carries the upgrade line (100 max) — the one place a modal
    // can show text.
    let (title, placeholder) = match character {
        Some(c) => (
            clip(format!("{side} · {}", c.name), 45),
            clip(format!("DKP · {}", c.upgrade), 100),
        ),
        None => (
            "Place a bid".to_owned(),
            "Whole number of DKP — 0 withdraws your bid".to_owned(),
        ),
    };
    let input = serenity::CreateInputText::new(serenity::InputTextStyle::Short, side, BID_INPUT_ID)
        .placeholder(placeholder)
        .required(true)
        .max_length(12);
    interaction
        .create_response(
            ctx,
            serenity::CreateInteractionResponse::Modal(
                serenity::CreateModal::new(
                    bid_custom_id(for_main, auction_id, character.map(|c| c.name.as_str())),
                    title,
                )
                .components(vec![serenity::CreateActionRow::InputText(input)]),
            ),
        )
        .await
        .context("opening the bid modal")?;
    crate::discord::record_component_ack(interaction.id.get());
    Ok(())
}

fn clip(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        return s;
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// What the picker knows about one member's side of one auction.
pub struct Pick {
    pub enabled: bool,
    pub item_name: String,
    /// "WAR CLR PAL" / "ERU HIE" as the item window prints them; `None`
    /// without a row, and the race line also when every race may.
    pub class_line: Option<String>,
    pub race_line: Option<String>,
    pub candidates: Vec<crate::loot_fit::Candidate>,
    pub excluded: Vec<crate::loot_fit::Excluded>,
}

/// Resolve a member's eligible characters for one side of an auction. Reads
/// only: the ledger projection, the item mirror's disk cache (the row was
/// fetched when the auction opened) and the site's last snapshot for
/// profiles — nothing here waits on the network, because the click has
/// three seconds and a modal cannot follow a defer.
pub async fn pick(
    data: &Data,
    ledger_guild: GuildId,
    auction_id: &str,
    player: PlayerId,
    for_main: bool,
) -> Pick {
    let aid = auction_id.to_owned();
    let (enabled, item_id, item_name, chars): (
        bool,
        String,
        String,
        Vec<nocturnal_core::RosterCharacter>,
    ) = data
        .driver
        .query(move |l| {
            let g = l.state().guild(ledger_guild);
            let item = g.and_then(|g| g.auctions.get(&aid)).map(|a| a.item.clone());
            (
                g.is_some_and(|g| g.config.character_bids),
                item.as_ref().map(|i| i.id.clone()).unwrap_or_default(),
                item.map(|i| i.name)
                    .unwrap_or_else(|| "the item".to_owned()),
                g.map(|g| {
                    g.bid_characters(player, for_main)
                        .into_iter()
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
            )
        })
        .await;
    if !enabled {
        return Pick {
            enabled,
            item_name,
            class_line: None,
            race_line: None,
            candidates: Vec::new(),
            excluded: Vec::new(),
        };
    }
    let item = item_id
        .parse::<i64>()
        .ok()
        .and_then(|id| data.item_mirror.cached(id))
        .map(|row| crate::items::ItemSummary::from_row(&row));
    let snapshot = data.site.read().ok().and_then(|s| s.clone());
    let empty_profiles = Default::default();
    let empty_gear = Default::default();
    let (profiles, gear) = match &snapshot {
        Some(s) => (&s.profiles, &s.gear_items),
        None => (&empty_profiles, &empty_gear),
    };
    let fit = crate::loot_fit::Fit {
        item: item.as_ref(),
        profiles,
        gear,
    };
    let refs: Vec<&nocturnal_core::RosterCharacter> = chars.iter().collect();
    let (candidates, excluded) = fit.split(&refs);
    Pick {
        enabled,
        item_name,
        class_line: item.as_ref().map(crate::loot_fit::class_line),
        race_line: item
            .as_ref()
            .map(crate::loot_fit::race_line)
            .filter(|r| r != "ALL"),
        candidates,
        excluded,
    }
}

/// A Main/Alt click with character bids on: straight to the modal for one
/// eligible character, a select for several, a refusal for none.
async fn character_bid_click(
    ctx: &serenity::Context,
    interaction: &serenity::ComponentInteraction,
    data: &Data,
    ledger_guild: GuildId,
    auction_id: &str,
    for_main: bool,
) -> anyhow::Result<()> {
    let p = pick(
        data,
        ledger_guild,
        auction_id,
        interaction.user.id.get(),
        for_main,
    )
    .await;
    if !p.enabled {
        return open_bid_modal(ctx, interaction, auction_id, for_main, None).await;
    }
    let side = if for_main { "main" } else { "other characters" };
    match p.candidates.len() {
        0 => {
            let mut text = format!(
                ":no_entry: None of your {side} can use **{}**{}{}.",
                p.item_name,
                p.class_line
                    .as_deref()
                    .map(|c| format!(" (Class: {c}"))
                    .unwrap_or_default(),
                match (&p.class_line, &p.race_line) {
                    (Some(_), Some(r)) => format!(" · Race: {r})"),
                    (Some(_), None) => ")".to_owned(),
                    _ => String::new(),
                }
            );
            if !p.excluded.is_empty() {
                text.push_str("\nNot eligible: ");
                text.push_str(
                    &p.excluded
                        .iter()
                        .map(|e| format!("{} ({})", e.name, e.class))
                        .collect::<Vec<_>>()
                        .join(", "),
                );
            }
            text.push_str(if for_main {
                "\nYour main is the character an officer ranked with `/roster rank`; other characters bid with **Alt bid**."
            } else {
                "\nA character not on your row: `/roster add`."
            });
            ephemeral_response(ctx, interaction, text, Vec::new()).await
        }
        1 => open_bid_modal(ctx, interaction, auction_id, for_main, p.candidates.first()).await,
        _ => {
            let action = if for_main {
                Action::PickMain
            } else {
                Action::PickAlt
            };
            let options = p
                .candidates
                .iter()
                .take(25)
                .map(|c| {
                    serenity::CreateSelectMenuOption::new(
                        clip(format!("{} · {} {}", c.name, c.class, c.level), 100),
                        c.name.clone(),
                    )
                    .description(clip(c.upgrade.clone(), 100))
                })
                .collect();
            let menu = serenity::CreateSelectMenu::new(
                custom_id(action, auction_id),
                serenity::CreateSelectMenuKind::String { options },
            )
            .placeholder("Which character is this bid for?");
            let text = format!(
                "Which {} is **{}** for?",
                if for_main { "main" } else { "character" },
                p.item_name
            );
            ephemeral_response(
                ctx,
                interaction,
                text,
                vec![serenity::CreateActionRow::SelectMenu(menu)],
            )
            .await
        }
    }
}

/// The chosen character from the select, then the modal — the select
/// interaction is the one the modal answers.
async fn character_pick_selected(
    ctx: &serenity::Context,
    interaction: &serenity::ComponentInteraction,
    data: &Data,
    ledger_guild: GuildId,
    auction_id: &str,
    for_main: bool,
) -> anyhow::Result<()> {
    let chosen = match &interaction.data.kind {
        serenity::ComponentInteractionDataKind::StringSelect { values } => values.first().cloned(),
        _ => None,
    };
    let Some(chosen) = chosen else {
        return ephemeral_response(
            ctx,
            interaction,
            ":no_entry: No character chosen.",
            Vec::new(),
        )
        .await;
    };
    // Recomputed rather than trusted: the select's values are the client's.
    let p = pick(
        data,
        ledger_guild,
        auction_id,
        interaction.user.id.get(),
        for_main,
    )
    .await;
    match p
        .candidates
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(&chosen))
    {
        Some(c) => open_bid_modal(ctx, interaction, auction_id, for_main, Some(c)).await,
        None => {
            ephemeral_response(
                ctx,
                interaction,
                format!(
                    ":no_entry: **{chosen}** is not one of your eligible characters for this bid."
                ),
                Vec::new(),
            )
            .await
        }
    }
}

/// An ephemeral message as the *response* to a click (not a follow-up):
/// used before any defer, on the same footing as opening a modal.
async fn ephemeral_response(
    ctx: &serenity::Context,
    interaction: &serenity::ComponentInteraction,
    text: impl Into<String>,
    components: Vec<serenity::CreateActionRow>,
) -> anyhow::Result<()> {
    interaction
        .create_response(
            ctx,
            serenity::CreateInteractionResponse::Message(
                serenity::CreateInteractionResponseMessage::new()
                    .content(text.into())
                    .components(components)
                    .ephemeral(true),
            ),
        )
        .await
        .context("character pick reply")?;
    crate::discord::record_component_ack(interaction.id.get());
    Ok(())
}

/// The text input's own id, inside the modal.
const BID_INPUT_ID: &str = "amount";

/// Pull the typed amount out of a submitted modal.
fn submitted_amount(modal: &serenity::ModalInteraction) -> Option<&str> {
    modal.data.components.iter().find_map(|row| {
        row.components.iter().find_map(|c| match c {
            serenity::ActionRowComponent::InputText(input) if input.custom_id == BID_INPUT_ID => {
                input.value.as_deref()
            }
            _ => None,
        })
    })
}

/// Decide a submitted bid. Free of serenity beyond the parse, so the rules can
/// be tested against a real ledger: returns the reply text and, when the
/// ledger changed, the auction whose embed needs re-rendering.
pub async fn resolve_modal_bid(
    data: &Data,
    ledger_guild: GuildId,
    auction_id: &str,
    player: PlayerId,
    for_main: bool,
    character: Option<&str>,
    raw: &str,
) -> (String, Option<String>) {
    let raw = raw.trim();
    // Strict: "50abc" is a typo, not a bid of 50.
    let Ok(amount) = raw.parse::<i64>() else {
        return (
            format!("`{raw}` is not a whole number — press the button again to retry."),
            None,
        );
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
            character: character.map(str::to_owned),
        }
    };
    let outcome = data
        .driver
        .execute(ledger_guild, Actor::User(player), cmd)
        .await;
    let aid = auction_id.to_owned();
    let item = data
        .driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .and_then(|g| g.auctions.get(&aid).map(|a| a.item.name.clone()))
        })
        .await
        .unwrap_or_else(|| "the item".to_owned());
    // Name the item and the side: a bidder with two auctions open needs to
    // see which one this answered.
    let text = match &outcome {
        Ok(_) if amount == 0 => format!("Bid withdrawn from **{item}**"),
        Ok(_) => format!(
            "Bid **{amount}** as {}{} on **{item}**",
            if for_main { "MAIN" } else { "ALT" },
            character.map(|c| format!(" ({c})")).unwrap_or_default()
        ),
        Err(e) => rejection_text(e),
    };
    tracing::info!(
        { attr::NOCTURNAL_PLAYER_ID } = player,
        { attr::NOCTURNAL_AUCTION_ID } = auction_id,
        { attr::NOCTURNAL_BID_AMOUNT } = amount,
        { attr::NOCTURNAL_BID_ACCEPTED } = outcome.is_ok(),
        "modal bid resolved"
    );
    (text, outcome.is_ok().then(|| auction_id.to_owned()))
}

/// A submitted bid modal.
#[tracing::instrument(
    name = "modal.bid",
    skip_all,
    fields(otel.kind = "server", nocturnal.auction.id = tracing::field::Empty)
)]
pub async fn handle_modal(
    ctx: &serenity::Context,
    modal: &serenity::ModalInteraction,
    data: &Data,
) -> anyhow::Result<()> {
    let Some((action, auction_id, character)) = parse_custom_id(&modal.data.custom_id) else {
        return Ok(()); // not ours
    };
    tracing::Span::current().record("nocturnal.auction.id", auction_id);
    // Defer first, exactly as for a click: everything below borrows time we
    // no longer owe Discord.
    modal
        .create_response(
            ctx,
            serenity::CreateInteractionResponse::Defer(
                serenity::CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await
        .context("deferring bid modal")?;
    crate::discord::record_component_ack(modal.id.get());

    let Some(discord_guild) = modal.guild_id.map(|g| g.get()) else {
        return Ok(());
    };
    let ledger_guild = match data.data_guild {
        Some((from, to)) if from == discord_guild => to,
        _ => discord_guild,
    };
    let Some(raw) = submitted_amount(modal) else {
        return modal_reply(ctx, modal, ":no_entry: No amount was submitted.").await;
    };
    let (text, refresh_id) = resolve_modal_bid(
        data,
        ledger_guild,
        auction_id,
        modal.user.id.get(),
        action == Action::Bid,
        character,
        raw,
    )
    .await;
    modal_reply(ctx, modal, text).await?;
    if let Some(auction_id) = refresh_id {
        refresh(
            ctx.http.as_ref(),
            &data.auctions,
            &data.driver,
            ledger_guild,
            &auction_id,
        )
        .await;
    }
    Ok(())
}

async fn modal_reply(
    ctx: &serenity::Context,
    modal: &serenity::ModalInteraction,
    text: impl Into<String>,
) -> anyhow::Result<()> {
    modal
        .create_followup(
            ctx,
            serenity::CreateInteractionResponseFollowup::new()
                .content(text.into())
                .ephemeral(true),
        )
        .await
        .context("bid modal follow-up")?;
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
    let Some((action, auction_id, _)) = parse_custom_id(&interaction.data.custom_id) else {
        return Ok(()); // not ours (item pickers, pagination, …)
    };
    tracing::Span::current().record("nocturnal.auction.id", auction_id);
    let Some(discord_guild) = interaction.guild_id.map(|g| g.get()) else {
        return Ok(());
    };
    let ledger_guild = match data.data_guild {
        Some((from, to)) if from == discord_guild => to,
        _ => discord_guild,
    };
    // A modal *is* the response to the click, so it cannot follow an
    // acknowledge — these branches answer before the defer-first rule
    // applies. With character bids off that is one API call; with it on, a
    // ledger query and a disk read, still well inside the 3-second window.
    // Every check the bid needs happens on submit, where the ledger decides.
    if matches!(action, Action::Bid | Action::BidAlt) {
        return character_bid_click(
            ctx,
            interaction,
            data,
            ledger_guild,
            auction_id,
            action.for_main(),
        )
        .await;
    }
    if matches!(action, Action::PickMain | Action::PickAlt) {
        return character_pick_selected(
            ctx,
            interaction,
            data,
            ledger_guild,
            auction_id,
            action.for_main(),
        )
        .await;
    }
    // Defer-first: nothing below this line races the 3-second window.
    ack(ctx, interaction).await?;

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
        // Answered above: a bid opens a modal instead of being acknowledged.
        Action::Bid | Action::BidAlt | Action::PickMain | Action::PickAlt => Ok(()),
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
    use super::{
        closed_message, custom_id, details_readable, live_message, parse_custom_id, Action,
        AuctionStatus, Flavor,
    };

    /// The one status that must stay sealed, and the three that must not.
    /// A running auction's bids are worth an item to whoever reads them.
    #[test]
    fn only_a_running_auction_keeps_its_bids_sealed() {
        assert!(!details_readable(AuctionStatus::Open), "still taking bids");
        for status in [
            AuctionStatus::Closed,
            AuctionStatus::Finalized,
            AuctionStatus::Cancelled,
        ] {
            assert!(details_readable(status), "{status:?} is settled");
        }
    }

    #[test]
    fn custom_ids_round_trip() {
        for action in [Action::Bid, Action::BidAlt, Action::Cancel, Action::Confirm] {
            let id = custom_id(action, "au-1234abcd");
            assert!(id.len() <= 100, "Discord custom_id limit");
            assert_eq!(parse_custom_id(&id), Some((action, "au-1234abcd", None)));
        }
    }

    fn sample_auction(flavor: Flavor) -> nocturnal_core::state::Auction {
        nocturnal_core::state::Auction {
            item: nocturnal_core::Item {
                id: "1".into(),
                name: "Cloak".into(),
                url: None,
                data: None,
                image: None,
            },
            flavor,
            min_bid: 5,
            num_items: 1,
            min_bid_to_lock_for_main: 0,
            over_bid_to_win_main: 0,
            deadline_ts_ms: 1_700_000_000_000,
            status: nocturnal_core::state::AuctionStatus::Open,
            bids: Vec::new(),
            winners: Vec::new(),
            cancelled_by: None,
            cancelled_ts_ms: None,
        }
    }

    /// A live short auction MUST carry its three buttons — without them there
    /// is no way to bid at all.
    #[test]
    fn live_short_auction_has_bid_buttons() {
        let (content, _, rows) = live_message("au-1", &sample_auction(Flavor::Short));
        assert!(content.contains("**5 DKP** minimum bid"), "{content}");
        let json = serde_json::to_value(&rows).expect("rows serialize");
        let ids: Vec<String> = json[0]["components"]
            .as_array()
            .expect("button row")
            .iter()
            .map(|b| b["custom_id"].as_str().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(
            ids,
            vec!["nb:bid:au-1", "nb:alt:au-1", "nb:cancel:au-1"],
            "live short auction must offer bid / alt / cancel"
        );
    }

    /// Long auctions are bid on with the same two buttons as short ones. They
    /// survive a restart because the auction id is in the custom id and the
    /// auction is in the ledger — no listener has to stay alive for 48 hours.
    #[test]
    fn live_long_auction_offers_the_bid_buttons() {
        let (_, _, rows) = live_message("au-2", &sample_auction(Flavor::Long));
        let json = serde_json::to_value(&rows).expect("rows serialize");
        let ids: Vec<String> = json[0]["components"]
            .as_array()
            .expect("button row")
            .iter()
            .map(|b| b["custom_id"].as_str().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(
            ids,
            vec!["nb:bid:au-2", "nb:alt:au-2"],
            "main and alt, and no Cancel — a long auction is pulled with /cancelauction"
        );
    }

    /// A closed auction with proposed winners offers exactly one Confirm.
    #[test]
    fn closed_auction_offers_confirm() {
        let mut auction = sample_auction(Flavor::Short);
        auction.status = nocturnal_core::state::AuctionStatus::Closed;
        let winners = vec![nocturnal_core::event::Winner {
            player: 7,
            amount: 12,
            for_main: true,
            character: None,
        }];
        let (_, rows) = closed_message("au-3", &auction, &winners);
        let json = serde_json::to_value(&rows).expect("rows serialize");
        assert_eq!(json[0]["components"][0]["custom_id"], "nb:confirm:au-3");
        // …and none when there is nothing to confirm.
        let (_, rows) = closed_message("au-3", &auction, &[]);
        assert!(rows.is_empty());
    }

    /// End to end over a real ledger and WAL, minus Discord: the same
    /// function the modal submission calls. This is the path a bidder's typed
    /// amount actually travels, and the one that used to hang on a per-click
    /// DM collector.
    #[tokio::test]
    async fn a_submitted_amount_places_a_real_bid() {
        use super::resolve_modal_bid;
        use crate::discord::Data;
        use nocturnal_core::event::Flavor;
        use nocturnal_core::{Actor, Command, Item};

        const GUILD: u64 = 42;
        const PLAYER: u64 = 7;

        let dir = tempfile::tempdir().expect("tempdir");
        let (driver, _) = crate::driver::start(dir.path()).expect("driver");
        let data = Data {
            driver: driver.clone(),
            bell: crate::config::BellConfig::default(),
            auctions: std::sync::Arc::new(super::AuctionUi::default()),
            data_guild: None,
            items: std::sync::Arc::new(crate::items::ItemSearch::new().expect("item search")),
            provisioning: None,
            roster_access_labels: Vec::new(),
            roster_output: None,
            ourios: None,
            item_mirror: std::sync::Arc::new(crate::items::ItemMirror::new(std::path::Path::new(
                "/nonexistent",
            ))),
            site: Default::default(),
            members: Default::default(),
            feedback: None,
            prometheus_query_url: None,
            raid_bosses_path: None,
        };

        driver
            .execute(
                GUILD,
                Actor::System,
                Command::ImportPlayer {
                    player: PLAYER,
                    balance: 100,
                    characters: vec![],
                    creation_ts_ms: 1,
                    log: vec![],
                    legacy_id: None,
                },
            )
            .await
            .expect("import");
        driver
            .execute(
                GUILD,
                Actor::System,
                Command::OpenAuction {
                    auction_id: "au-1".into(),
                    item: Item {
                        id: "1".into(),
                        name: "Cloak".into(),
                        url: None,
                        data: None,
                        image: None,
                    },
                    flavor: Flavor::Short,
                    min_bid: 0,
                    num_items: 1,
                    min_bid_to_lock_for_main: 0,
                    over_bid_to_win_main: 0,
                    duration_ms: 600_000,
                },
            )
            .await
            .expect("open");

        let (text, refresh) =
            resolve_modal_bid(&data, GUILD, "au-1", PLAYER, true, None, " 40 ").await;
        assert!(text.contains("40"), "{text}");
        assert!(text.contains("Cloak"), "the item is named: {text}");
        assert_eq!(refresh.as_deref(), Some("au-1"));
        let bids = driver
            .query(|l| {
                l.state()
                    .guild(GUILD)
                    .map(|g| g.auctions["au-1"].bids.clone())
                    .unwrap_or_default()
            })
            .await;
        assert_eq!(bids.len(), 1);
        assert_eq!(bids[0].amount, 40);

        // Re-bidding replaces rather than stacking.
        resolve_modal_bid(&data, GUILD, "au-1", PLAYER, true, None, "55").await;
        let bids = driver
            .query(|l| {
                l.state()
                    .guild(GUILD)
                    .map(|g| g.auctions["au-1"].bids.clone())
                    .unwrap_or_default()
            })
            .await;
        assert_eq!(bids.len(), 1, "the second bid replaced the first");
        assert_eq!(bids[0].amount, 55);

        // 0 withdraws.
        let (text, refresh) =
            resolve_modal_bid(&data, GUILD, "au-1", PLAYER, true, None, "0").await;
        assert!(text.contains("withdrawn"), "{text}");
        assert_eq!(refresh.as_deref(), Some("au-1"));
        let bids = driver
            .query(|l| {
                l.state()
                    .guild(GUILD)
                    .map(|g| g.auctions["au-1"].bids.clone())
                    .unwrap_or_default()
            })
            .await;
        assert!(bids.is_empty());

        // Anything that is not a whole number is a typo, not a bid.
        for raw in ["50abc", "", "twelve", "3.5"] {
            let (text, refresh) =
                resolve_modal_bid(&data, GUILD, "au-1", PLAYER, true, None, raw).await;
            assert!(refresh.is_none(), "{raw} changed the ledger");
            assert!(text.contains("not a whole number"), "{raw}: {text}");
        }

        // More than the bidder has is refused by the ledger, not by the modal.
        let (text, refresh) =
            resolve_modal_bid(&data, GUILD, "au-1", PLAYER, true, None, "500").await;
        assert!(refresh.is_none());
        assert!(text.contains("greater than your current DKP"), "{text}");
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
