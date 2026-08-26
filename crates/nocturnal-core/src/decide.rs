//! The decide step: validate a command against current projections and emit
//! the events it becomes. Pure — same inputs, same output, no I/O, no clock,
//! no RNG beyond the seed carried by the command.

use crate::auction::{winners, Rng};
use crate::command::{Command, Ctx};
use crate::event::{ConfigPatch, Event, RaidRef, Winner};
use crate::reject::Rejection;
use crate::state::{AuctionStatus, Bid, State};

pub fn decide(state: &State, ctx: &Ctx, cmd: &Command) -> Result<Vec<Event>, Rejection> {
    // Borrow, never clone: guild state carries the full imported history and
    // deep-copying it per command turned a 160-bid storm into a multi-minute
    // grind on the writer thread (found live, 2026-08-22).
    static EMPTY: std::sync::LazyLock<crate::state::GuildState> =
        std::sync::LazyLock::new(crate::state::GuildState::default);
    let g: &crate::state::GuildState = state.guild(ctx.guild).unwrap_or(&EMPTY);

    match cmd {
        Command::LinkCharacter { player, character } => {
            let key = character.to_lowercase();
            if g.characters.contains_key(&key) {
                return Err(Rejection::CharacterAlreadyRegistered {
                    character: character.clone(),
                });
            }
            Ok(vec![Event::CharacterLinked {
                player: *player,
                character: character.clone(),
            }])
        }

        Command::AdjustDkp {
            player,
            delta,
            comment,
            item,
        } => {
            if *delta == 0 {
                return Err(Rejection::InvalidAmount);
            }
            if *delta < 0 {
                let balance = g.balance(*player);
                if balance + *delta < 0 {
                    return Err(Rejection::InsufficientBalance {
                        available: balance,
                        committed: 0,
                        needed: -*delta,
                    });
                }
            }
            Ok(vec![Event::DkpAdjusted {
                player: *player,
                delta: *delta,
                comment: comment.clone(),
                raid: active_raid_ref(g),
                item: item.clone(),
            }])
        }

        Command::AdjustByCharacter {
            character,
            delta,
            comment,
        } => {
            let key = character.to_lowercase();
            let player = *g
                .characters
                .get(&key)
                .ok_or(Rejection::CharacterNotRegistered {
                    character: character.clone(),
                })?;
            decide(
                state,
                ctx,
                &Command::AdjustDkp {
                    player,
                    delta: *delta,
                    comment: comment.clone(),
                    item: None,
                },
            )
        }

        Command::StartRaid {
            raid_id,
            name,
            tick_interval_ms,
            dkp_per_tick,
            players_present,
            event_id,
        } => {
            if let Some(active_id) = &g.active_raid {
                let name = g
                    .raids
                    .get(active_id)
                    .map_or(String::new(), |r| r.name.clone());
                return Err(Rejection::RaidAlreadyActive { name });
            }
            if *dkp_per_tick < 0 || *tick_interval_ms <= 0 {
                return Err(Rejection::InvalidAmount);
            }
            let mut events = vec![Event::RaidStarted {
                raid_id: raid_id.clone(),
                name: name.clone(),
                tick_interval_ms: *tick_interval_ms,
                dkp_per_tick: *dkp_per_tick,
                event_id: event_id.clone(),
            }];
            // Legacy `/startraid` awards a starting tick's worth to everyone present.
            events.push(Event::RaidAwarded {
                raid_id: raid_id.clone(),
                players: players_present.clone(),
                amount: *dkp_per_tick,
                comment: "Start".to_owned(),
            });
            Ok(events)
        }

        Command::Tick { players_present } => {
            let raid_id = g.active_raid.clone().ok_or(Rejection::NoActiveRaid)?;
            let raid = g.raids.get(&raid_id).ok_or(Rejection::RaidNotFound)?;
            // Due when the last attendance entry is at least one interval old
            // (legacy semantics); serialized tick_no makes double ticks
            // unrepresentable (audit #35/#47).
            if let Some(last) = raid.entries.last() {
                if last.ts_ms + raid.tick_interval_ms > ctx.now_ms {
                    return Err(Rejection::TickTooSoon);
                }
            }
            Ok(vec![Event::RaidTicked {
                raid_id,
                tick_no: raid.tick_no + 1,
                players: players_present.clone(),
                amount: raid.dkp_per_tick,
            }])
        }

        Command::AwardRaid {
            players,
            amount,
            comment,
        } => {
            let raid_id = g.active_raid.clone().ok_or(Rejection::NoActiveRaid)?;
            if *amount < 0 {
                return Err(Rejection::InvalidAmount);
            }
            Ok(vec![Event::RaidAwarded {
                raid_id,
                players: players.clone(),
                amount: *amount,
                comment: comment.clone(),
            }])
        }

        Command::EndRaid {
            players_present,
            reason,
        } => {
            let raid_id = g.active_raid.clone().ok_or(Rejection::NoActiveRaid)?;
            Ok(vec![
                Event::RaidAwarded {
                    raid_id: raid_id.clone(),
                    players: players_present.clone(),
                    amount: 0,
                    comment: "End".to_owned(),
                },
                Event::RaidEnded {
                    raid_id,
                    reason: reason.clone(),
                },
            ])
        }

        Command::OpenAuction {
            auction_id,
            item,
            flavor,
            min_bid,
            num_items,
            min_bid_to_lock_for_main,
            over_bid_to_win_main,
            duration_ms,
        } => {
            if g.auctions.contains_key(auction_id) {
                return Err(Rejection::AuctionIdTaken);
            }
            if *min_bid < 0 || *duration_ms <= 0 {
                return Err(Rejection::InvalidAmount);
            }
            Ok(vec![Event::AuctionOpened {
                auction_id: auction_id.clone(),
                item: item.clone(),
                flavor: *flavor,
                min_bid: *min_bid,
                num_items: (*num_items).max(1),
                min_bid_to_lock_for_main: *min_bid_to_lock_for_main,
                over_bid_to_win_main: *over_bid_to_win_main,
                deadline_ts_ms: ctx.now_ms + *duration_ms,
            }])
        }

        Command::PlaceBid {
            auction_id,
            player,
            amount,
            for_main,
        } => {
            let auction = g
                .auctions
                .get(auction_id)
                .ok_or(Rejection::AuctionNotFound)?;
            if auction.status != AuctionStatus::Open {
                return Err(Rejection::AuctionNotActive);
            }
            if *amount <= 0 {
                return Err(Rejection::InvalidAmount);
            }
            if *amount < auction.min_bid {
                return Err(Rejection::BidBelowMinimum {
                    min_bid: auction.min_bid,
                });
            }
            let p = g.players.get(player).ok_or(Rejection::PlayerNotFound)?;
            // Cross-auction reservation: standing bids elsewhere already claim
            // part of the balance (audit #46 — the double-spend fix).
            let committed = g.committed_elsewhere(*player, auction_id);
            if *amount > p.balance - committed {
                return Err(Rejection::InsufficientBalance {
                    available: p.balance - committed,
                    committed,
                    needed: *amount,
                });
            }
            Ok(vec![Event::BidPlaced {
                auction_id: auction_id.clone(),
                player: *player,
                amount: *amount,
                for_main: *for_main,
                attendance: g.attendance_pct(*player, ctx.now_ms),
            }])
        }

        Command::RetractBid { auction_id, player } => {
            let auction = g
                .auctions
                .get(auction_id)
                .ok_or(Rejection::AuctionNotFound)?;
            if auction.status != AuctionStatus::Open {
                return Err(Rejection::AuctionNotActive);
            }
            if !auction.bids.iter().any(|b| b.player == *player) {
                return Err(Rejection::PlayerNotFound);
            }
            Ok(vec![Event::BidRetracted {
                auction_id: auction_id.clone(),
                player: *player,
            }])
        }

        Command::CloseAuction {
            auction_id,
            ended_ts_ms,
        } => {
            let auction = g
                .auctions
                .get(auction_id)
                .ok_or(Rejection::AuctionNotFound)?;
            if auction.status != AuctionStatus::Open {
                return Err(Rejection::AuctionNotActive);
            }
            Ok(vec![Event::AuctionClosed {
                auction_id: auction_id.clone(),
                ended_ts_ms: *ended_ts_ms,
            }])
        }

        Command::FinalizeAuction { auction_id, seed } => {
            let auction = g
                .auctions
                .get(auction_id)
                .ok_or(Rejection::AuctionNotFound)?;
            if auction.status != AuctionStatus::Closed {
                return Err(Rejection::AuctionNotClosed);
            }
            let winners = compute_winners(g, auction_id, *seed);
            Ok(vec![Event::AuctionFinalized {
                auction_id: auction_id.clone(),
                winners,
                seed: *seed,
            }])
        }

        Command::CancelAuction { auction_id, reason } => {
            let auction = g
                .auctions
                .get(auction_id)
                .ok_or(Rejection::AuctionNotFound)?;
            match auction.status {
                AuctionStatus::Open | AuctionStatus::Closed => Ok(vec![Event::AuctionCancelled {
                    auction_id: auction_id.clone(),
                    reason: reason.clone(),
                }]),
                _ => Err(Rejection::AuctionNotActive),
            }
        }

        Command::UpdateConfig { patch } => {
            validate_config(g, patch)?;
            Ok(vec![Event::ConfigUpdated {
                patch: patch.clone(),
            }])
        }

        Command::IssueToken {
            username,
            token_fp,
            role,
        } => {
            if g.telemetry.contains_key(username) {
                return Err(Rejection::AlreadyProvisioned {
                    username: username.clone(),
                });
            }
            Ok(vec![Event::TelemetryTokenIssued {
                username: username.clone(),
                token_fp: token_fp.clone(),
                role: role.clone(),
            }])
        }

        Command::RefreshAccess { username, role } => {
            if !g.telemetry.contains_key(username) {
                return Err(Rejection::NotProvisioned {
                    username: username.clone(),
                });
            }
            Ok(vec![Event::TelemetryAccessUpdated {
                username: username.clone(),
                role: role.clone(),
            }])
        }

        Command::RevokeToken { username } => {
            if !g.telemetry.contains_key(username) {
                return Err(Rejection::NotProvisioned {
                    username: username.clone(),
                });
            }
            Ok(vec![Event::TelemetryTokenRevoked {
                username: username.clone(),
            }])
        }

        Command::ImportPlayer {
            player,
            balance,
            characters,
            creation_ts_ms,
            log,
            legacy_id,
        } => Ok(vec![Event::PlayerImported {
            player: *player,
            balance: *balance,
            characters: characters.clone(),
            creation_ts_ms: *creation_ts_ms,
            log: log.clone(),
            legacy_id: legacy_id.clone(),
        }]),

        Command::ImportRaid {
            raid_id,
            name,
            date_ms,
            entries,
            tick_interval_ms,
            dkp_per_tick,
            event_id,
        } => Ok(vec![Event::RaidImported {
            raid_id: raid_id.clone(),
            name: name.clone(),
            date_ms: *date_ms,
            entries: entries.clone(),
            tick_interval_ms: *tick_interval_ms,
            dkp_per_tick: *dkp_per_tick,
            event_id: event_id.clone(),
        }]),
    }
}

/// Guard the values `/configure` writes.
///
/// Discord enforces some of these on the slash-command options themselves, but
/// that check only exists on the one path that happens to use it: the migrator
/// and any future caller reach the ledger directly. And an unguarded value is
/// felt somewhere else entirely — a zero tick interval is accepted happily at
/// `/configure` and only refused at `/startraid`, as `InvalidAmount`, long
/// after the officer who typed it has moved on. Refusing it where it is
/// entered names the setting instead.
///
/// Values the patch leaves absent are unchanged, so they are checked against
/// what the guild already holds rather than against a default.
fn validate_config(g: &crate::state::GuildState, p: &ConfigPatch) -> Result<(), Rejection> {
    let bad = |setting, reason: &str| Rejection::InvalidConfig {
        setting,
        reason: reason.to_owned(),
    };

    if let Some(ms) = p.tick_duration_ms {
        if ms <= 0 {
            return Err(bad("tickduration", "must be more than zero minutes"));
        }
    }
    if let Some(ms) = p.raid_deprecation_ms {
        if ms <= 0 {
            // The legacy fallback was 90 *milliseconds* (audit S9), which
            // deprecated every raid the instant it ended. Zero would be worse.
            return Err(bad("raiddeprecationtime", "must be more than zero days"));
        }
    }
    if let Some(s) = p.bid_time_s {
        if !(30..=1000).contains(&s) {
            return Err(bad("bidtime", "must be between 30 and 1000 seconds"));
        }
    }
    for (setting, value) in [
        ("minbid", p.min_bid),
        ("minbidtolockformain", p.min_bid_to_lock_for_main),
        ("overbidtowinmain", p.over_bid_to_win_main),
        ("raidhelpereventdkp", p.raidhelper_event_dkp),
    ] {
        if value.is_some_and(|v| v < 0) {
            return Err(bad(setting, "cannot be negative"));
        }
    }
    if p.raidhelper_api_key.as_ref().is_some_and(|k| {
        let k = k.as_str();
        k.trim().is_empty() || k.trim().len() != k.len()
    }) {
        // A blank key is not "disabled": it is stored, found, and sent, so
        // every raid start would call RaidHelper and fail. Surrounding
        // whitespace is the copy-paste version of the same outage.
        return Err(bad(
            "raidhelperapikey",
            "cannot be blank or have surrounding whitespace",
        ));
    }
    // Both channels are counted for attendance, so the same one twice would
    // award a doubled tick to everyone in it.
    let raid = p.raid_channel.or(g.config.raid_channel);
    let second = p.second_raid_channel.or(g.config.second_raid_channel);
    if let (Some(r), Some(s)) = (raid, second) {
        if r == s {
            return Err(bad(
                "secondraidchannel",
                "must be a different channel from the raid channel",
            ));
        }
    }
    Ok(())
}

fn active_raid_ref(g: &crate::state::GuildState) -> Option<RaidRef> {
    let id = g.active_raid.as_ref()?;
    let raid = g.raids.get(id)?;
    Some(RaidRef {
        raid_id: id.clone(),
        name: raid.name.clone(),
    })
}

/// Winner computation shared by finalize (authoritative) and the Discord layer
/// (display at short-auction close). Revalidates every bid against *current*
/// balances, exactly like legacy `calculateWinner` — a bid its player can no
/// longer cover is dropped, and the debit can therefore never go negative.
pub fn compute_winners(g: &crate::state::GuildState, auction_id: &str, seed: u64) -> Vec<Winner> {
    let Some(auction) = g.auctions.get(auction_id) else {
        return Vec::new();
    };
    let valid: Vec<Bid> = auction
        .bids
        .iter()
        .filter(|b| b.amount > 0 && b.amount >= auction.min_bid && b.amount <= g.balance(b.player))
        .cloned()
        .collect();
    if valid.is_empty() {
        return Vec::new();
    }
    let n = (auction.num_items as usize).min(valid.len());
    let mut rng = Rng::new(seed);
    winners(
        &valid,
        n,
        auction.min_bid_to_lock_for_main,
        auction.over_bid_to_win_main,
        &mut rng,
    )
    .into_iter()
    .map(|b| Winner {
        player: b.player,
        amount: b.amount,
        for_main: b.for_main,
    })
    .collect()
}
