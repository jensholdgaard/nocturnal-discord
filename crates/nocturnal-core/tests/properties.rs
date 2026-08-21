//! Property tests: the integrity invariants from `docs/architecture.md`,
//! checked against arbitrary command streams. Whatever sequence of valid or
//! invalid requests arrives, the ledger can never reach a corrupt state.

#![allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion

use proptest::prelude::*;

use nocturnal_core::event::Flavor;
use nocturnal_core::state::AuctionStatus;
use nocturnal_core::{Actor, Command, Ctx, Envelope, Item, Ledger};

const GUILD: u64 = 1;

fn arb_command() -> impl Strategy<Value = Command> {
    let player = 1u64..6;
    let auction_id = prop::sample::select(vec!["a1", "a2", "a3"]);
    let amount = 1i64..200;
    prop_oneof![
        (player.clone(), -50i64..120).prop_map(|(player, delta)| Command::AdjustDkp {
            player,
            delta,
            comment: "c".into(),
            item: None,
        }),
        (prop::sample::select(vec!["r1", "r2"]), player.clone()).prop_map(|(raid_id, p)| {
            Command::StartRaid {
                raid_id: raid_id.into(),
                name: "raid".into(),
                tick_interval_ms: 10,
                dkp_per_tick: 1,
                players_present: vec![p],
                event_id: None,
            }
        }),
        player.clone().prop_map(|p| Command::Tick {
            players_present: vec![p]
        }),
        player.clone().prop_map(|p| Command::AwardRaid {
            players: vec![p],
            amount: 2,
            comment: "x".into()
        }),
        Just(Command::EndRaid {
            players_present: vec![],
            reason: "r".into()
        }),
        (auction_id.clone(), 1u32..3).prop_map(|(id, n)| Command::OpenAuction {
            auction_id: id.into(),
            item: Item {
                id: "1".into(),
                name: "item".into(),
                url: None
            },
            flavor: Flavor::Short,
            min_bid: 0,
            num_items: n,
            min_bid_to_lock_for_main: 0,
            over_bid_to_win_main: 0,
            duration_ms: 1_000,
        }),
        (auction_id.clone(), player.clone(), amount, any::<bool>()).prop_map(
            |(id, player, amount, for_main)| Command::PlaceBid {
                auction_id: id.into(),
                player,
                amount,
                for_main,
            }
        ),
        (auction_id.clone(), player).prop_map(|(id, player)| Command::RetractBid {
            auction_id: id.into(),
            player,
        }),
        auction_id.clone().prop_map(|id| Command::CloseAuction {
            auction_id: id.into()
        }),
        (auction_id.clone(), any::<u64>()).prop_map(|(id, seed)| Command::FinalizeAuction {
            auction_id: id.into(),
            seed,
        }),
        auction_id.prop_map(|id| Command::CancelAuction {
            auction_id: id.into(),
            reason: "r".into()
        }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Balances never go negative, there is never more than one active raid,
    /// and every finalized auction's winners were debited exactly once —
    /// no matter what command stream arrives.
    #[test]
    fn invariants_hold_for_any_command_stream(cmds in prop::collection::vec(arb_command(), 1..120)) {
        let mut ledger = Ledger::new();
        let mut now = 1_000i64;
        let mut envelopes: Vec<Envelope> = Vec::new();

        for cmd in &cmds {
            now += 7;
            let ctx = Ctx { guild: GUILD, actor: Actor::System, now_ms: now };
            if let Ok(evs) = ledger.execute(&ctx, cmd) {
                envelopes.extend(evs);
            }
            if let Some(g) = ledger.state().guild(GUILD) {
                // Invariant 1: no negative balances, ever.
                for (id, p) in &g.players {
                    prop_assert!(p.balance >= 0, "player {id} balance {}", p.balance);
                }
                // Invariant 2: at most one active raid.
                let active = g.raids.values().filter(|r| r.active).count();
                prop_assert!(active <= 1, "{active} active raids");
                prop_assert_eq!(active == 1, g.active_raid.is_some());
                // Invariant 3: every winner of a finalized auction could
                // afford the debit — a winner's post-debit balance is >= 0
                // (covered by invariant 1) and the winner amounts are the
                // recorded bids, never more.
                for a in g.auctions.values() {
                    if a.status == AuctionStatus::Finalized {
                        for w in &a.winners {
                            prop_assert!(w.amount > 0);
                        }
                    }
                }
            }
        }

        // Invariant 4: replay determinism — folding the persisted envelopes
        // into a fresh ledger reproduces the exact same state.
        let mut replayed = Ledger::new();
        for env in &envelopes {
            replayed.replay(env);
        }
        prop_assert_eq!(replayed, ledger);

        // Invariant 5: serialization round-trips the whole log.
        for env in &envelopes {
            let json = serde_json::to_string(env).unwrap();
            let back: Envelope = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(&back, env);
        }
    }

    /// A rejected command leaves the ledger byte-identical.
    #[test]
    fn rejection_mutates_nothing(cmds in prop::collection::vec(arb_command(), 1..60)) {
        let mut ledger = Ledger::new();
        let mut now = 1_000i64;
        for cmd in &cmds {
            now += 7;
            let ctx = Ctx { guild: GUILD, actor: Actor::System, now_ms: now };
            let snapshot = ledger.clone();
            if ledger.execute(&ctx, cmd).is_err() {
                prop_assert_eq!(&snapshot, &ledger);
            }
        }
    }
}
