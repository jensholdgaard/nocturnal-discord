//! Raid tick scheduler. Timers are state, not callbacks (hazard B6): every
//! cycle it *proposes* a tick and lets the ledger's decide step judge
//! due-ness — a rejected `TickTooSoon` is the normal quiet case, so missed
//! cycles, restarts, and double-fires are all self-correcting.

use nocturnal_telemetry::attr;
use std::time::Duration;

use nocturnal_core::event::Flavor;
use nocturnal_core::state::AuctionStatus;
use nocturnal_core::{Actor, Command, GuildId};
use poise::serenity_prelude as serenity;

use crate::discord::{raid_embed, voice_members, EMBED_BLUE_TICK};
use crate::driver::{DriverHandle, ExecError};

fn now_ms() -> i64 {
    crate::discord::chrono_now_ms()
}

const CYCLE: Duration = Duration::from_secs(10);

/// How late a derived timer actually fired.
///
/// Timers here are state, not callbacks (hazard B6): a cycle notices that
/// something became due and proposes the command. Drift is therefore the
/// honest saturation signal for the whole path — a busy writer, a slow Discord
/// call, or a stalled cycle all show up here before commit latency moves,
/// because the delay lands *between* cycles rather than inside one.
///
/// One `CYCLE` of drift is the floor and entirely normal; sustained drift far
/// above it means cycles are not keeping up.
fn record_drift(timer: &'static str, due_ms: i64) {
    let late = (now_ms() - due_ms) as f64 / 1000.0;
    if late < 0.0 {
        return; // clock stepped backwards; not a real sample
    }
    nocturnal_telemetry::metrics().scheduler_drift.record(
        late,
        &[opentelemetry::KeyValue::new(
            nocturnal_telemetry::attr::NOCTURNAL_SCHEDULER_TIMER,
            timer,
        )],
    );
}

pub struct Scheduler {
    pub ctx: serenity::Context,
    pub driver: DriverHandle,
    pub auctions: std::sync::Arc<crate::auctions::AuctionUi>,
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
            tracing::warn!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "scheduler cycle error");
        }
        if let Err(e) = auction_cycle(&s).await {
            tracing::warn!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "auction cycle error");
        }
    }
}

#[tracing::instrument(name = "scheduler.cycle", skip_all, fields(otel.kind = "internal"))]
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
                    // When this tick became due, by the same rule `decide`
                    // applies: one interval after the last attendance entry.
                    raid.entries.last().map(|e| e.ts_ms + raid.tick_interval_ms),
                ))
            })
        })
        .await;
    let Some((name, dkp_per_tick, raid_channel, second_channel, log_channel, due_ms)) = raid else {
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
            if let Some(due_ms) = due_ms {
                record_drift("raid_tick", due_ms);
            }
            tracing::info!({ attr::NOCTURNAL_RAID_TICK_PLAYER_COUNT } = players.len(),
                { attr::NOCTURNAL_RAID_NAME } = %name,
                "raid tick awarded");
            if let Some(log_channel) = log_channel {
                let embed = raid_embed(
                    EMBED_BLUE_TICK,
                    &format!("{name} raid *tick*"),
                    &players,
                    dkp_per_tick,
                );
                let _ = crate::discord::discord_call("send tick embed", async {
                    serenity::ChannelId::new(log_channel)
                        .send_message(&s.ctx.http, serenity::CreateMessage::new().embed(embed))
                        .await
                })
                .await
                .map_err(
                    |e| tracing::warn!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "tick embed failed"),
                );
            }
        }
        Err(ExecError::Rejected(_)) => { /* not due yet — the normal case */ }
        Err(e @ ExecError::Storage(_)) => return Err(anyhow::anyhow!(e.to_string())),
    }
    Ok(())
}

/// Auction timers, derived from ledger state (hazard B6): an auction past its
/// deadline closes; a *long* auction closed longer than the legacy grace
/// period finalizes (which is the debit). Both are idempotent — a rejected
/// command just means another cycle already did it, and a restart mid-auction
/// simply resumes here.
#[tracing::instrument(name = "scheduler.auctions", skip_all, fields(otel.kind = "internal"))]
async fn auction_cycle(s: &Scheduler) -> anyhow::Result<()> {
    let ledger_guild = s.ledger_guild;
    let now = now_ms();
    let due = s
        .driver
        .query(move |l| {
            let Some(g) = l.state().guild(ledger_guild) else {
                return Vec::new();
            };
            g.auctions
                .iter()
                .filter_map(|(id, a)| match a.status {
                    AuctionStatus::Open if a.deadline_ts_ms <= now => {
                        Some((id.clone(), a.flavor, false, a.deadline_ts_ms))
                    }
                    AuctionStatus::Closed
                        if a.flavor == Flavor::Long
                            && a.deadline_ts_ms + crate::auctions::LONG_AUCTION_GRACE_MS <= now =>
                    {
                        Some((
                            id.clone(),
                            a.flavor,
                            true,
                            a.deadline_ts_ms + crate::auctions::LONG_AUCTION_GRACE_MS,
                        ))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .await;

    for (auction_id, flavor, finalize, due_ms) in due {
        let cmd = if finalize {
            Command::FinalizeAuction {
                auction_id: auction_id.clone(),
                // Recorded in the event, so any tie-break draw is reproducible.
                seed: now as u64,
            }
        } else {
            Command::CloseAuction {
                auction_id: auction_id.clone(),
            }
        };
        match s.driver.execute(ledger_guild, Actor::System, cmd).await {
            Ok(_) => {
                record_drift("auction", due_ms);
                tracing::info!(
                    { attr::NOCTURNAL_AUCTION_ID } = auction_id,
                    { attr::NOCTURNAL_AUCTION_FLAVOR } = ?flavor,
                    { attr::NOCTURNAL_AUCTION_TIMER_ACTION } =
                        if finalize { "finalized" } else { "closed" },
                    "auction timer fired"
                );
            }
            // Another cycle got there first, or an officer already acted.
            Err(crate::driver::ExecError::Rejected(_)) => continue,
            Err(e) => return Err(anyhow::anyhow!(e.to_string())),
        }
        crate::auctions::refresh(
            s.ctx.http.as_ref(),
            &s.auctions,
            &s.driver,
            ledger_guild,
            &auction_id,
        )
        .await;
    }
    Ok(())
}
