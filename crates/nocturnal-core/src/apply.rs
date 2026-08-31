//! The apply step: fold one event into the projections. Total and infallible —
//! every historical `(kind, v)` must apply forever, and replaying the same
//! log always yields the same state (pinned by tests).

use crate::event::{Envelope, Event};
use crate::state::{
    AttendanceEntry, Auction, AuctionStatus, Bid, LogEntry, Player, Raid, State, TokenGrant,
};

pub fn apply(state: &mut State, env: &Envelope) {
    let g = state.guild_mut(env.guild);
    let ts = env.ts_ms;

    match &env.event {
        Event::CharacterLinked { player, character } => {
            let p = g.players.entry(*player).or_insert_with(|| new_player(ts));
            p.characters.push(character.clone());
            g.characters.insert(character.to_lowercase(), *player);
        }

        Event::PlayerImported {
            player,
            balance,
            characters,
            creation_ts_ms,
            log,
            legacy_id,
        } => {
            let p = Player {
                balance: *balance,
                characters: characters.clone(),
                creation_ts_ms: *creation_ts_ms,
                legacy_id: legacy_id.clone(),
                log: log
                    .iter()
                    .map(|e| LogEntry {
                        dkp: e.dkp,
                        comment: e.comment.clone(),
                        ts_ms: e.ts_ms,
                        raid: e.raid.clone(),
                        item: e.item.clone(),
                    })
                    .collect(),
            };
            for c in characters {
                g.characters.insert(c.to_lowercase(), *player);
            }
            g.players.insert(*player, p);
        }

        Event::DkpAdjusted {
            player,
            delta,
            comment,
            raid,
            item,
        } => {
            let p = g.players.entry(*player).or_insert_with(|| new_player(ts));
            p.balance += delta;
            p.log.push(LogEntry {
                dkp: *delta,
                comment: comment.clone(),
                ts_ms: ts,
                raid: raid.clone(),
                item: item.clone(),
            });
        }

        Event::RaidStarted {
            raid_id,
            name,
            tick_interval_ms,
            dkp_per_tick,
            event_id,
        } => {
            g.raids.insert(
                raid_id.clone(),
                Raid {
                    name: name.clone(),
                    date_ms: ts,
                    tick_interval_ms: *tick_interval_ms,
                    dkp_per_tick: *dkp_per_tick,
                    active: true,
                    tick_no: 0,
                    event_id: event_id.clone(),
                    entries: Vec::new(),
                },
            );
            g.active_raid = Some(raid_id.clone());
        }

        Event::RaidAwarded {
            raid_id,
            players,
            amount,
            comment,
        } => {
            award(g, raid_id, players, *amount, comment, ts, None);
        }

        Event::RaidTicked {
            raid_id,
            tick_no,
            players,
            amount,
        } => {
            award(g, raid_id, players, *amount, "Tick", ts, Some(*tick_no));
        }

        Event::RaidEnded { raid_id, .. } => {
            if let Some(r) = g.raids.get_mut(raid_id) {
                r.active = false;
            }
            if g.active_raid.as_deref() == Some(raid_id) {
                g.active_raid = None;
            }
        }

        Event::RaidImported {
            raid_id,
            name,
            date_ms,
            entries,
            tick_interval_ms,
            dkp_per_tick,
            event_id,
        } => {
            g.raids.insert(
                raid_id.clone(),
                Raid {
                    name: name.clone(),
                    date_ms: *date_ms,
                    tick_interval_ms: *tick_interval_ms,
                    dkp_per_tick: *dkp_per_tick,
                    active: false,
                    tick_no: entries.len() as u32,
                    event_id: event_id.clone(),
                    entries: entries
                        .iter()
                        .map(|e| AttendanceEntry {
                            players: e.players.clone(),
                            comment: e.comment.clone(),
                            ts_ms: e.ts_ms,
                            amount: e.amount,
                        })
                        .collect(),
                },
            );
        }

        Event::AuctionOpened {
            auction_id,
            item,
            flavor,
            min_bid,
            num_items,
            min_bid_to_lock_for_main,
            over_bid_to_win_main,
            deadline_ts_ms,
        } => {
            g.auctions.insert(
                auction_id.clone(),
                Auction {
                    item: item.clone(),
                    flavor: *flavor,
                    min_bid: *min_bid,
                    num_items: *num_items,
                    min_bid_to_lock_for_main: *min_bid_to_lock_for_main,
                    over_bid_to_win_main: *over_bid_to_win_main,
                    deadline_ts_ms: *deadline_ts_ms,
                    status: AuctionStatus::Open,
                    bids: Vec::new(),
                    winners: Vec::new(),
                    cancelled_by: None,
                    cancelled_ts_ms: None,
                },
            );
        }

        Event::BidPlaced {
            auction_id,
            player,
            amount,
            for_main,
            attendance,
        } => {
            if let Some(a) = g.auctions.get_mut(auction_id) {
                // Re-bid replaces (legacy semantics).
                if let Some(b) = a.bids.iter_mut().find(|b| b.player == *player) {
                    b.amount = *amount;
                    b.for_main = *for_main;
                    b.attendance = *attendance;
                } else {
                    a.bids.push(Bid {
                        player: *player,
                        amount: *amount,
                        for_main: *for_main,
                        attendance: *attendance,
                    });
                }
            }
        }

        Event::BidRetracted { auction_id, player } => {
            if let Some(a) = g.auctions.get_mut(auction_id) {
                a.bids.retain(|b| b.player != *player);
            }
        }

        Event::AuctionClosed {
            auction_id,
            ended_ts_ms,
        } => {
            if let Some(a) = g.auctions.get_mut(auction_id) {
                a.status = AuctionStatus::Closed;
                // An officer stopped it early: the deadline *is* that moment,
                // so every embed and every later read reports when bidding
                // actually stopped.
                if let Some(ended) = ended_ts_ms {
                    a.deadline_ts_ms = *ended;
                }
            }
        }

        Event::AuctionFinalized {
            auction_id,
            winners,
            ..
        } => {
            let item = g.auctions.get(auction_id).map(|a| a.item.clone());
            // Attribute the loot to the raid it was won in, exactly like the
            // legacy `removeDKP(..., raid, item)` call. Without this the raid
            // summary and /dkphistory cannot say who won what.
            let raid_ref = g.active_raid.as_ref().and_then(|id| {
                g.raids.get(id).map(|r| crate::event::RaidRef {
                    raid_id: id.clone(),
                    name: r.name.clone(),
                })
            });
            if let Some(a) = g.auctions.get_mut(auction_id) {
                a.status = AuctionStatus::Finalized;
                a.winners = winners.clone();
            }
            // The debit lives in this fold step: a finalized winner is always
            // charged, atomically with the announcement fact (audit E2/#46).
            for w in winners {
                let p = g.players.entry(w.player).or_insert_with(|| new_player(ts));
                p.balance -= w.amount;
                p.log.push(LogEntry {
                    dkp: -w.amount,
                    comment: item.as_ref().map_or_else(String::new, |i| i.name.clone()),
                    ts_ms: ts,
                    raid: raid_ref.clone(),
                    item: item.clone(),
                });
            }
        }

        Event::AuctionCancelled { auction_id, .. } => {
            if let Some(a) = g.auctions.get_mut(auction_id) {
                a.status = AuctionStatus::Cancelled;
                // Who pulled it, and when. Taken from the envelope rather
                // than the payload: every event already carries both, and a
                // cancelled auction is the one an officer gets asked about.
                a.cancelled_by = match env.actor {
                    crate::event::Actor::User(id) => Some(id),
                    crate::event::Actor::System => None,
                };
                a.cancelled_ts_ms = Some(ts);
            }
        }

        Event::RosterCharacterSet { player, character } => {
            g.roster
                .entry(*player)
                .or_default()
                .insert(character.name.to_lowercase(), character.clone());
        }

        Event::RosterCharacterRemoved { player, name } => {
            if let Some(chars) = g.roster.get_mut(player) {
                chars.remove(&name.to_lowercase());
                if chars.is_empty() {
                    g.roster.remove(player);
                }
            }
        }

        Event::ConfigUpdated { patch } => {
            g.config.apply_patch(patch);
        }

        Event::TelemetryTokenIssued {
            username,
            token_fp,
            role,
        } => {
            g.telemetry.insert(
                username.clone(),
                TokenGrant {
                    token_fp: token_fp.clone(),
                    role: role.clone(),
                },
            );
            // Never removed on revoke: this is what lets the materializer
            // delete our line without touching a service token.
            g.telemetry_managed.insert(username.clone());
        }

        Event::TelemetryAccessUpdated { username, role } => {
            if let Some(t) = g.telemetry.get_mut(username) {
                t.role = role.clone();
            }
        }

        Event::TelemetryTokenRevoked { username } => {
            g.telemetry.remove(username);
        }
    }
}

fn new_player(ts_ms: i64) -> Player {
    Player {
        creation_ts_ms: ts_ms,
        ..Player::default()
    }
}

fn award(
    g: &mut crate::state::GuildState,
    raid_id: &str,
    players: &[crate::event::PlayerId],
    amount: i64,
    comment: &str,
    ts: i64,
    tick_no: Option<u32>,
) {
    let raid_ref = g.raids.get(raid_id).map(|r| crate::event::RaidRef {
        raid_id: raid_id.to_owned(),
        name: r.name.clone(),
    });
    if amount != 0 {
        for player in players {
            let p = g.players.entry(*player).or_insert_with(|| new_player(ts));
            p.balance += amount;
            p.log.push(LogEntry {
                dkp: amount,
                comment: comment.to_owned(),
                ts_ms: ts,
                raid: raid_ref.clone(),
                item: None,
            });
        }
    }
    if let Some(r) = g.raids.get_mut(raid_id) {
        r.entries.push(AttendanceEntry {
            players: players.to_vec(),
            comment: comment.to_owned(),
            ts_ms: ts,
            amount,
        });
        if let Some(n) = tick_no {
            r.tick_no = n;
        }
    }
}
