//! Discord layer (M3, read-only commands): serenity/poise, defer-first,
//! errors contained, command registration scoped to the test guild.
//! Embed formats port the legacy bot's output so officers see what they know.

use std::time::Duration;

use anyhow::Context as _;
use poise::serenity_prelude as serenity;

use nocturnal_core::state::LogEntry;
use nocturnal_core::PlayerId;

use crate::config::Config;
use crate::driver::DriverHandle;
use crate::health::Readiness;

pub struct Data {
    pub driver: DriverHandle,
    /// Test-server mapping: serve this ledger guild for interactions from the
    /// registration guild (see `discord.data_guild_id`).
    pub data_guild: Option<(u64, u64)>,
}

type Error = anyhow::Error;
type Context<'a> = poise::Context<'a, Data, Error>;

const EMBED_BLUE: u32 = 0x0099ff;

fn ts_sec(ms: i64) -> i64 {
    ms / 1000
}

/// The ledger guild for this interaction: normally the Discord guild itself,
/// remapped when a test server serves imported production data.
fn require_guild(ctx: &Context<'_>) -> anyhow::Result<u64> {
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
#[tracing::instrument(name = "command.playerdkp", skip_all)]
pub async fn playerdkp(
    ctx: Context<'_>,
    #[description = "The player"] player: Option<serenity::User>,
) -> Result<(), Error> {
    let guild = require_guild(&ctx)?;
    ctx.defer_ephemeral().await?;
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
#[tracing::instrument(name = "command.dkphistory", skip_all)]
pub async fn dkphistory(
    ctx: Context<'_>,
    #[description = "The player"] player: Option<serenity::User>,
) -> Result<(), Error> {
    let guild = require_guild(&ctx)?;
    ctx.defer_ephemeral().await?;
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
#[tracing::instrument(name = "command.listplayersdkps", skip_all)]
pub async fn listplayersdkps(ctx: Context<'_>) -> Result<(), Error> {
    let guild = require_guild(&ctx)?;
    ctx.defer_ephemeral().await?;
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
            let cutoff = now_ms - g.config.raid_deprecation_ms;
            let mut rows: Vec<Row> = g
                .players
                .iter()
                .filter(|(_, p)| p.log.last().is_some_and(|e| e.ts_ms >= cutoff))
                .map(|(id, p)| Row {
                    player: *id,
                    current: p.balance,
                    attendance: g.attendance_pct(*id, now_ms),
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
#[tracing::instrument(name = "command.searchlogs", skip_all)]
pub async fn searchlogs(
    ctx: Context<'_>,
    #[description = "Search term"] search: String,
) -> Result<(), Error> {
    let guild = require_guild(&ctx)?;
    ctx.defer_ephemeral().await?;
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

fn chrono_now_ms() -> i64 {
    #[allow(clippy::expect_used)]
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after 1970");
    d.as_millis() as i64
}

async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
    match error {
        poise::FrameworkError::Command { error, ctx, .. } => {
            tracing::warn!(command = ctx.command().name, error = %error, "command error");
            let _ = ctx.say(format!(":no_entry: {error}")).await;
        }
        other => {
            if let Err(e) = poise::builtins::on_error(other).await {
                tracing::error!(error = %e, "error handler failed");
            }
        }
    }
}

/// Connect the gateway, register commands in the configured test guild, and
/// run until shutdown.
pub async fn run(cfg: &Config, driver: DriverHandle, readiness: Readiness) -> anyhow::Result<()> {
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
        adddkp(),
        removedkp(),
        addraiddkp(),
        parsedkps(),
        registercharacter(),
    ];
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
        ..Default::default()
    };
    let data_guild = cfg.discord.data_guild_id.map(|to| (guild_id, to));
    if let Some((from, to)) = data_guild {
        tracing::info!(
            from,
            to,
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
                tracing::info!(user = %ready.user.name, guild_id, "gateway ready, commands registered");
                readiness.set_ready();
                tokio::spawn(crate::scheduler::run(crate::scheduler::Scheduler {
                    ctx: ctx.clone(),
                    driver: driver.clone(),
                    discord_guild: guild_id,
                    ledger_guild: data_guild.map_or(guild_id, |(_, to)| to),
                }));
                Ok(Data { driver, data_guild })
            })
        })
        .build();
    let mut client =
        serenity::ClientBuilder::new(&token, serenity::GatewayIntents::non_privileged())
            .framework(framework)
            .await
            .context("building Discord client")?;

    let shard_manager = client.shard_manager.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("shutdown signal, closing gateway");
        shard_manager.shutdown_all().await;
    });

    client.start().await.context("gateway run")?;
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
        if let Err(e) = serenity::ChannelId::new(channel)
            .send_message(
                ctx.serenity_context(),
                serenity::CreateMessage::new().embed(embed),
            )
            .await
        {
            tracing::warn!(error = %e, "log channel embed failed");
        }
    }
}

/// Legacy `restricted` gate: guild Administrators bypass; otherwise the
/// member needs the configured officer role.
async fn officer_check(ctx: Context<'_>) -> Result<bool, Error> {
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

async fn execute(
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

fn rejection_text(e: &ExecError) -> String {
    match e {
        ExecError::Rejected(r) => match r {
            nocturnal_core::Rejection::RaidAlreadyActive { name } => {
                format!(":no_entry: There is already an active raid: {name}")
            }
            nocturnal_core::Rejection::NoActiveRaid => {
                ":no_entry: There is no active raid, use /startraid to start one first".into()
            }
            nocturnal_core::Rejection::InsufficientBalance { balance, .. } => {
                format!(":no_entry: DKP Bot scowls at you. Not enough DKP (current: {balance})")
            }
            nocturnal_core::Rejection::CharacterAlreadyRegistered { character } => {
                format!(":no_entry: Character {character} already registered")
            }
            nocturnal_core::Rejection::CharacterNotRegistered { character } => {
                format!(":no_entry: Character {character} not registered")
            }
            nocturnal_core::Rejection::InvalidAmount => {
                ":no_entry: DKP Bot scowls at you. Invalid amount".into()
            }
            other => format!(":no_entry: {other}"),
        },
        ExecError::Storage(_) => {
            ":no_entry: Storage failure — the command was NOT applied. Check the logs.".into()
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
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    if raidchannel.id == secondraidchannel.as_ref().map(|c| c.id).unwrap_or_default() {
        ctx.say(":no_entry: Raid channel and second raid channel must be different")
            .await?;
        return Ok(());
    }
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
        raidhelper_api_key: None,
    };
    match execute(&ctx, Command::UpdateConfig { patch }).await? {
        Ok(_) => ctx.say("Configuration saved").await?,
        Err(e) => ctx.say(rejection_text(&e)).await?,
    };
    Ok(())
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
    ctx.defer_ephemeral().await?;
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
        .field(
            "Tick duration",
            format!("{} minutes", cfg.tick_duration_ms / 60_000),
            false,
        )
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
        );
    ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}

/// Create a new raid.
#[tracing::instrument(name = "command.startraid", skip_all)]
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
    ctx.defer_ephemeral().await?;
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
    let name = name.unwrap_or_else(|| format!("<t:{}:D>", chrono_now_ms() / 1000));
    let raid_id = format!("rd-{:x}", chrono_now_ms());
    let cmd = Command::StartRaid {
        raid_id,
        name: name.clone(),
        tick_interval_ms,
        dkp_per_tick,
        players_present: players.clone(),
        event_id: None,
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
#[tracing::instrument(name = "command.endraid", skip_all)]
#[poise::command(slash_command, ephemeral, rename = "endraid", check = "officer_check")]
pub async fn endraid(ctx: Context<'_>) -> Result<(), Error> {
    let ledger_guild = require_guild(&ctx)?;
    let discord_guild = ctx.guild_id().map(|g| g.get()).unwrap_or_default();
    ctx.defer_ephemeral().await?;
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
            let mut moves: Vec<Movement> = Vec::new();
            let mut agg: Option<(String, i64, i64)> = None; // comment, dkps, ts
            for e in &raid.entries {
                match &mut agg {
                    Some((comment, dkps, _)) if *comment == e.comment => *dkps += e.amount,
                    _ => {
                        if let Some((comment, dkps, ts)) = agg.take() {
                            moves.push(Movement {
                                ts_ms: ts,
                                text: format!("<t:{}:t> *{comment}* ({dkps} dkps)", ts / 1000),
                            });
                        }
                        agg = Some((e.comment.clone(), e.amount, e.ts_ms));
                    }
                }
            }
            if let Some((comment, dkps, ts)) = agg {
                moves.push(Movement {
                    ts_ms: ts,
                    text: format!("<t:{}:t> *{comment}* ({dkps} dkps)", ts / 1000),
                });
            }
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
#[tracing::instrument(name = "command.adddkp", skip_all)]
#[poise::command(slash_command, rename = "adddkp", check = "officer_check")]
pub async fn adddkp(
    ctx: Context<'_>,
    #[description = "The player"] player: serenity::User,
    #[description = "The amount of DKP to add"]
    #[min = 1]
    dkp: i64,
    #[description = "Reason"] comment: String,
) -> Result<(), Error> {
    ctx.defer().await?;
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
#[tracing::instrument(name = "command.removedkp", skip_all)]
#[poise::command(slash_command, rename = "removedkp", check = "officer_check")]
pub async fn removedkp(
    ctx: Context<'_>,
    #[description = "The player"] player: serenity::User,
    #[description = "The amount of DKP to remove"]
    #[min = 1]
    dkp: i64,
    #[description = "Reason"] comment: String,
) -> Result<(), Error> {
    ctx.defer().await?;
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
#[tracing::instrument(name = "command.addraiddkp", skip_all)]
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
    ctx.defer_ephemeral().await?;
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

/// Parse an EQ /who log and award DKP by character.
#[tracing::instrument(name = "command.parsedkps", skip_all)]
#[poise::command(slash_command, rename = "parsedkps", check = "officer_check")]
pub async fn parsedkps(
    ctx: Context<'_>,
    #[description = "Comment"] comment: String,
    #[description = "The amount of dkps"]
    #[min = 1]
    dkps: i64,
    #[description = "Is this a raid?"] raid: bool,
    #[description = "The /who log to parse"] log: String,
) -> Result<(), Error> {
    let _ = raid; // the active raid attaches automatically (fixes audit E10)
    ctx.defer().await?;
    let parsed = nocturnal_core::who::parse_who(&log);
    let mut errors: Vec<String> = Vec::new();
    for character in &parsed.characters {
        let cmd = Command::AdjustByCharacter {
            character: character.clone(),
            delta: dkps,
            comment: comment.clone(),
        };
        if let Err(e) = execute(&ctx, cmd).await? {
            errors.push(match e {
                ExecError::Rejected(nocturnal_core::Rejection::CharacterNotRegistered {
                    character,
                }) => {
                    format!("Character {character} not registered")
                }
                other => other.to_string(),
            });
        }
    }
    let mut characters = parsed.characters.clone();
    characters.sort();
    let embed = serenity::CreateEmbed::new()
        .color(EMBED_BLUE_TICK)
        .title(comment)
        .field("DKPS", dkps.to_string(), false)
        .field(
            "Characters",
            if characters.is_empty() {
                "-".into()
            } else {
                characters.join("\n")
            },
            false,
        )
        .field(
            "errors",
            if errors.is_empty() {
                "-".into()
            } else {
                errors.join("\n")
            },
            false,
        );
    if let Err(e) = ctx
        .channel_id()
        .send_message(
            ctx.serenity_context(),
            serenity::CreateMessage::new().embed(embed),
        )
        .await
    {
        tracing::warn!(error = %e, "parsedkps embed failed");
    }
    ctx.say(format!("Parsed {} characters", parsed.characters.len()))
        .await?;
    Ok(())
}

/// Register an EQ character to your Discord account.
#[tracing::instrument(name = "command.registercharacter", skip_all)]
#[poise::command(slash_command, rename = "registercharacter")]
pub async fn registercharacter(
    ctx: Context<'_>,
    #[description = "The character name"] name: String,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
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
