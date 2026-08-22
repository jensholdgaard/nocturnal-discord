//! Raid tick scheduler. Timers are state, not callbacks (hazard B6): every
//! cycle it *proposes* a tick and lets the ledger's decide step judge
//! due-ness — a rejected `TickTooSoon` is the normal quiet case, so missed
//! cycles, restarts, and double-fires are all self-correcting.

use std::time::Duration;

use nocturnal_core::{Actor, Command, GuildId};
use poise::serenity_prelude as serenity;

use crate::discord::{raid_embed, voice_members, EMBED_BLUE_TICK};
use crate::driver::{DriverHandle, ExecError};

const CYCLE: Duration = Duration::from_secs(10);

pub struct Scheduler {
    pub ctx: serenity::Context,
    pub driver: DriverHandle,
    /// Discord guild whose voice channels are scanned.
    pub discord_guild: GuildId,
    /// Ledger guild ticks are written to (differs on the test server).
    pub ledger_guild: GuildId,
}

pub async fn run(s: Scheduler) {
    let meter = opentelemetry::global::meter("nocturnal");
    let heartbeat = meter
        .u64_counter(nocturnal_telemetry::metric::NOCTURNAL_SCHEDULER_HEARTBEAT)
        .with_unit("{cycle}")
        .build();
    let mut interval = tokio::time::interval(CYCLE);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tracing::info!("raid tick scheduler running");
    loop {
        interval.tick().await;
        heartbeat.add(1, &[]);
        if let Err(e) = cycle(&s).await {
            // Side-effect failures are logged and never fatal (audit #3/#8).
            tracing::warn!(error = %e, "scheduler cycle error");
        }
    }
}

async fn cycle(s: &Scheduler) -> anyhow::Result<()> {
    let ledger_guild = s.ledger_guild;
    let raid = s
        .driver
        .query(move |l| {
            l.state().guild(ledger_guild).and_then(|g| {
                let id = g.active_raid.clone()?;
                let raid = g.raids.get(&id)?;
                Some((
                    raid.name.clone(),
                    raid.dkp_per_tick,
                    g.config.raid_channel,
                    g.config.second_raid_channel,
                    g.config.log_channel,
                ))
            })
        })
        .await;
    let Some((name, dkp_per_tick, raid_channel, second_channel, log_channel)) = raid else {
        return Ok(());
    };
    let Some(raid_channel) = raid_channel else {
        return Ok(()); // no raid channel configured; nothing to count
    };

    let mut players = voice_members(&s.ctx, s.discord_guild, raid_channel);
    if let Some(second) = second_channel {
        players.extend(voice_members(&s.ctx, s.discord_guild, second));
    }
    players.dedup();

    match s
        .driver
        .execute(
            ledger_guild,
            Actor::System,
            Command::Tick {
                players_present: players.clone(),
            },
        )
        .await
    {
        Ok(_) => {
            tracing::info!(players = players.len(), raid = %name, "raid tick awarded");
            if let Some(log_channel) = log_channel {
                let embed = raid_embed(
                    EMBED_BLUE_TICK,
                    &format!("{name} raid *tick*"),
                    &players,
                    dkp_per_tick,
                );
                let _ = serenity::ChannelId::new(log_channel)
                    .send_message(&s.ctx.http, serenity::CreateMessage::new().embed(embed))
                    .await
                    .map_err(|e| tracing::warn!(error = %e, "tick embed failed"));
            }
        }
        Err(ExecError::Rejected(_)) => { /* not due yet — the normal case */ }
        Err(e @ ExecError::Storage(_)) => return Err(anyhow::anyhow!(e.to_string())),
    }
    Ok(())
}
