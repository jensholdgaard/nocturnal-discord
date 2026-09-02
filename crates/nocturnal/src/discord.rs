//! Discord layer (M3, read-only commands): serenity/poise, defer-first,
//! errors contained, command registration scoped to the test guild.
//! Embed formats port the legacy bot's output so officers see what they know.

use nocturnal_telemetry::attr;
use std::time::Duration;

use tracing::Instrument as _;

use anyhow::Context as _;
use poise::serenity_prelude as serenity;

use nocturnal_core::state::LogEntry;
use nocturnal_core::PlayerId;

use crate::config::Config;
use crate::driver::DriverHandle;
use crate::health::Readiness;

pub struct Data {
    pub driver: DriverHandle,
    pub bell: crate::config::BellConfig,
    pub auctions: std::sync::Arc<crate::auctions::AuctionUi>,
    /// Test-server mapping: serve this ledger guild for interactions from the
    /// registration guild (see `discord.data_guild_id`).
    pub data_guild: Option<(u64, u64)>,
    pub items: std::sync::Arc<crate::items::ItemSearch>,
    /// Telemetry provisioning paths; `None` disables /dpstoken and /dpsrevoke.
    pub provisioning: Option<crate::provision::Provisioning>,
    /// Raid-access labels `/roster` accepts (config).
    pub roster_access_labels: Vec<String>,
    /// Where the roster page payload is written; `None` = not materialized.
    pub roster_output: Option<std::path::PathBuf>,
    /// Ourios query endpoint and tenant for character profiles; `None` = none.
    pub ourios: Option<(String, String)>,
    /// The item mirror (pqdi rows cached on disk), for profiles' gear.
    pub item_mirror: std::sync::Arc<crate::items::ItemMirror>,
    /// The page server's live snapshot, replaced on every render.
    pub site: crate::site::SiteHandle,
    /// The feedback-channel mirror, when configured.
    pub feedback: Option<std::sync::Arc<crate::feedback::Feedback>>,
    /// Discord display names and roles by player id, filled lazily by the
    /// page materializer. Presentation state: lost on restart, refilled.
    pub members: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<u64, crate::roster_page::MemberInfo>>,
    >,
}

pub type Error = anyhow::Error;
pub type Context<'a> = poise::Context<'a, Data, Error>;

const EMBED_BLUE: u32 = 0x0099ff;

pub fn ts_sec(ms: i64) -> i64 {
    ms / 1000
}

/// The ledger guild for this interaction: normally the Discord guild itself,
/// remapped when a test server serves imported production data.
pub fn require_guild(ctx: &Context<'_>) -> anyhow::Result<u64> {
    let guild = ctx
        .guild_id()
        .map(|g| g.get())
        .context("This command can only be used in a discord server")?;
    Ok(match ctx.data().data_guild {
        Some((from, to)) if from == guild => to,
        _ => guild,
    })
}

/// Shows the DKP of a player.
#[poise::command(slash_command, ephemeral)]
#[tracing::instrument(name = "command.playerdkp", skip_all, fields(otel.kind = "server"))]
pub async fn playerdkp(
    ctx: Context<'_>,
    #[description = "The player"] player: Option<serenity::User>,
) -> Result<(), Error> {
    let guild = require_guild(&ctx)?;
    crate::discord::ack_ephemeral(&ctx).await?;
    let target: PlayerId = player.map_or(ctx.author().id.get(), |u| u.id.get());
    let balance = ctx
        .data()
        .driver
        .query(move |l| {
            l.state()
                .guild(guild)
                .and_then(|g| g.players.get(&target).map(|p| p.balance))
        })
        .await;
    match balance {
        Some(b) => ctx.say(format!("` {b} ` DKP")).await?,
        None => ctx.say("No DKP records for that player yet").await?,
    };
    Ok(())
}

/// Shows the DKP history of a player (ticks aggregated per raid, 30/page).
#[poise::command(slash_command, ephemeral)]
#[tracing::instrument(name = "command.dkphistory", skip_all, fields(otel.kind = "server"))]
pub async fn dkphistory(
    ctx: Context<'_>,
    #[description = "The player"] player: Option<serenity::User>,
) -> Result<(), Error> {
    let guild = require_guild(&ctx)?;
    crate::discord::ack_ephemeral(&ctx).await?;
    let user = player.unwrap_or_else(|| ctx.author().clone());
    let target = user.id.get();
    let log: Vec<LogEntry> = ctx
        .data()
        .driver
        .query(move |l| {
            l.state()
                .guild(guild)
                .and_then(|g| g.players.get(&target).map(|p| p.log.clone()))
                .unwrap_or_default()
        })
        .await;
    if log.is_empty() {
        ctx.say("No history for that player yet").await?;
        return Ok(());
    }
    let lines = history_lines(&log);
    let entries_per_page = 30;
    if lines.len() <= entries_per_page {
        ctx.say(lines.join("\n")).await?;
        return Ok(());
    }
    let pages: Vec<serenity::CreateEmbed> = lines
        .chunks(entries_per_page)
        .enumerate()
        .map(|(i, chunk)| {
            serenity::CreateEmbed::new()
                .title(format!("DKP History of {}", user.name))
                .description(chunk.join("\n"))
                .footer(serenity::CreateEmbedFooter::new(format!(
                    "{}/{}",
                    i + 1,
                    lines.len().div_ceil(entries_per_page)
                )))
        })
        .collect();
    paginate(ctx, pages).await
}

/// Legacy history rendering: newest first, consecutive ticks for the same
/// raid collapse into one "N aggregated ticks" line.
fn history_lines(log: &[LogEntry]) -> Vec<String> {
    let mut entries: Vec<&LogEntry> = log.iter().collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.ts_ms));
    let mut lines = Vec::new();
    let mut ticks = 0u32;
    for (i, e) in entries.iter().enumerate() {
        let raid_name = e
            .raid
            .as_ref()
            .map_or(" ".to_owned(), |r| format!(" *{}* ", r.name));
        if e.comment == "Tick" {
            ticks += 1;
            let next_is_same_raid_tick = entries.get(i + 1).is_some_and(|n| {
                n.comment == "Tick"
                    && n.raid.as_ref().map(|r| &r.raid_id) == e.raid.as_ref().map(|r| &r.raid_id)
            });
            if next_is_same_raid_tick {
                continue;
            }
            lines.push(format!(
                "- <t:{}:d>  **{}**{}*aggregated ticks*",
                ts_sec(e.ts_ms),
                ticks,
                raid_name
            ));
            ticks = 0;
        } else if let Some(item) = &e.item {
            lines.push(format!(
                "- <t:{}:d>  **{}**{}{}",
                ts_sec(e.ts_ms),
                e.dkp,
                raid_name,
                item.name
            ));
        } else {
            lines.push(format!(
                "- <t:{}:d>  **{}**{}*{}*",
                ts_sec(e.ts_ms),
                e.dkp,
                raid_name,
                e.comment
            ));
        }
    }
    lines
}

/// List all players and their current DKP (10/page; refused during a raid).
#[poise::command(slash_command, rename = "listplayersdkps", ephemeral)]
#[tracing::instrument(name = "command.listplayersdkps", skip_all, fields(otel.kind = "server"))]
pub async fn listplayersdkps(ctx: Context<'_>) -> Result<(), Error> {
    let guild = require_guild(&ctx)?;
    crate::discord::ack_ephemeral(&ctx).await?;
    let caller = ctx.author().id.get();
    let now_ms = chrono_now_ms();

    struct Row {
        player: PlayerId,
        current: i64,
        attendance: f64,
    }
    struct Listing {
        raid_active: bool,
        rows: Vec<Row>,
        caller_row: Option<(usize, Row)>,
    }

    let listing = ctx
        .data()
        .driver
        .query(move |l| {
            let Some(g) = l.state().guild(guild) else {
                return Listing {
                    raid_active: false,
                    rows: Vec::new(),
                    caller_row: None,
                };
            };
            if g.active_raid.is_some() {
                return Listing {
                    raid_active: true,
                    rows: Vec::new(),
                    caller_row: None,
                };
            }
            let mut rows: Vec<Row> = g
                .raiding_players(now_ms)
                .map(|(id, p)| Row {
                    player: id,
                    current: p.balance,
                    attendance: g.attendance_pct(id, now_ms),
                })
                .collect();
            rows.sort_by_key(|r| std::cmp::Reverse(r.current));
            let caller_row = rows.iter().position(|r| r.player == caller).map(|i| {
                (
                    i,
                    Row {
                        player: caller,
                        current: rows[i].current,
                        attendance: rows[i].attendance,
                    },
                )
            });
            Listing {
                raid_active: false,
                rows,
                caller_row,
            }
        })
        .await;

    if listing.raid_active {
        ctx.say(":no_entry: DKP Bot scowls at you. This command is forbidden during raids.")
            .await?;
        return Ok(());
    }
    if listing.rows.is_empty() {
        ctx.say(":no_entry: No players found").await?;
        return Ok(());
    }

    let page_size = 10;
    let total_pages = listing.rows.len().div_ceil(page_size);
    let (caller_pos, caller_line) = match &listing.caller_row {
        Some((i, r)) => (
            format!("| `{:>2}`: <@{}>", i + 1, r.player),
            format!("| ` {:>6} ` |     `{:>5}%`     |", r.current, r.attendance),
        ),
        None => (String::new(), String::new()),
    };
    let sep1 = "\n-----------------------------------------\n";
    let sep2 = "\n--------------------------\n";
    let pages: Vec<serenity::CreateEmbed> = listing
        .rows
        .chunks(page_size)
        .enumerate()
        .map(|(page, chunk)| {
            let names: Vec<String> = chunk
                .iter()
                .enumerate()
                .map(|(i, r)| format!("| `{:>2}`: <@{}>", page * page_size + i + 1, r.player))
                .collect();
            let data: Vec<String> = chunk
                .iter()
                .map(|r| format!("| ` {:>6} ` |     `{:>5}%`     |", r.current, r.attendance))
                .collect();
            serenity::CreateEmbed::new()
                .color(EMBED_BLUE)
                .author(serenity::CreateEmbedAuthor::new(format!("{}/{total_pages}", page + 1)))
                .field(
                    "\u{200b}",
                    format!(
                        "| # | **Player Name**{sep1}{}{sep1}{sep1}{caller_pos}{sep1}",
                        names.join(sep1)
                    ),
                    true,
                )
                .field(
                    "\u{200b}",
                    format!(
                        "|      **DKP**      | **Attendance** |{sep2}{}{sep2}{sep2}{caller_line}{sep2}",
                        data.join(sep2)
                    ),
                    true,
                )
        })
        .collect();
    if pages.len() == 1 {
        ctx.send(
            poise::CreateReply::default()
                .embed(pages[0].clone())
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }
    paginate(ctx, pages).await
}

/// Search the ledger's comments (literal text — audit E6; 20/page).
#[poise::command(slash_command, rename = "searchlogs", ephemeral)]
#[tracing::instrument(name = "command.searchlogs", skip_all, fields(otel.kind = "server"))]
pub async fn searchlogs(
    ctx: Context<'_>,
    #[description = "Search term"] search: String,
) -> Result<(), Error> {
    let guild = require_guild(&ctx)?;
    crate::discord::ack_ephemeral(&ctx).await?;
    if search.to_lowercase().contains("tick") {
        ctx.say("DKP - bot scowls at you. What do you want your tombstone to say?")
            .await?;
        return Ok(());
    }
    let needle = search.to_lowercase();
    let hits: Vec<(PlayerId, LogEntry)> = ctx
        .data()
        .driver
        .query(move |l| {
            let mut hits: Vec<(PlayerId, LogEntry)> = l
                .state()
                .guild(guild)
                .map(|g| {
                    g.players
                        .iter()
                        .flat_map(|(id, p)| {
                            p.log
                                .iter()
                                .filter(|e| e.comment.to_lowercase().contains(&needle))
                                .map(|e| (*id, e.clone()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            hits.sort_by_key(|(_, e)| e.ts_ms);
            hits
        })
        .await;
    if hits.is_empty() {
        ctx.say("No logs found").await?;
        return Ok(());
    }
    let lines: Vec<String> = hits
        .iter()
        .map(|(player, e)| {
            let what = match &e.item {
                Some(item) => match &item.url {
                    Some(url) => format!("[{}]({url})", item.name),
                    None => item.name.clone(),
                },
                None => format!("*{}*", e.comment),
            };
            format!(
                "- <t:{}:d>  **{}** {what} <@{player}>",
                ts_sec(e.ts_ms),
                e.dkp
            )
        })
        .collect();
    let per_page = 20;
    let total = lines.len().div_ceil(per_page);
    let pages: Vec<serenity::CreateEmbed> = lines
        .chunks(per_page)
        .enumerate()
        .map(|(i, chunk)| {
            serenity::CreateEmbed::new()
                .color(EMBED_BLUE)
                .title(format!("Logs for: {search} ({} results)", lines.len()))
                .description(chunk.join("\n"))
                .footer(serenity::CreateEmbedFooter::new(format!(
                    "{}/{total}",
                    i + 1
                )))
        })
        .collect();
    paginate(ctx, pages).await
}

/// The one pagination helper (legacy had three diverging copies — audit S12).
async fn paginate(ctx: Context<'_>, pages: Vec<serenity::CreateEmbed>) -> Result<(), Error> {
    let ctx_id = ctx.id();
    let prev_id = format!("{ctx_id}prev");
    let next_id = format!("{ctx_id}next");
    let buttons = |page: usize, disabled_all: bool| {
        serenity::CreateActionRow::Buttons(vec![
            serenity::CreateButton::new(&prev_id)
                .label("Previous Page")
                .style(serenity::ButtonStyle::Primary)
                .disabled(disabled_all || page == 0),
            serenity::CreateButton::new(&next_id)
                .label("Next Page")
                .style(serenity::ButtonStyle::Primary)
                .disabled(disabled_all || page + 1 == pages.len()),
        ])
    };
    let mut page = 0usize;
    let msg = ctx
        .send(
            poise::CreateReply::default()
                .embed(pages[0].clone())
                .components(vec![buttons(0, false)])
                .ephemeral(true),
        )
        .await?;
    while let Some(press) = serenity::collector::ComponentInteractionCollector::new(ctx)
        .filter(move |press| press.data.custom_id.starts_with(&ctx_id.to_string()))
        .timeout(Duration::from_secs(120))
        .await
    {
        if press.data.custom_id == next_id {
            page = (page + 1).min(pages.len() - 1);
        } else if press.data.custom_id == prev_id {
            page = page.saturating_sub(1);
        } else {
            continue;
        }
        press
            .create_response(
                ctx.serenity_context(),
                serenity::CreateInteractionResponse::UpdateMessage(
                    serenity::CreateInteractionResponseMessage::new()
                        .embed(pages[page].clone())
                        .components(vec![buttons(page, false)]),
                ),
            )
            .await?;
    }
    // Collector expired: disable the buttons (legacy behaviour).
    msg.edit(
        ctx,
        poise::CreateReply::default()
            .embed(pages[page].clone())
            .components(vec![buttons(page, true)]),
    )
    .await?;
    Ok(())
}

pub fn chrono_now_ms() -> i64 {
    #[allow(clippy::expect_used)]
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after 1970");
    d.as_millis() as i64
}

/// Discord's epoch, 2015-01-01T00:00:00Z, in milliseconds.
const DISCORD_EPOCH_MS: i64 = 1_420_070_400_000;

/// The instant Discord *created* an interaction, decoded from its snowflake.
///
/// This is the baseline that matters: the three-second acknowledgment
/// deadline runs from creation, not from the moment the event reaches us, so
/// timing from a local `Instant` would silently exclude the gateway hop —
/// exactly the part that goes wrong first under load. Reading the id
/// arithmetically also keeps this independent of whether serenity was built
/// against `chrono` or `time`.
fn snowflake_ms(id: u64) -> i64 {
    (id >> 22) as i64 + DISCORD_EPOCH_MS
}

fn record_ack(created_ms: i64, kind: &'static str) {
    let elapsed = (chrono_now_ms() - created_ms) as f64 / 1000.0;
    // A clock that stepped backwards would otherwise post a negative sample
    // and poison the histogram's sum for the whole collection interval.
    if elapsed < 0.0 {
        return;
    }
    nocturnal_telemetry::metrics().ack_duration.record(
        elapsed,
        &[opentelemetry::KeyValue::new(
            nocturnal_telemetry::attr::NOCTURNAL_INTERACTION_KIND,
            kind,
        )],
    );
}

/// Acknowledge a slash command, timed against Discord's own clock.
///
/// Every command defers before doing work — the legacy bot's habit of
/// answering inline is what made the 3-second window a recurring outage — so
/// this is the one place the acknowledgment latency can be measured.
pub async fn ack(ctx: &Context<'_>) -> Result<(), Box<serenity::Error>> {
    let result = ctx.defer().await.map_err(Box::new);
    if let poise::Context::Application(app) = ctx {
        record_ack(snowflake_ms(app.interaction.id.get()), "command");
    }
    result
}

/// As [`ack`], for the ephemeral replies most commands use.
pub async fn ack_ephemeral(ctx: &Context<'_>) -> Result<(), Box<serenity::Error>> {
    let result = ctx.defer_ephemeral().await.map_err(Box::new);
    if let poise::Context::Application(app) = ctx {
        record_ack(snowflake_ms(app.interaction.id.get()), "command");
    }
    result
}

/// Acknowledge a button press. Buttons share the same 3-second deadline and
/// are the hot path during a bid storm, so they are timed too.
pub async fn ack_component(
    ctx: &serenity::Context,
    press: &serenity::ComponentInteraction,
) -> Result<(), Box<serenity::Error>> {
    let result = press.defer(ctx).await.map_err(Box::new);
    record_component_ack(press.id.get());
    result
}

/// Time a button acknowledgment that was sent some other way (the auction
/// embeds answer with `Acknowledge` rather than `defer`).
pub fn record_component_ack(interaction_id: u64) {
    record_ack(snowflake_ms(interaction_id), "button");
}

/// Gateway events we handle outside the command framework: auction buttons.
async fn on_event(
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    data: &Data,
) -> Result<(), Error> {
    // The feedback channel, mirrored into Ourios; edits re-emit under the
    // same message id, and the newest record wins on read.
    if let Some(f) = &data.feedback {
        match event {
            serenity::FullEvent::Message { new_message } if f.wants(new_message) => {
                f.record(new_message, "posted");
            }
            serenity::FullEvent::MessageUpdate { new: Some(m), .. } if f.wants(m) => {
                f.record(m, "edited");
            }
            serenity::FullEvent::MessageUpdate {
                event: upd,
                new: None,
                ..
            } if upd.channel_id == f.channel && upd.content.is_some() => {
                // No cache: fetch the edited message once.
                if let Ok(m) = f.channel.message(&ctx.http, upd.id).await {
                    if f.wants(&m) {
                        f.record(&m, "edited");
                    }
                }
            }
            _ => {}
        }
    }
    if let serenity::FullEvent::InteractionCreate { interaction } = event {
        if let Some(component) = interaction.as_message_component() {
            // Diagnostic: proves component clicks reach us at all, and shows
            // the id we were handed if dispatch ever stops matching.
            tracing::info!(
                { attr::NOCTURNAL_DISCORD_CUSTOM_ID } = %component.data.custom_id,
                { attr::NOCTURNAL_DISCORD_USER_ID } = component.user.id.get(),
                "component interaction received"
            );
            // The /dpstoken gate button first; everything else is an auction.
            let handled = match crate::provision::handle_component(ctx, component, data).await {
                Ok(handled) => handled,
                Err(e) => {
                    tracing::warn!(
                        { attr::NOCTURNAL_ERROR_MESSAGE } = format!("{e:#}"),
                        "dpstoken gate handler failed"
                    );
                    true
                }
            };
            if !handled {
                if let Err(e) = crate::auctions::handle_component(ctx, component, data).await {
                    // `{:#}` prints the whole anyhow chain — the outermost context
                    // alone hid the real cause of a failed component reply.
                    tracing::warn!(
                        { attr::NOCTURNAL_ERROR_MESSAGE } = format!("{e:#}"),
                        "auction component handler failed"
                    );
                }
            }
        }
        // The amount typed into a bid modal comes back as its own interaction.
        if let Some(modal) = interaction.as_modal_submit() {
            tracing::info!(
                { attr::NOCTURNAL_DISCORD_CUSTOM_ID } = %modal.data.custom_id,
                { attr::NOCTURNAL_DISCORD_USER_ID } = modal.user.id.get(),
                "modal submitted"
            );
            if modal.data.custom_id == crate::provision::TOKEN_MODAL_ID {
                // Submitting the token modal: acknowledge with the rest of
                // the setup — everything but the secret.
                let steps = data
                    .provisioning
                    .as_ref()
                    .map(|p| crate::provision::setup_steps(&p.dashboard_url))
                    .unwrap_or_else(|| "You're set.".to_owned());
                if let Err(e) = modal
                    .create_response(
                        ctx,
                        serenity::CreateInteractionResponse::Message(
                            serenity::CreateInteractionResponseMessage::new()
                                .content(steps)
                                .ephemeral(true),
                        ),
                    )
                    .await
                {
                    tracing::warn!(
                        { attr::NOCTURNAL_ERROR_MESSAGE } = format!("{e:#}"),
                        "token modal ack failed"
                    );
                }
            } else if let Err(e) = crate::auctions::handle_modal(ctx, modal, data).await {
                tracing::warn!(
                    { attr::NOCTURNAL_ERROR_MESSAGE } = format!("{e:#}"),
                    "bid modal handler failed"
                );
            }
        }
    }
    Ok(())
}

async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
    match error {
        poise::FrameworkError::Command { error, ctx, .. } => {
            tracing::warn!({ attr::NOCTURNAL_COMMAND } = ctx.command().name,
                { attr::NOCTURNAL_ERROR_MESSAGE } = %error,
                "command error");
            let _ = ctx.say(format!(":no_entry: {error}")).await;
        }
        other => {
            if let Err(e) = poise::builtins::on_error(other).await {
                tracing::error!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "error handler failed");
            }
        }
    }
}

/// Connect the gateway, register commands in the configured test guild, and
/// A second gateway connection, for the bot identity members already have.
///
/// The DKP commands are guild-scoped to the test server behind a `controels-`
/// prefix until cutover, but the token commands are finished, and they were the
/// only reason a separate Python bot was still running beside this one. Rather
/// than move members to a new bot — a re-invite, a new name in the member list,
/// and the rewrite's whole command surface arriving early — this process opens
/// a second connection under the existing application and registers only
/// `/dpstoken` and `/dpsrevoke` there.
///
/// Two identities, one process, one ledger: the frameworks share a
/// `DriverHandle`, so there is no second data directory and nothing to keep in
/// step. Intents are narrower than the DKP client's — this one reads member
/// roles off the interaction and *sends* DMs, so it needs neither
/// DIRECT_MESSAGES nor a voice stack.
async fn provisioning_client(
    token: String,
    guild_id: u64,
    driver: DriverHandle,
    provisioning: crate::provision::Provisioning,
) -> anyhow::Result<serenity::Client> {
    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: crate::provision::commands(),
            on_error: |error| Box::pin(async move { on_error(error).await }),
            ..Default::default()
        })
        .setup(move |ctx, ready, framework| {
            Box::pin(async move {
                poise::builtins::register_in_guild(
                    ctx,
                    &framework.options().commands,
                    serenity::GuildId::new(guild_id),
                )
                .await?;
                tracing::info!(
                    { attr::NOCTURNAL_DISCORD_USER_NAME } = %ready.user.name,
                    { attr::NOCTURNAL_GUILD_ID } = guild_id,
                    "provisioning gateway ready, commands registered"
                );
                // The derived files are healed by the DKP client's boot
                // recovery, which owns the same ledger. Doing it twice would
                // race two writers over one tokens.txt for no gain.
                Ok(Data {
                    driver,
                    bell: crate::config::BellConfig::default(),
                    auctions: std::sync::Arc::new(crate::auctions::AuctionUi::default()),
                    // This identity lives in the members' guild, which *is* the
                    // ledger guild, so there is nothing to remap.
                    data_guild: None,
                    items: std::sync::Arc::new(
                        crate::items::ItemSearch::new().expect("item search client"),
                    ),
                    provisioning: Some(provisioning),
                    roster_access_labels: Vec::new(),
                    roster_output: None,
                    ourios: None,
                    item_mirror: std::sync::Arc::new(crate::items::ItemMirror::new(
                        std::path::Path::new("/nonexistent"),
                    )),
                    site: Default::default(),
                    members: Default::default(),
                    feedback: None,
                })
            })
        })
        .build();
    serenity::ClientBuilder::new(&token, serenity::GatewayIntents::GUILDS)
        .framework(framework)
        .await
        .context("building provisioning Discord client")
}

/// run until shutdown.
pub async fn run(
    cfg: &Config,
    driver: DriverHandle,
    readiness: Readiness,
    site_handle: crate::site::SiteHandle,
) -> anyhow::Result<()> {
    let ourios: Option<(String, String)> = cfg
        .roster
        .ourios_query_url
        .clone()
        .map(|u| (u, cfg.roster.ourios_tenant.clone()));
    let item_mirror = std::sync::Arc::new(crate::items::ItemMirror::new(&cfg.data.dir));
    let token = Config::discord_token()?;
    let guild_id = cfg
        .discord
        .guild_id
        .context("discord.guild_id is required — commands register guild-scoped only (test server) while the legacy bot is alive")?;
    let mut commands = vec![
        playerdkp(),
        dkphistory(),
        listplayersdkps(),
        searchlogs(),
        configure(),
        showconfig(),
        startraid(),
        endraid(),
        mergeraid(),
        adddkp(),
        removedkp(),
        addraiddkp(),
        registercharacter(),
        addraideventdkp(),
        belltest(),
        searchitem(),
        stresstest(),
    ];
    commands.extend(crate::auctions::commands());
    commands.extend(crate::provision::commands());
    commands.extend(crate::backup::commands());
    commands.extend(crate::roster::commands());
    // A test server can share the bot application with other deployments by
    // prefixing every command name (e.g. /controels-playerdkp).
    if !cfg.discord.command_prefix.is_empty() {
        for cmd in &mut commands {
            cmd.name = format!("{}{}", cfg.discord.command_prefix, cmd.name);
        }
    }
    let options = poise::FrameworkOptions {
        commands,
        on_error: |error| Box::pin(on_error(error)),
        event_handler: |ctx, event, _framework, data| Box::pin(on_event(ctx, event, data)),
        ..Default::default()
    };
    // Cloned before the DKP framework's setup closure consumes it: the second
    // identity talks to the same ledger through the same writer.
    let provision_driver = driver.clone();
    let roster_labels = cfg.roster.access_labels.clone();
    let roster_output = cfg.roster.output_path.clone();
    let feedback = cfg
        .discord
        .feedback_channel_id
        .map(|c| std::sync::Arc::new(crate::feedback::Feedback::new(c, &cfg.data.dir)));
    let feedback_enabled = feedback.is_some();
    let members: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<u64, crate::roster_page::MemberInfo>>,
    > = Default::default();
    let auction_ui = std::sync::Arc::new(crate::auctions::AuctionUi::default());
    let bell_cfg = cfg.bell.clone();
    let provisioning = crate::provision::Provisioning::from_config(&cfg.provision);
    if provisioning.is_none() {
        tracing::info!("telemetry provisioning not configured; /dpstoken and /dpsrevoke are off");
    }
    let data_guild = cfg.discord.data_guild_id.map(|to| (guild_id, to));
    if let Some((from, to)) = data_guild {
        tracing::info!(
            { attr::NOCTURNAL_GUILD_REMAP_FROM } = from,
            { attr::NOCTURNAL_GUILD_REMAP_TO } = to,
            "serving remapped ledger guild for the test server"
        );
    }
    let framework = poise::Framework::builder()
        .options(options)
        .setup(move |ctx, ready, framework| {
            Box::pin(async move {
                poise::builtins::register_in_guild(
                    ctx,
                    &framework.options().commands,
                    serenity::GuildId::new(guild_id),
                )
                .await?;
                tracing::info!(
                    { attr::NOCTURNAL_DISCORD_USER_NAME } = %ready.user.name,
                    { attr::NOCTURNAL_GUILD_ID } = guild_id,
                    "gateway ready, commands registered"
                );
                readiness.set_ready();
                // Boot recovery: the provisioning files are derived state, so
                // re-deriving them here is what heals a half-written file, a
                // restored VM, or a manual edit gone wrong.
                if let Some(p) = &provisioning {
                    crate::provision::rematerialize(
                        &driver,
                        p,
                        data_guild.map_or(guild_id, |(_, to)| to),
                    )
                    .await;
                }
                // The roster page is derived state too: render it on boot so
                // a restart (or a fresh import) is reflected without waiting
                // for the next /roster command. Spawned, not awaited: the
                // first render resolves every member with one Discord call
                // each, which is minutes for the whole guild, and nothing
                // else in this setup should wait behind it.
                //
                // And never via `framework.user_data()` from inside setup:
                // that value exists only once setup returns, so awaiting it
                // here deadlocks the framework — commands are then never
                // dispatched while the bot looks perfectly alive. Found on
                // the live box on 2026-08-31, after 12 minutes of silence.
                if let Some(out) = roster_output.clone() {
                    let http = ctx.http.clone();
                    let driver = driver.clone();
                    let members = members.clone();
                    let ourios = ourios.clone();
                    let site_for_render = site_handle.clone();
                    let items = item_mirror.clone();
                    let ledger_guild = data_guild.map_or(guild_id, |(_, to)| to);
                    // And every half hour after: profiles arrive from
                    // members' clients through Ourios on their own schedule,
                    // and nothing else would re-render the page for them.
                    tokio::spawn(async move {
                        loop {
                            crate::roster_page::rematerialize(
                                http.as_ref(),
                                guild_id,
                                &driver,
                                &out,
                                &members,
                                ledger_guild,
                                ourios.as_ref(),
                                &items,
                                &site_for_render,
                            )
                            .await;
                            tokio::time::sleep(std::time::Duration::from_secs(30 * 60)).await;
                        }
                    });
                }
                // The feedback mirror catches up on whatever was posted while
                // the bot was away, then follows the channel live.
                if let Some(f) = feedback.clone() {
                    let http = ctx.http.clone();
                    tokio::spawn(async move { f.backfill(http.as_ref()).await });
                }
                // Boot recovery: auctions still open in the ledger get fresh
                // embeds so their buttons work again (hazard B11).
                crate::auctions::repost_open_auctions(
                    ctx.http.as_ref(),
                    &auction_ui,
                    &driver,
                    data_guild.map_or(guild_id, |(_, to)| to),
                )
                .await;
                tokio::spawn(crate::scheduler::run(crate::scheduler::Scheduler {
                    ctx: ctx.clone(),
                    driver: driver.clone(),
                    auctions: auction_ui.clone(),
                    discord_guild: guild_id,
                    ledger_guild: data_guild.map_or(guild_id, |(_, to)| to),
                }));
                Ok(Data {
                    driver,
                    bell: bell_cfg,
                    auctions: auction_ui,
                    data_guild,
                    items: std::sync::Arc::new(
                        crate::items::ItemSearch::new().expect("item search client"),
                    ),
                    provisioning,
                    roster_access_labels: roster_labels.clone(),
                    roster_output: roster_output.clone(),
                    ourios: ourios.clone(),
                    item_mirror: item_mirror.clone(),
                    site: site_handle.clone(),
                    members: members.clone(),
                    feedback: feedback.clone(),
                })
            })
        })
        .build();
    // Discord HTTP client with rate-limit visibility: serenity calls this
    // back whenever it *delays* a request to respect a bucket — the early
    // warning that fires before Discord would 429 us.
    let metrics = nocturnal_telemetry::Metrics::new();
    let rl_metrics = std::sync::Arc::new(metrics);
    let cb_metrics = rl_metrics.clone();
    let mut http = serenity::HttpBuilder::new(&token).build();
    if let Some(ratelimiter) = http.ratelimiter.as_mut() {
        ratelimiter.set_ratelimit_callback(Box::new(move |info| {
            tracing::warn!(
                { attr::URL_PATH } = ?info.path,
                { attr::HTTP_REQUEST_METHOD } = ?info.method,
                { attr::NOCTURNAL_DISCORD_RATELIMIT_DELAY } = info.timeout.as_secs_f64(),
                { attr::NOCTURNAL_DISCORD_RATELIMIT_GLOBAL } = info.global,
                "discord request delayed by rate limiter"
            );
            let attrs = [opentelemetry::KeyValue::new(
                nocturnal_telemetry::attr::NOCTURNAL_DISCORD_RATELIMIT_GLOBAL,
                info.global,
            )];
            cb_metrics.ratelimit_delays.add(1, &attrs);
            cb_metrics
                .ratelimit_delay_duration
                .record(info.timeout.as_secs_f64(), &attrs);
        }));
    }
    use songbird::serenity::SerenityInit as _;
    let mut client = serenity::ClientBuilder::new_with_http(
        http,
        // Exactly what the bot needs, spelled out (see feedback.rs): the
        // privileged MESSAGE_CONTENT intent is requested only when a
        // feedback channel is configured. DMs with the app are exempt from
        // it, so bids read regardless.
        crate::feedback::gateway_intents(feedback_enabled),
    )
    .framework(framework)
    .register_songbird()
    .await
    .context("building Discord client")?;

    // Gateway heartbeat latency, sampled every 30 s.
    let latency_shards = client.shard_manager.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let runners = latency_shards.runners.lock().await;
            if let Some(info) = runners.values().next() {
                if let Some(latency) = info.latency {
                    rl_metrics
                        .gateway_latency
                        .record(latency.as_secs_f64(), &[]);
                }
            }
        }
    });

    let shard_manager = client.shard_manager.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("shutdown signal, closing gateway");
        shard_manager.shutdown_all().await;
    });

    // The second identity, when one is configured. Both run on this runtime and
    // are joined rather than spawned: if either gateway dies the process exits
    // and systemd restarts it, instead of the bot half-working with one set of
    // commands silently gone.
    let second = match (
        Config::provision_token()?,
        cfg.provision.guild_id,
        crate::provision::Provisioning::from_config(&cfg.provision),
    ) {
        (Some(token), Some(guild), Some(p)) => {
            Some(provisioning_client(token, guild, provision_driver, p).await?)
        }
        (Some(_), Some(_), None) => {
            anyhow::bail!(
                "a provisioning bot token and guild are set, but the provisioning paths are not \
                 — /dpstoken would register and then refuse every caller"
            )
        }
        _ => {
            tracing::info!(
                "single Discord identity; provisioning commands are not registered separately"
            );
            None
        }
    };

    match second {
        Some(mut second) => {
            let (a, b) = tokio::join!(client.start(), second.start());
            a.context("gateway run")?;
            b.context("provisioning gateway run")?;
        }
        None => client.start().await.context("gateway run")?,
    }
    Ok(())
}

// ===========================================================================
// M4: write commands — raids, DKP admin, characters, config. Every mutation
// flows through the single writer; embeds and replies happen after the fact
// is durable, and their failures are never fatal.
// ===========================================================================

pub const EMBED_BLUE_TICK: u32 = 3447003;
pub const EMBED_GREEN: u32 = 5763719;
pub const EMBED_ORANGE: u32 = 15105570;
pub const EMBED_PINK: u32 = 15277667;

use nocturnal_core::{Actor, Command};

use crate::driver::ExecError;

/// Wrap an outbound Discord REST call in a CLIENT span, per OTel guidance
/// ("create a new Span prior to the remote outgoing call"). serenity's own
/// request spans nest underneath, so a trace shows interaction → ledger →
/// each Discord call with its real latency.
pub async fn discord_call<F, T>(operation: &'static str, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    use tracing::Instrument as _;
    let span = tracing::info_span!(
        "discord.request",
        otel.kind = "client",
        otel.name = operation,
        server.address = "discord.com",
    );
    fut.instrument(span).await
}

/// Members currently in a voice channel, from the gateway cache.
pub fn voice_members(
    ctx: &serenity::Context,
    discord_guild: u64,
    channel: u64,
) -> Vec<nocturnal_core::PlayerId> {
    let Some(guild) = ctx.cache.guild(serenity::GuildId::new(discord_guild)) else {
        return Vec::new();
    };
    guild
        .voice_states
        .iter()
        .filter(|(_, vs)| vs.channel_id.map(|c| c.get()) == Some(channel))
        .map(|(user, _)| user.get())
        .collect()
}

/// Legacy `sendRaidEmebed`: Time + DKPs fields, players in inline chunks of 15.
pub fn raid_embed(
    color: u32,
    title: &str,
    players: &[nocturnal_core::PlayerId],
    dkps: i64,
) -> serenity::CreateEmbed {
    let now = chrono_now_ms() / 1000;
    let mut names: Vec<String> = players.iter().map(|p| format!("- <@{p}>")).collect();
    names.sort();
    let mut embed = serenity::CreateEmbed::new()
        .color(color)
        .title(title.to_owned())
        .field("Time", format!("<t:{now}:t>"), true)
        .field("DKPs", dkps.to_string(), true)
        .field("\u{200b}", "\u{200b}", false);
    if names.is_empty() {
        embed = embed.field(format!("Players ({})", players.len()), "No players", true);
    }
    for (i, chunk) in names.chunks(15).enumerate() {
        let label = if i == 0 {
            format!("Players ({})", players.len())
        } else {
            "\u{200b}".to_owned()
        };
        embed = embed.field(label, chunk.join("\n"), true);
    }
    embed
}

async fn send_log_embed(ctx: &Context<'_>, embed: serenity::CreateEmbed) {
    let ledger_guild = match require_guild(ctx) {
        Ok(g) => g,
        Err(_) => return,
    };
    let log_channel = ctx
        .data()
        .driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .and_then(|g| g.config.log_channel)
        })
        .await;
    if let Some(channel) = log_channel {
        if let Err(e) = discord_call("send log embed", async {
            serenity::ChannelId::new(channel)
                .send_message(
                    ctx.serenity_context(),
                    serenity::CreateMessage::new().embed(embed),
                )
                .await
        })
        .await
        {
            tracing::warn!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "log channel embed failed");
        }
    }
}

/// Legacy `restricted` gate: guild Administrators bypass; otherwise the
/// member needs the configured officer role.
pub async fn officer_check(ctx: Context<'_>) -> Result<bool, Error> {
    let Some(member) = ctx.author_member().await else {
        return Ok(false);
    };
    if member.permissions.is_some_and(|p| p.administrator()) {
        return Ok(true);
    }
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
    let allowed = admin_role.is_some_and(|r| member.roles.iter().any(|role| role.get() == r));
    if !allowed {
        ctx.say("You don't have the permission to use this command")
            .await?;
    }
    Ok(allowed)
}

pub(crate) async fn execute(
    ctx: &Context<'_>,
    cmd: Command,
) -> Result<Result<Vec<nocturnal_core::Envelope>, ExecError>, Error> {
    let ledger_guild = require_guild(ctx)?;
    Ok(ctx
        .data()
        .driver
        .execute(ledger_guild, Actor::User(ctx.author().id.get()), cmd)
        .await)
}

pub fn rejection_text(e: &ExecError) -> String {
    use nocturnal_core::Rejection as R;
    let rejection = match e {
        ExecError::Rejected(r) => r,
        ExecError::Storage(_) => {
            return ":no_entry: Storage failure — the command was NOT applied. Check the logs."
                .to_owned()
        }
    };
    match rejection {
        R::SameRaid => ":no_entry: That is the same raid on both sides.".to_owned(),
        R::RaidStillActive { name } => {
            format!(":no_entry: **{name}** is still running — `/endraid` it first.")
        }
        // The one people hit most: they tried to spend more than they have.
        R::InsufficientBalance {
            available,
            committed,
            needed,
        } if *committed > 0 => format!(
            ":no_entry: DKP Bot scowls at you. {needed} is more than you can cover: \
             you have **{}** DKP but **{committed}** is already committed to your bids on \
             other open auctions, leaving **{available}** available.",
            available + committed
        ),
        R::InsufficientBalance { available, needed, .. } => format!(
            ":no_entry: DKP Bot scowls at you. {needed} is greater than your current DKP (**{available}**)"
        ),
        R::BidBelowMinimum { min_bid } => format!(
            ":no_entry: DKP Bot scowls at you. Bid amount is less than the minimum bid ({min_bid})"
        ),
        R::InvalidAmount => {
            ":no_entry: DKP Bot scowls at you. Bid amount must be a whole number greater than 0"
                .to_owned()
        }
        R::AuctionNotFound => ":no_entry: Auction not found".to_owned(),
        R::AuctionNotActive => ":no_entry: Bidding on this auction has closed".to_owned(),
        R::AuctionNotClosed => ":no_entry: This auction is not awaiting confirmation".to_owned(),
        R::AuctionIdTaken => ":no_entry: That auction id already exists".to_owned(),
        R::RaidAlreadyActive { name } => {
            format!(":no_entry: There is already an active raid: {name}")
        }
        R::NoActiveRaid => {
            ":no_entry: There is no active raid, use /startraid to start one first".to_owned()
        }
        R::RaidNotFound => ":no_entry: Raid not found".to_owned(),
        R::TickTooSoon => ":no_entry: The next raid tick is not due yet".to_owned(),
        R::PlayerNotFound => {
            ":no_entry: No DKP record for that player yet — earn a tick first".to_owned()
        }
        R::CharacterNotRegistered { character } => {
            format!(":no_entry: Character {character} not registered")
        }
        R::CharacterAlreadyRegistered { character } => {
            format!(":no_entry: Character {character} already registered")
        }
        R::AlreadyProvisioned { username } => {
            format!(":no_entry: {username} already has a token")
        }
        R::NotProvisioned { username } => format!(":no_entry: {username} has no token"),
        R::InvalidConfig { setting, reason } => {
            format!(":no_entry: `{setting}` {reason} — nothing was changed")
        }
        R::RosterCharacterMissing { name } => {
            format!(":no_entry: **{name}** is not on your row — use `/roster add`")
        }
        R::RosterCharacterExists { name } => {
            format!(":no_entry: **{name}** is already on your row — use `/roster edit`")
        }
        R::InvalidRosterEntry { field, reason } => {
            format!(":no_entry: `{field}` {reason}")
        }
    }
}

/// Set bot configuration (channels, officer role, raid/bid defaults).
#[allow(clippy::too_many_arguments)]
#[poise::command(
    slash_command,
    ephemeral,
    rename = "configure",
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn configure(
    ctx: Context<'_>,
    #[description = "Officer role who can handle raids and dkps"] role: serenity::Role,
    #[description = "The raid voice channel"]
    #[channel_types("Voice")]
    raidchannel: serenity::GuildChannel,
    #[description = "Channel for DKP log movements"]
    #[channel_types("Text")]
    logchannel: serenity::GuildChannel,
    #[description = "Channel for auctions"]
    #[channel_types("Text")]
    auctionchannel: serenity::GuildChannel,
    #[description = "Minutes between ticks (e.g. 6)"] tickduration: Option<f64>,
    #[description = "Days before raids stop counting for attendance"] raiddeprecationtime: Option<
        f64,
    >,
    #[description = "Short auction duration in seconds (30-1000)"]
    #[min = 30]
    #[max = 1000]
    bidtime: Option<i64>,
    #[description = "Channel for long auctions"]
    #[channel_types("Text")]
    longauctionchannel: Option<serenity::GuildChannel>,
    #[description = "Second raid voice channel"]
    #[channel_types("Voice")]
    secondraidchannel: Option<serenity::GuildChannel>,
    #[description = "Minimum bid"]
    #[min = 0]
    minbid: Option<i64>,
    #[description = "Minimum bid to lock as MAIN"]
    #[min = 0]
    minbidtolockformain: Option<i64>,
    #[description = "Overbid needed for an ALT to win over MAIN"]
    #[min = 0]
    overbidtowinmain: Option<i64>,
    #[description = "RaidHelper API key (enables event linking and awards)"]
    raidhelperapikey: Option<String>,
    #[description = "DKP awarded to signups who attended, when a linked raid ends (default 5)"]
    #[min = 0]
    raidhelpereventdkp: Option<i64>,
) -> Result<(), Error> {
    crate::discord::ack_ephemeral(&ctx).await?;
    // Every value here is validated by the decide step, which is the only
    // place that sees the merged result — set the second raid channel today
    // and the raid channel tomorrow and there is no single call to check.
    let patch = nocturnal_core::event::ConfigPatch {
        admin_role: Some(role.id.get()),
        raid_channel: Some(raidchannel.id.get()),
        log_channel: Some(logchannel.id.get()),
        auction_channel: Some(auctionchannel.id.get()),
        long_auction_channel: longauctionchannel.map(|c| c.id.get()),
        second_raid_channel: secondraidchannel.map(|c| c.id.get()),
        tick_duration_ms: tickduration.map(|m| (m * 60_000.0) as i64),
        raid_deprecation_ms: raiddeprecationtime
            .map(|d| (d * nocturnal_core::state::DAY_MS as f64) as i64),
        bid_time_s: bidtime,
        min_bid: minbid,
        min_bid_to_lock_for_main: minbidtolockformain,
        over_bid_to_win_main: overbidtowinmain,
        raidhelper_api_key: raidhelperapikey.map(nocturnal_core::Secret::from),
        raidhelper_event_dkp: raidhelpereventdkp,
    };
    match execute(&ctx, Command::UpdateConfig { patch }).await? {
        Ok(_) => ctx.say("Configuration saved").await?,
        Err(e) => ctx.say(rejection_text(&e)).await?,
    };
    Ok(())
}

/// Tick durations are sub-minute in practice (`0.5` = 30 s), so plain integer
/// minutes would render the guild's real setting as "0 minutes".
fn human_ms(ms: i64) -> String {
    if ms % 60_000 == 0 {
        format!("{} minutes", ms / 60_000)
    } else {
        format!("{} seconds", ms as f64 / 1000.0)
    }
}

/// Show the current configuration of the bot in this server.
#[poise::command(
    slash_command,
    ephemeral,
    rename = "showconfig",
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn showconfig(ctx: Context<'_>) -> Result<(), Error> {
    let ledger_guild = require_guild(&ctx)?;
    crate::discord::ack_ephemeral(&ctx).await?;
    let cfg = ctx
        .data()
        .driver
        .query(move |l| l.state().guild(ledger_guild).map(|g| g.config.clone()))
        .await
        .unwrap_or_default();
    let role = |v: Option<u64>| v.map_or("Not set".into(), |r| format!("<@&{r}>"));
    let chan = |v: Option<u64>| v.map_or("Not set".into(), |c| format!("<#{c}>"));
    let embed = serenity::CreateEmbed::new()
        .color(EMBED_GREEN)
        .title("Current configuration")
        .field("DKP Officer role", role(cfg.admin_role), false)
        .field(
            "Raid deprecation time",
            format!(
                "{} days",
                cfg.raid_deprecation_ms / nocturnal_core::state::DAY_MS
            ),
            false,
        )
        .field("Raid channel", chan(cfg.raid_channel), false)
        .field("Second raid channel", chan(cfg.second_raid_channel), false)
        .field("Log channel", chan(cfg.log_channel), false)
        .field("Auction channel", chan(cfg.auction_channel), false)
        .field(
            "Long auction channel",
            chan(cfg.long_auction_channel),
            false,
        )
        .field("Bid time", format!("{} seconds", cfg.bid_time_s), false)
        .field("Tick duration", human_ms(cfg.tick_duration_ms), false)
        .field("Minimum bid", format!("{} DKP", cfg.min_bid), false)
        .field(
            "Minimum bid to lock for main",
            format!("{} DKP", cfg.min_bid_to_lock_for_main),
            false,
        )
        .field(
            "Over bid to win main",
            format!("{} DKP", cfg.over_bid_to_win_main),
            false,
        )
        // Presence only. The key is a live credential, and this embed is one
        // screenshot away from a public channel.
        .field(
            "RaidHelper API key",
            if cfg.raidhelper_api_key.is_some() {
                "Set"
            } else {
                "Not set"
            },
            false,
        )
        .field(
            "RaidHelper event DKP",
            format!("{} DKP", cfg.raidhelper_event_dkp),
            false,
        );
    ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}

/// Ended raids, newest first, as picker choices: "Seru & Emp · 31 Aug · 2 entries".
async fn autocomplete_raid(ctx: Context<'_>, partial: &str) -> Vec<serenity::AutocompleteChoice> {
    let Ok(ledger_guild) = require_guild(&ctx) else {
        return Vec::new();
    };
    let partial = partial.to_lowercase();
    ctx.data()
        .driver
        .query(move |l| {
            let Some(g) = l.state().guild(ledger_guild) else {
                return Vec::new();
            };
            let mut raids: Vec<(&String, &nocturnal_core::state::Raid)> =
                g.raids.iter().filter(|(_, r)| !r.active).collect();
            raids.sort_by_key(|(_, r)| std::cmp::Reverse(r.date_ms));
            raids
                .into_iter()
                .filter(|(id, r)| {
                    partial.is_empty()
                        || r.name.to_lowercase().contains(&partial)
                        || id.contains(&partial)
                })
                .take(25)
                .map(|(id, r)| {
                    let name: String = r.name.chars().take(40).collect();
                    let when = crate::web::pages::day(r.date_ms);
                    let when: Vec<&str> = when.split(' ').skip(1).take(2).collect();
                    serenity::AutocompleteChoice::new(
                        format!("{name} · {} · {} entries", when.join(" "), r.entries.len()),
                        id.clone(),
                    )
                })
                .collect()
        })
        .await
}

/// Fold a false-start raid into the real one (attendance and loot lines
/// re-filed, no DKP moves).
#[tracing::instrument(name = "command.mergeraid", skip_all, fields(otel.kind = "server"))]
#[poise::command(
    slash_command,
    ephemeral,
    rename = "mergeraid",
    check = "officer_check"
)]
pub async fn mergeraid(
    ctx: Context<'_>,
    #[description = "The false start — it goes away"]
    #[autocomplete = "autocomplete_raid"]
    from: String,
    #[description = "The real raid — it keeps everything"]
    #[autocomplete = "autocomplete_raid"]
    into: String,
) -> Result<(), Error> {
    let ledger_guild = require_guild(&ctx)?;
    crate::discord::ack_ephemeral(&ctx).await?;
    let (f, i) = (from.clone(), into.clone());
    let names = ctx
        .data()
        .driver
        .query(move |l| {
            let g = l.state().guild(ledger_guild)?;
            let src = g.raids.get(&f)?;
            let dst = g.raids.get(&i)?;
            Some((src.name.clone(), dst.name.clone(), src.entries.len()))
        })
        .await;
    match execute(&ctx, Command::MergeRaid { from, into }).await? {
        Ok(_) => {
            let (fname, iname, n) = names.unwrap_or_default();
            ctx.say(format!(
                "Merged **{fname}** into **{iname}** — {n} attendance entries and every DKP line \
                 re-filed under it; no balances changed. The site is rebuilding."
            ))
            .await?;
            if let Some(out) = &ctx.data().roster_output {
                crate::roster_page::rematerialize(
                    ctx.serenity_context().http.as_ref(),
                    ctx.guild_id().map_or(0, |g| g.get()),
                    &ctx.data().driver,
                    out,
                    &ctx.data().members,
                    ledger_guild,
                    ctx.data().ourios.as_ref(),
                    &ctx.data().item_mirror,
                    &ctx.data().site,
                )
                .await;
            }
        }
        Err(e) => {
            ctx.say(rejection_text(&e)).await?;
        }
    }
    Ok(())
}

/// Create a new raid.
#[tracing::instrument(name = "command.startraid", skip_all, fields(otel.kind = "server"))]
#[poise::command(
    slash_command,
    ephemeral,
    rename = "startraid",
    check = "officer_check"
)]
pub async fn startraid(
    ctx: Context<'_>,
    #[description = "Name"] name: Option<String>,
    #[description = "DKP per tick"]
    #[min = 0]
    dkpspertick: Option<i64>,
    #[description = "Minutes between ticks (0.5 = 30s)"] tickduration: Option<f64>,
) -> Result<(), Error> {
    let ledger_guild = require_guild(&ctx)?;
    let discord_guild = ctx.guild_id().map(|g| g.get()).unwrap_or_default();
    crate::discord::ack_ephemeral(&ctx).await?;
    let (raid_channel, second_channel, cfg_tick_ms) = ctx
        .data()
        .driver
        .query(move |l| {
            let g = l.state().guild(ledger_guild);
            (
                g.and_then(|g| g.config.raid_channel),
                g.and_then(|g| g.config.second_raid_channel),
                g.map_or(6 * 60_000, |g| g.config.tick_duration_ms),
            )
        })
        .await;
    let Some(raid_channel) = raid_channel else {
        ctx.say(":no_entry: Raid channel not set, use /configure to set it")
            .await?;
        return Ok(());
    };
    let mut players = voice_members(ctx.serenity_context(), discord_guild, raid_channel);
    if let Some(second) = second_channel {
        players.extend(voice_members(ctx.serenity_context(), discord_guild, second));
    }
    let dkp_per_tick = dkpspertick.unwrap_or(1);
    let tick_interval_ms = tickduration.map_or(cfg_tick_ms, |m| (m * 60_000.0) as i64);

    // RaidHelper: an event starting within ±10 minutes names and links the
    // raid, exactly like the legacy /startraid.
    let api_key = ctx
        .data()
        .driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .and_then(|g| g.config.raidhelper_api_key.clone())
        })
        .await;
    let mut event_id = None;
    let mut name = name;
    if let Some(key) = api_key {
        match crate::raidhelper::event_starting_now(key.as_str(), discord_guild, chrono_now_ms())
            .await
        {
            Ok(Some(event)) => {
                if name.is_none() {
                    name = Some(event.title.clone());
                }
                event_id = Some(event.id);
            }
            Ok(None) => {}
            // A RaidHelper hiccup must never stop a raid starting.
            Err(e) => tracing::warn!(
                { attr::NOCTURNAL_ERROR_MESSAGE } = %e,
                "raid-helper lookup failed; starting unlinked"
            ),
        }
    }
    let name = name.unwrap_or_else(|| format!("<t:{}:D>", chrono_now_ms() / 1000));
    let raid_id = format!("rd-{:x}", chrono_now_ms());
    let cmd = Command::StartRaid {
        raid_id,
        name: name.clone(),
        tick_interval_ms,
        dkp_per_tick,
        players_present: players.clone(),
        event_id: event_id.clone(),
    };
    match execute(&ctx, cmd).await? {
        Ok(_) => {
            ctx.say(format!(
                "Raid {name} started with {dkp_per_tick} DKP per tick every {} minutes",
                tick_interval_ms as f64 / 60_000.0
            ))
            .await?;
            send_log_embed(
                &ctx,
                raid_embed(
                    EMBED_GREEN,
                    &format!("{name} raid Start"),
                    &players,
                    dkp_per_tick,
                ),
            )
            .await;
        }
        Err(e) => {
            ctx.say(rejection_text(&e)).await?;
        }
    }
    Ok(())
}

/// End the current raid.
#[tracing::instrument(name = "command.endraid", skip_all, fields(otel.kind = "server"))]
#[poise::command(slash_command, ephemeral, rename = "endraid", check = "officer_check")]
pub async fn endraid(ctx: Context<'_>) -> Result<(), Error> {
    let ledger_guild = require_guild(&ctx)?;
    let discord_guild = ctx.guild_id().map(|g| g.get()).unwrap_or_default();
    crate::discord::ack_ephemeral(&ctx).await?;
    let (raid_channel, second_channel) = ctx
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
    let mut players = Vec::new();
    if let Some(c) = raid_channel {
        players = voice_members(ctx.serenity_context(), discord_guild, c);
    }
    if let Some(second) = second_channel {
        players.extend(voice_members(ctx.serenity_context(), discord_guild, second));
    }
    let envelopes = match execute(
        &ctx,
        Command::EndRaid {
            players_present: players,
            reason: "officer".into(),
        },
    )
    .await?
    {
        Ok(env) => env,
        Err(e) => {
            ctx.say(rejection_text(&e)).await?;
            return Ok(());
        }
    };
    let raid_id = envelopes
        .iter()
        .find_map(|e| match &e.event {
            nocturnal_core::Event::RaidEnded { raid_id, .. } => Some(raid_id.clone()),
            _ => None,
        })
        .unwrap_or_default();
    // Movement log: attendance entries aggregated by consecutive comment,
    // plus every loot debit tied to this raid (legacy getRaidDKPMovements).
    let rid = raid_id.clone();
    let (raid_name, lines) = ctx
        .data()
        .driver
        .query(move |l| {
            let Some(g) = l.state().guild(ledger_guild) else {
                return (String::new(), Vec::new());
            };
            let Some(raid) = g.raids.get(&rid) else {
                return (String::new(), Vec::new());
            };
            #[derive(Clone)]
            struct Movement {
                ts_ms: i64,
                text: String,
            }
            // One line per attendance entry — every Start, Tick and End, as
            // officers are used to reading them (the legacy code aggregated
            // into a variable it then never used).
            let mut moves: Vec<Movement> = raid
                .entries
                .iter()
                .map(|e| Movement {
                    ts_ms: e.ts_ms,
                    text: format!(
                        "<t:{}:t> *{}*{}",
                        e.ts_ms / 1000,
                        e.comment,
                        if e.amount != 0 {
                            format!(" — {} dkp to {} player(s)", e.amount, e.players.len())
                        } else {
                            String::new()
                        }
                    ),
                })
                .collect();
            for (player, p) in &g.players {
                for e in &p.log {
                    if e.dkp < 0 && e.raid.as_ref().is_some_and(|r| r.raid_id == rid) {
                        let what = match &e.item {
                            Some(item) => match &item.url {
                                Some(url) => {
                                    format!("won [{}]({url}) for {} dkps", item.name, -e.dkp)
                                }
                                None => format!("won {} for {} dkps", item.name, -e.dkp),
                            },
                            None => format!("lost {} dkps *{}*", -e.dkp, e.comment),
                        };
                        moves.push(Movement {
                            ts_ms: e.ts_ms,
                            text: format!("<t:{}:t> <@{player}> {what}", e.ts_ms / 1000),
                        });
                    }
                }
            }
            moves.sort_by_key(|m| m.ts_ms);
            (
                raid.name.clone(),
                moves.into_iter().map(|m| m.text).collect::<Vec<_>>(),
            )
        })
        .await;
    ctx.say(format!("Raid {raid_name} ended")).await?;

    // Linked to a RaidHelper event? Award the signups who actually turned up.
    let rid = raid_id.clone();
    let linked = ctx
        .data()
        .driver
        .query(move |l| {
            l.state().guild(ledger_guild).and_then(|g| {
                let raid = g.raids.get(&rid)?;
                raid.event_id
                    .clone()
                    .map(|e| (e, g.config.raidhelper_event_dkp))
            })
        })
        .await;
    if let Some((event_id, dkp)) = linked {
        match award_raidhelper_event(&ctx, &raid_id, &event_id, dkp).await {
            Ok(summary) => tracing::info!(
                { attr::NOCTURNAL_RAID_ID } = raid_id,
                { attr::NOCTURNAL_RAID_EVENT_ID } = event_id,
                { attr::NOCTURNAL_RAID_AWARD_SUMMARY } = %summary,
                "raid event DKP awarded"
            ),
            Err(e) => {
                tracing::warn!(
                    { attr::NOCTURNAL_RAID_ID } = raid_id,
                    { attr::NOCTURNAL_ERROR_MESSAGE } = %e,
                    "raid event DKP failed; raid still ended"
                )
            }
        }
    }
    let chunks: Vec<&[String]> = lines.chunks(35).collect();
    let total = chunks.len().max(1);
    for (i, chunk) in chunks.iter().enumerate() {
        let embed = serenity::CreateEmbed::new()
            .color(EMBED_PINK)
            .title(format!("{raid_name} raid ended - *{} of {total}*", i + 1))
            .description(chunk.join("\n"))
            .field(
                "Date",
                format!("<t:{0}:d> <t:{0}:t>", chrono_now_ms() / 1000),
                true,
            )
            .field("ID", raid_id.clone(), true);
        send_log_embed(&ctx, embed).await;
    }
    Ok(())
}

/// Add DKP to a player.
#[tracing::instrument(name = "command.adddkp", skip_all, fields(otel.kind = "server"))]
#[poise::command(slash_command, rename = "adddkp", check = "officer_check")]
pub async fn adddkp(
    ctx: Context<'_>,
    #[description = "The player"] player: serenity::User,
    #[description = "The amount of DKP to add"]
    #[min = 1]
    dkp: i64,
    #[description = "Reason"] comment: String,
) -> Result<(), Error> {
    crate::discord::ack(&ctx).await?;
    let cmd = Command::AdjustDkp {
        player: player.id.get(),
        delta: dkp,
        comment: comment.clone(),
        item: None,
    };
    match execute(&ctx, cmd).await? {
        Ok(_) => {
            ctx.say(format!(
                "Added {dkp} DKPs to <@{}>. {comment}",
                player.id.get()
            ))
            .await?
        }
        Err(e) => ctx.say(rejection_text(&e)).await?,
    };
    Ok(())
}

/// Remove DKP from a player.
#[tracing::instrument(name = "command.removedkp", skip_all, fields(otel.kind = "server"))]
#[poise::command(slash_command, rename = "removedkp", check = "officer_check")]
pub async fn removedkp(
    ctx: Context<'_>,
    #[description = "The player"] player: serenity::User,
    #[description = "The amount of DKP to remove"]
    #[min = 1]
    dkp: i64,
    #[description = "Reason"] comment: String,
) -> Result<(), Error> {
    crate::discord::ack(&ctx).await?;
    let cmd = Command::AdjustDkp {
        player: player.id.get(),
        delta: -dkp,
        comment: comment.clone(),
        item: None,
    };
    match execute(&ctx, cmd).await? {
        Ok(_) => {
            ctx.say(format!(
                "Removed {dkp} DKPs from <@{}>. {comment}",
                player.id.get()
            ))
            .await?
        }
        Err(e) => ctx.say(rejection_text(&e)).await?,
    };
    Ok(())
}

/// Add DKP to everyone in the raid channel.
#[tracing::instrument(name = "command.addraiddkp", skip_all, fields(otel.kind = "server"))]
#[poise::command(
    slash_command,
    ephemeral,
    rename = "addraiddkp",
    check = "officer_check"
)]
pub async fn addraiddkp(
    ctx: Context<'_>,
    #[description = "The amount of DKP to add"]
    #[min = 0]
    dkp: i64,
    #[description = "Reason"] comment: String,
) -> Result<(), Error> {
    let ledger_guild = require_guild(&ctx)?;
    let discord_guild = ctx.guild_id().map(|g| g.get()).unwrap_or_default();
    crate::discord::ack_ephemeral(&ctx).await?;
    let raid_channel = ctx
        .data()
        .driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .and_then(|g| g.config.raid_channel)
        })
        .await;
    let Some(raid_channel) = raid_channel else {
        ctx.say(":no_entry: Please set the raid channel first with /configure")
            .await?;
        return Ok(());
    };
    let players = voice_members(ctx.serenity_context(), discord_guild, raid_channel);
    if players.is_empty() {
        ctx.say(":no_entry: No players in the raid channel").await?;
        return Ok(());
    }
    let (raid_name, dkp_amount) = (comment.clone(), dkp);
    match execute(
        &ctx,
        Command::AwardRaid {
            players: players.clone(),
            amount: dkp,
            comment: comment.clone(),
        },
    )
    .await?
    {
        Ok(_) => {
            ctx.say(format!(
                "Added {dkp} DKP to all players ({}) in the raid channel",
                players.len()
            ))
            .await?;
            let title = ctx
                .data()
                .driver
                .query(move |l| {
                    l.state()
                        .guild(ledger_guild)
                        .and_then(|g| {
                            let id = g.active_raid.as_ref()?;
                            g.raids.get(id).map(|r| format!("{}: {raid_name}", r.name))
                        })
                        .unwrap_or(raid_name)
                })
                .await;
            send_log_embed(&ctx, raid_embed(EMBED_ORANGE, &title, &players, dkp_amount)).await;
        }
        Err(e) => {
            ctx.say(rejection_text(&e)).await?;
        }
    }
    Ok(())
}

/// Register an EQ character to your Discord account.
#[tracing::instrument(name = "command.registercharacter", skip_all, fields(otel.kind = "server"))]
#[poise::command(slash_command, rename = "registercharacter")]
pub async fn registercharacter(
    ctx: Context<'_>,
    #[description = "The character name"] name: String,
) -> Result<(), Error> {
    crate::discord::ack_ephemeral(&ctx).await?;
    let cmd = Command::LinkCharacter {
        player: ctx.author().id.get(),
        character: name.clone(),
    };
    match execute(&ctx, cmd).await? {
        Ok(_) => ctx.say(format!("Successfully registered {name}!")).await?,
        Err(e) => ctx.say(rejection_text(&e)).await?,
    };
    Ok(())
}

// ===========================================================================
// /stresstest — replays the legacy bot's death scenario end to end: real
// auction embeds posted to the auction channel, live embed edits during the
// bid storm (the exact load that congested the legacy event loop), every bid
// fsynced through the single writer. Auctions are cancelled by default; pass
// finalize:true to exercise the debit path.
// ===========================================================================

fn percentile(sorted_ms: &[f64], p: f64) -> f64 {
    if sorted_ms.is_empty() {
        return 0.0;
    }
    let idx = ((sorted_ms.len() as f64 - 1.0) * p).round() as usize;
    sorted_ms[idx.min(sorted_ms.len() - 1)]
}

fn stats(mut ms: Vec<f64>) -> String {
    if ms.is_empty() {
        return "-".into();
    }
    ms.sort_by(|a, b| a.total_cmp(b));
    format!(
        "p50 `{:.2}ms`  p95 `{:.2}ms`  max `{:.2}ms`  (n={})",
        percentile(&ms, 0.50),
        percentile(&ms, 0.95),
        percentile(&ms, 1.0),
        ms.len()
    )
}

fn stress_embed(
    item: &nocturnal_core::Item,
    bids: usize,
    status: &str,
    color: u32,
) -> serenity::CreateEmbed {
    item_embed(item, color)
        .field("Bids", bids.to_string(), true)
        .field("Status", status.to_owned(), true)
}

/// Stress test: concurrent auctions with live embeds + every player bidding.
#[tracing::instrument(name = "command.stresstest", skip_all, fields(otel.kind = "server"))]
#[poise::command(
    slash_command,
    ephemeral,
    rename = "stresstest",
    check = "officer_check"
)]
#[allow(clippy::too_many_lines)]
pub async fn stresstest(
    ctx: Context<'_>,
    #[description = "Concurrent auctions (default 4 — the legacy killer)"]
    #[min = 1]
    #[max = 16]
    auctions: Option<usize>,
    #[description = "Concurrent bidders (default 40 — a full raid)"]
    #[min = 1]
    #[max = 200]
    bidders: Option<usize>,
    #[description = "Balance lookups per bidder (default 10)"]
    #[min = 0]
    #[max = 100]
    lookups: Option<usize>,
    #[description = "Actually finalize (debits winners 1 DKP) instead of cancelling"]
    finalize: Option<bool>,
    #[description = "Delete the auction messages afterwards"] cleanup: Option<bool>,
) -> Result<(), Error> {
    let ledger_guild = require_guild(&ctx)?;
    crate::discord::ack_ephemeral(&ctx).await?;
    let n_auctions = auctions.unwrap_or(4);
    let n_bidders = bidders.unwrap_or(40);
    let n_lookups = lookups.unwrap_or(10);
    let finalize = finalize.unwrap_or(false);
    let cleanup = cleanup.unwrap_or(false);
    let driver = ctx.data().driver.clone();
    let http = ctx.serenity_context().http.clone();
    let actor = Actor::User(ctx.author().id.get());

    let auction_channel = driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .and_then(|g| g.config.auction_channel)
        })
        .await;
    let Some(auction_channel) = auction_channel else {
        ctx.say(":no_entry: Auction channel not set — run /configure first")
            .await?;
        return Ok(());
    };
    let channel = serenity::ChannelId::new(auction_channel);

    let need = n_auctions as i64;
    let players: Vec<nocturnal_core::PlayerId> = driver
        .query(move |l| {
            let Some(g) = l.state().guild(ledger_guild) else {
                return Vec::new();
            };
            let mut ps: Vec<_> = g
                .players
                .iter()
                .filter(|(_, p)| p.balance >= need)
                .map(|(id, p)| (*id, p.balance))
                .collect();
            ps.sort_by_key(|(_, b)| std::cmp::Reverse(*b));
            ps.into_iter().map(|(id, _)| id).collect()
        })
        .await;
    let players: Vec<_> = players.into_iter().take(n_bidders).collect();
    if players.is_empty() {
        ctx.say(":no_entry: No players with enough DKP in the ledger to simulate bidders")
            .await?;
        return Ok(());
    }

    let run_id = chrono_now_ms();
    tracing::info!(
        { attr::NOCTURNAL_STRESSTEST_AUCTION_COUNT } = n_auctions,
        { attr::NOCTURNAL_STRESSTEST_BIDDER_COUNT } = players.len(),
        { attr::NOCTURNAL_STRESSTEST_LOOKUP_COUNT } = n_lookups,
        "stresstest: starting"
    );
    // Phase 0: real item lookups, exactly like the legacy /startbid hot path
    // (search + per-item detail scrape; audit #42 — now with timeouts and a
    // permanent cache, so a rerun measures the cached path).
    let mut item_ms: Vec<f64> = Vec::new();
    let mut stress_items: Vec<nocturnal_core::Item> = Vec::new();
    let t = std::time::Instant::now();
    let hits = ctx
        .data()
        .items
        .search("sword", crate::items::Database::Quarm)
        .await;
    item_ms.push(t.elapsed().as_secs_f64() * 1000.0);
    if let Ok(crate::items::SearchOutcome::Many(refs)) = hits {
        for r in refs.iter().take(n_auctions) {
            let t = std::time::Instant::now();
            if let Ok(Some(item)) = ctx
                .data()
                .items
                .by_id(&r.id, crate::items::Database::Quarm)
                .await
            {
                stress_items.push(item);
            }
            item_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        }
    }
    for i in stress_items.len()..n_auctions {
        // Item sites unreachable: degrade to synthetic, never fail the run.
        stress_items.push(nocturnal_core::Item {
            id: format!("stress{i}"),
            name: format!("Stress Test Item #{i}"),
            url: None,
            data: None,
            image: None,
        });
    }

    let t0 = std::time::Instant::now();

    // Phase 1: open the auctions in the ledger AND post their live embeds.
    let mut auction_ids = Vec::new();
    let mut messages: Vec<serenity::Message> = Vec::new();
    let mut post_ms = Vec::new();
    for (i, stress_item) in stress_items.iter().enumerate().take(n_auctions) {
        let auction_id = format!("stress-{run_id:x}-{i}");
        driver
            .execute(
                ledger_guild,
                actor,
                Command::OpenAuction {
                    auction_id: auction_id.clone(),
                    item: stress_item.clone(),
                    flavor: nocturnal_core::Flavor::Short,
                    min_bid: 0,
                    num_items: 1,
                    min_bid_to_lock_for_main: 0,
                    over_bid_to_win_main: 0,
                    duration_ms: 600_000,
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!("open auction: {e}"))?;
        let t = std::time::Instant::now();
        let msg = discord_call("post auction embed", async {
            channel
                .send_message(
                    &http,
                    serenity::CreateMessage::new().embed(stress_embed(
                        stress_item,
                        0,
                        "bidding…",
                        EMBED_ORANGE,
                    )),
                )
                .await
        })
        .await?;
        post_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        messages.push(msg);
        auction_ids.push(auction_id);
    }

    // Live editor: continuously updates every auction embed while the storm
    // runs — the "other auction embeds are being edited" half of the legacy
    // crash anatomy, and the load that exercises Discord's edit buckets.
    let editing = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let editor = {
        let editing = editing.clone();
        let driver = driver.clone();
        let http = http.clone();
        let auction_ids = auction_ids.clone();
        let editor_items = stress_items.clone();
        let message_ids: Vec<serenity::MessageId> = messages.iter().map(|m| m.id).collect();
        // Spawned tasks do not inherit the current span: attach it explicitly
        // or their Discord calls land in a separate, parentless trace.
        let task_span = tracing::info_span!("stresstest.editor", otel.kind = "internal");
        tokio::spawn(
            async move {
                let mut edit_ms: Vec<f64> = Vec::new();
                while editing.load(std::sync::atomic::Ordering::Acquire) {
                    for (i, (auction_id, msg_id)) in
                        auction_ids.iter().zip(&message_ids).enumerate()
                    {
                        let aid = auction_id.clone();
                        let bids = driver
                            .query(move |l| {
                                l.state()
                                    .guild(ledger_guild)
                                    .and_then(|g| g.auctions.get(&aid).map(|a| a.bids.len()))
                                    .unwrap_or_default()
                            })
                            .await;
                        let t = std::time::Instant::now();
                        let result = discord_call("edit auction embed", async {
                            channel
                                .edit_message(
                                    &http,
                                    *msg_id,
                                    serenity::EditMessage::new().embed(stress_embed(
                                        &editor_items[i],
                                        bids,
                                        "bidding…",
                                        EMBED_ORANGE,
                                    )),
                                )
                                .await
                        })
                        .await;
                        edit_ms.push(t.elapsed().as_secs_f64() * 1000.0);
                        if let Err(e) = result {
                            tracing::warn!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "stress edit failed");
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                }
                edit_ms
            }
            .instrument(task_span),
        )
    };

    // Phase 2: every bidder at once — lookups then a bid on every auction.
    let mut tasks = Vec::new();
    for &player in &players {
        let driver = driver.clone();
        let auction_ids = auction_ids.clone();
        let bidder_span = tracing::info_span!("stresstest.bidder", otel.kind = "internal");
        tasks.push(tokio::spawn(
            async move {
                let mut lookup_ms = Vec::new();
                let mut bid_ms = Vec::new();
                let mut accepted = 0usize;
                let mut rejected = 0usize;
                for _ in 0..n_lookups {
                    let t = std::time::Instant::now();
                    let _balance = driver
                        .query(move |l| {
                            l.state()
                                .guild(ledger_guild)
                                .map(|g| g.balance(player))
                                .unwrap_or_default()
                        })
                        .await;
                    lookup_ms.push(t.elapsed().as_secs_f64() * 1000.0);
                }
                for auction_id in &auction_ids {
                    let t = std::time::Instant::now();
                    let result = driver
                        .execute(
                            ledger_guild,
                            Actor::User(player),
                            Command::PlaceBid {
                                auction_id: auction_id.clone(),
                                player,
                                amount: 1,
                                for_main: true,
                            },
                        )
                        .await;
                    bid_ms.push(t.elapsed().as_secs_f64() * 1000.0);
                    match result {
                        Ok(_) => accepted += 1,
                        Err(_) => rejected += 1,
                    }
                }
                (lookup_ms, bid_ms, accepted, rejected)
            }
            .instrument(bidder_span),
        ));
    }
    let mut lookup_ms = Vec::new();
    let mut bid_ms = Vec::new();
    let (mut accepted, mut rejected) = (0usize, 0usize);
    for task in tasks {
        let (l, b, a, r) = task.await?;
        lookup_ms.extend(l);
        bid_ms.extend(b);
        accepted += a;
        rejected += r;
    }
    let storm = t0.elapsed();
    tracing::info!(
        { attr::NOCTURNAL_STRESSTEST_ACCEPTED_COUNT } = accepted,
        { attr::NOCTURNAL_STRESSTEST_REJECTED_COUNT } = rejected,
        { attr::NOCTURNAL_STRESSTEST_DURATION } = ?storm,
        "stresstest: bid storm done"
    );
    editing.store(false, std::sync::atomic::Ordering::Release);
    let edit_ms = editor.await?;

    // Phase 3: close, then finalize or cancel; settle the embeds.
    let mut winners = 0usize;
    for (i, auction_id) in auction_ids.iter().enumerate() {
        driver
            .execute(
                ledger_guild,
                actor,
                Command::CloseAuction {
                    auction_id: auction_id.clone(),
                    ended_ts_ms: None,
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!("close: {e}"))?;
        if finalize {
            let env = driver
                .execute(
                    ledger_guild,
                    actor,
                    Command::FinalizeAuction {
                        auction_id: auction_id.clone(),
                        seed: run_id as u64,
                    },
                )
                .await
                .map_err(|e| anyhow::anyhow!("finalize: {e}"))?;
            winners += env
                .iter()
                .filter_map(|e| match &e.event {
                    nocturnal_core::Event::AuctionFinalized { winners, .. } => Some(winners.len()),
                    _ => None,
                })
                .sum::<usize>();
        } else {
            driver
                .execute(
                    ledger_guild,
                    actor,
                    Command::CancelAuction {
                        auction_id: auction_id.clone(),
                        reason: "stress test".into(),
                    },
                )
                .await
                .map_err(|e| anyhow::anyhow!("cancel: {e}"))?;
        }
        let aid = auction_id.clone();
        let bids = driver
            .query(move |l| {
                l.state()
                    .guild(ledger_guild)
                    .and_then(|g| g.auctions.get(&aid).map(|a| a.bids.len()))
                    .unwrap_or_default()
            })
            .await;
        let status = if finalize {
            "finalized"
        } else {
            "cancelled (stress test)"
        };
        let _ = channel
            .edit_message(
                &http,
                messages[i].id,
                serenity::EditMessage::new().embed(stress_embed(
                    &stress_items[i],
                    bids,
                    status,
                    EMBED_GREEN,
                )),
            )
            .await;
    }
    if cleanup {
        for msg in &messages {
            let _ = channel.delete_message(&http, msg.id).await;
        }
    }
    let total = t0.elapsed();

    let (negative_balances, head) = driver
        .query(move |l| {
            let neg = l
                .state()
                .guild(ledger_guild)
                .map(|g| g.players.values().filter(|p| p.balance < 0).count())
                .unwrap_or_default();
            (neg, l.next_seq())
        })
        .await;

    let outcome = if finalize {
        format!("finalized — {winners} winner(s) debited 1 DKP each")
    } else {
        "cancelled — no balance changes".to_owned()
    };
    let embed = serenity::CreateEmbed::new()
        .color(EMBED_GREEN)
        .title("Stress test — the legacy killer scenario")
        .description(format!(
            "**{n_auctions} concurrent auctions × {} bidders**, live embeds edited throughout, every bid fsynced through the single writer.\n\
             The legacy bot died here (10062 → crash, all auctions lost). This run: nothing dropped, nothing raced.",
            players.len()
        ))
        .field("Balance lookups", stats(lookup_ms), false)
        .field("Bids (decide → fsync → apply)", stats(bid_ms), false)
        .field("Item lookups (pqdi.cc; cached after first run)", stats(item_ms), false)
        .field("Discord: embed posts", stats(post_ms), false)
        .field(format!("Discord: live embed edits ({})", edit_ms.len()), stats(edit_ms), false)
        .field("Bids accepted / rejected", format!("{accepted} / {rejected}"), true)
        .field("Bid storm", format!("{:.2}s", storm.as_secs_f64()), true)
        .field("Total (incl. close)", format!("{:.2}s", total.as_secs_f64()), true)
        .field("Auctions", outcome, false)
        .field(
            "Integrity",
            format!(
                "players with a negative balance: **{negative_balances}** (the legacy import carries 2; one more would be a bug) · ledger head seq {head}"
            ),
            false,
        )
        .field(
            "Rate limiting",
            "Watch the Overview dashboard's Discord section — edit-bucket delays show there as the pre-429 early warning.",
            false,
        );
    ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}

// ===========================================================================
// /searchitem — legacy item lookup UX: 1 hit → embed; 2–25 → button picker;
// 26–40 → plain list; >40 → refine. Timeouts + cache live in items.rs.
// ===========================================================================

pub fn item_embed(item: &nocturnal_core::Item, color: u32) -> serenity::CreateEmbed {
    // The legacy 56-dash rule wrapped onto a second line on a phone and cost
    // more height than the stats under it. A slim divider does the same job.
    let separator = "───────────────\n";
    let mut embed = serenity::CreateEmbed::new()
        .color(color)
        .title(format!("{} #{}", item.name, item.id))
        .description(format!("{separator}{}", item.data.as_deref().unwrap_or("")));
    if let Some(url) = &item.url {
        embed = embed.url(url.clone());
    }
    if let Some(image) = &item.image {
        embed = embed.thumbnail(image.clone());
    }
    embed
}

/// Search an item in the Quarm/TAKP databases.
#[tracing::instrument(name = "command.searchitem", skip_all, fields(otel.kind = "server"))]
#[poise::command(slash_command, rename = "searchitem")]
pub async fn searchitem(
    ctx: Context<'_>,
    #[description = "Item name or id"] search: String,
    #[description = "quarm | takp (default quarm)"] database: Option<String>,
) -> Result<(), Error> {
    crate::discord::ack(&ctx).await?;
    let Some(db) = crate::items::Database::parse(database.as_deref().unwrap_or("quarm")) else {
        ctx.say("Invalid database option. Must be quarm or takp")
            .await?;
        return Ok(());
    };
    let outcome = match ctx.data().items.search(&search, db).await {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "item search failed");
            ctx.say(format!(":no_entry: Item lookup failed: {e}"))
                .await?;
            return Ok(());
        }
    };
    match outcome {
        crate::items::SearchOutcome::None => {
            ctx.say("No items found").await?;
        }
        crate::items::SearchOutcome::One(item) => {
            ctx.send(poise::CreateReply::default().embed(item_embed(&item, EMBED_BLUE)))
                .await?;
        }
        crate::items::SearchOutcome::Many(refs) if refs.len() > 40 => {
            ctx.say(format!("List too long ({}), refine search", refs.len()))
                .await?;
        }
        crate::items::SearchOutcome::Many(refs) if refs.len() > 25 => {
            let listing = refs
                .iter()
                .map(|r| {
                    format!(
                        "#{:<10} {}{}",
                        r.id,
                        r.name,
                        r.kind
                            .as_deref()
                            .map(|k| format!(" - {k}"))
                            .unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let embed = serenity::CreateEmbed::new()
                .title("Search Results")
                .description(listing);
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
        }
        crate::items::SearchOutcome::Many(refs) => {
            // Button picker, one per hit (rows of 5, 30 s window).
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
                        .components(rows),
                )
                .await?;
            let press = serenity::collector::ComponentInteractionCollector::new(ctx)
                .filter(move |press| press.data.custom_id.starts_with(&ctx_id.to_string()))
                .timeout(std::time::Duration::from_secs(30))
                .await;
            let Some(press) = press else {
                msg.edit(
                    ctx,
                    poise::CreateReply::default()
                        .content("Time out")
                        .components(vec![]),
                )
                .await?;
                return Ok(());
            };
            let id = press.data.custom_id[format!("{ctx_id}item").len()..].to_owned();
            crate::discord::ack_component(ctx.serenity_context(), &press).await?;
            match ctx.data().items.by_id(&id, db).await {
                Ok(Some(item)) => {
                    msg.edit(
                        ctx,
                        poise::CreateReply::default()
                            .content("")
                            .embed(item_embed(&item, EMBED_BLUE))
                            .components(vec![]),
                    )
                    .await?;
                }
                Ok(None) => {
                    msg.edit(
                        ctx,
                        poise::CreateReply::default()
                            .content("No items found")
                            .components(vec![]),
                    )
                    .await?;
                }
                Err(e) => {
                    msg.edit(
                        ctx,
                        poise::CreateReply::default()
                            .content(format!(":no_entry: Item lookup failed: {e}"))
                            .components(vec![]),
                    )
                    .await?;
                }
            }
        }
    }
    Ok(())
}

// ===========================================================================
// RaidHelper awards: signups who actually attended get DKP when a linked raid
// ends, or when an officer runs it by hand for a past raid.
// ===========================================================================

fn mention_chunks(
    label: &str,
    players: &[nocturnal_core::PlayerId],
) -> Vec<(String, String, bool)> {
    if players.is_empty() {
        return vec![(label.to_owned(), "-".to_owned(), true)];
    }
    players
        .chunks(10)
        .enumerate()
        .map(|(i, chunk)| {
            (
                if i == 0 {
                    format!("{label} ({})", players.len())
                } else {
                    "\u{200b}".to_owned()
                },
                chunk
                    .iter()
                    .map(|p| format!("- <@{p}>"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                true,
            )
        })
        .collect()
}

/// Award the event DKP for `raid_id` and post the legacy-style report.
/// Never fatal: a RaidHelper outage must not break ending a raid.
async fn award_raidhelper_event(
    ctx: &Context<'_>,
    raid_id: &str,
    event_id: &str,
    dkp: i64,
) -> anyhow::Result<String> {
    let ledger_guild = require_guild(ctx)?;
    let event = crate::raidhelper::fetch_event(event_id).await?;
    let rid = raid_id.to_owned();
    let raid = ctx
        .data()
        .driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .and_then(|g| g.raids.get(&rid).cloned())
        })
        .await
        .context("raid not found")?;

    let award = crate::raidhelper::decide_award(&raid, &event.signups);
    for player in &award.rewarded {
        if dkp > 0 {
            let _ = ctx
                .data()
                .driver
                .execute(
                    ledger_guild,
                    Actor::User(ctx.author().id.get()),
                    Command::AdjustDkp {
                        player: *player,
                        delta: dkp,
                        comment: "Subscribed and attended raid event".into(),
                        item: None,
                    },
                )
                .await;
        }
    }

    let mut embed = serenity::CreateEmbed::new()
        .color(EMBED_ORANGE)
        .title(format!("Raid Event DKP - {}", event.title))
        .description(format!(
            "Adding {dkp} DKP to players that subscribed and attended at least {} tick(s)",
            award.required
        ));
    for (name, value, inline) in mention_chunks("Rewarded", &award.rewarded) {
        embed = embed.field(name, value, inline);
    }
    let short: Vec<nocturnal_core::PlayerId> = award
        .not_enough_attendance
        .iter()
        .map(|(p, _)| *p)
        .collect();
    for (name, value, inline) in mention_chunks("NOT enough attendance", &short) {
        embed = embed.field(name, value, inline);
    }
    for (name, value, inline) in mention_chunks("NOT subscribed", &award.attended_unsigned) {
        embed = embed.field(name, value, inline);
    }
    for (name, value, inline) in mention_chunks("NOT attended", &award.signed_up_absent) {
        embed = embed.field(name, value, inline);
    }
    send_log_embed(ctx, embed).await;
    Ok(format!(
        "{} rewarded, {} short of attendance, {} signed up but absent",
        award.rewarded.len(),
        award.not_enough_attendance.len(),
        award.signed_up_absent.len()
    ))
}

/// Add DKP to raid attendants that subscribed to a RaidHelper event.
#[tracing::instrument(name = "command.addraideventdkp", skip_all, fields(otel.kind = "server"))]
#[poise::command(
    slash_command,
    ephemeral,
    rename = "addraideventdkp",
    check = "officer_check"
)]
pub async fn addraideventdkp(
    ctx: Context<'_>,
    #[description = "The amount of DKP to add"]
    #[min = 0]
    dkp: i64,
    #[description = "Raid ID"] raidid: String,
    #[description = "Event ID"] eventid: String,
) -> Result<(), Error> {
    crate::discord::ack_ephemeral(&ctx).await?;
    match award_raidhelper_event(&ctx, &raidid, &eventid, dkp).await {
        Ok(summary) => {
            ctx.say(format!("Raid event DKP applied — {summary}"))
                .await?
        }
        Err(e) => ctx.say(format!(":no_entry: {e}")).await?,
    };
    Ok(())
}

/// Ring the auction bell now, to check voice permissions after a deploy.
#[tracing::instrument(name = "command.belltest", skip_all, fields(otel.kind = "server"))]
#[poise::command(slash_command, ephemeral, rename = "belltest", check = "officer_check")]
pub async fn belltest(
    ctx: Context<'_>,
    #[description = "Voice channel (defaults to the configured raid channel)"]
    #[channel_types("Voice")]
    channel: Option<serenity::GuildChannel>,
) -> Result<(), Error> {
    let ledger_guild = require_guild(&ctx)?;
    crate::discord::ack_ephemeral(&ctx).await?;
    let configured = ctx
        .data()
        .driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .and_then(|g| g.config.raid_channel)
        })
        .await;
    let Some(target) = channel.map(|c| c.id.get()).or(configured) else {
        ctx.say(":no_entry: No voice channel given and no raid channel configured")
            .await?;
        return Ok(());
    };
    let discord_guild = ctx.guild_id().map(|g| g.get()).unwrap_or_default();

    // Say what the bot may actually do there, since a silent bell is almost
    // always a missing Connect or Speak.
    let permissions = ctx
        .serenity_context()
        .cache
        .guild(serenity::GuildId::new(discord_guild))
        .and_then(|guild| {
            let me = ctx.serenity_context().cache.current_user().id;
            let member = guild.members.get(&me)?.clone();
            let channel = guild
                .channels
                .get(&serenity::ChannelId::new(target))?
                .clone();
            Some(guild.user_permissions_in(&channel, &member))
        });
    let (can_connect, can_speak) = permissions
        .map(|p| (p.connect(), p.speak()))
        .unwrap_or((false, false));

    crate::bell::ring(
        ctx.serenity_context(),
        discord_guild,
        vec![target],
        ctx.data().bell.path.clone(),
    );
    ctx.say(format!(
        "Ringing the bell in <#{target}> — Connect: {}, Speak: {}. \
         If you hear nothing, the journal says why.",
        if can_connect { "yes" } else { "**no**" },
        if can_speak { "yes" } else { "**no**" }
    ))
    .await?;
    Ok(())
}

#[cfg(test)]
mod ack_tests {
    use super::{snowflake_ms, DISCORD_EPOCH_MS};

    /// The acknowledgment histogram is only as good as this arithmetic, and a
    /// wrong shift or epoch fails silently — it still yields a plausible-looking
    /// number of seconds, just against the wrong origin.
    #[test]
    fn snowflakes_decode_to_their_creation_instant() {
        // The lowest id whose timestamp field is 1ms past Discord's epoch.
        assert_eq!(snowflake_ms(1 << 22), DISCORD_EPOCH_MS + 1);
        // A real id: the test guild, created 2026-08-19.
        assert_eq!(snowflake_ms(1_540_111_927_995_539_506), 1_787_261_697_530);
    }

    /// The 22-bit shift discards the worker/process/increment fields, so ids
    /// minted in the same millisecond must decode identically — otherwise a
    /// bid storm's clicks would each get a slightly different baseline.
    #[test]
    fn the_low_bits_do_not_reach_the_timestamp() {
        let base = 1_540_111_927_995_539_506u64 & !((1 << 22) - 1);
        assert_eq!(snowflake_ms(base), snowflake_ms(base | 0x3F_FFFF));
    }
}
