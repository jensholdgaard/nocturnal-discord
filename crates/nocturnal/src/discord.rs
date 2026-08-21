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
}

type Error = anyhow::Error;
type Context<'a> = poise::Context<'a, Data, Error>;

const EMBED_BLUE: u32 = 0x0099ff;

fn ts_sec(ms: i64) -> i64 {
    ms / 1000
}

fn require_guild(ctx: &Context<'_>) -> anyhow::Result<u64> {
    ctx.guild_id()
        .map(|g| g.get())
        .context("This command can only be used in a discord server")
}

/// Shows the DKP of a player.
#[poise::command(slash_command, ephemeral)]
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
    let options = poise::FrameworkOptions {
        commands: vec![playerdkp(), dkphistory(), listplayersdkps(), searchlogs()],
        on_error: |error| Box::pin(on_error(error)),
        ..Default::default()
    };
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
                Ok(Data { driver })
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
