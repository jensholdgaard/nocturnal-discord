//! M1 exit criterion (docs/plan.md): a scripted raid night — raid, ticks,
//! three overlapping auctions, a kill -9 mid-auction — replays to provably
//! correct balances. This test IS the "anatomy of a typical crash" from the
//! audit, made boring.

#![allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion

use nocturnal_core::event::Flavor;
use nocturnal_core::state::AuctionStatus;
use nocturnal_core::{Actor, Command, Ctx, Item, Ledger};
use nocturnal_store::Wal;

const GUILD: u64 = 1;
const OFFICER: u64 = 900;
const RAIDERS: [u64; 4] = [1, 2, 3, 4];

/// The driver loop in miniature: decide → append (fsync) → apply.
fn run(ledger: &mut Ledger, wal: &mut Wal, now_ms: i64, cmd: Command) {
    let ctx = Ctx {
        guild: GUILD,
        actor: Actor::User(OFFICER),
        now_ms,
    };
    let envelopes = ledger.propose(&ctx, &cmd).expect("command accepted");
    wal.append(&envelopes).expect("durable");
    ledger.commit(&envelopes);
}

fn boot(dir: &std::path::Path) -> (Ledger, Wal) {
    let (wal, envelopes) = Wal::open(dir).expect("wal opens");
    let mut ledger = Ledger::new();
    for env in &envelopes {
        ledger.replay(env);
    }
    (ledger, wal)
}

fn item(name: &str) -> Item {
    Item {
        id: name.to_owned(),
        name: name.to_owned(),
        url: None,
        data: None,
        image: None,
    }
}

#[test]
fn raid_night_survives_kill_dash_nine() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = 1_000_000i64;

    // -- evening starts -------------------------------------------------------
    let (mut ledger, mut wal) = boot(dir.path());
    for p in RAIDERS {
        run(
            &mut ledger,
            &mut wal,
            t,
            Command::AdjustDkp {
                player: p,
                delta: 50,
                comment: "seed balance".into(),
                item: None,
            },
        );
    }
    run(
        &mut ledger,
        &mut wal,
        t,
        Command::StartRaid {
            raid_id: "naggy".into(),
            name: "Nagafen".into(),
            tick_interval_ms: 360_000,
            dkp_per_tick: 1,
            players_present: RAIDERS.to_vec(),
            event_id: None,
        },
    );
    for _ in 0..3 {
        t += 361_000;
        run(
            &mut ledger,
            &mut wal,
            t,
            Command::Tick {
                players_present: RAIDERS.to_vec(),
            },
        );
    }
    // Everyone: 50 seed + 1 start + 3 ticks = 54.

    // -- three overlapping auctions ------------------------------------------
    for (id, name) in [("a1", "Cloak"), ("a2", "Blade"), ("a3", "Ring")] {
        run(
            &mut ledger,
            &mut wal,
            t,
            Command::OpenAuction {
                auction_id: id.into(),
                item: item(name),
                flavor: Flavor::Short,
                min_bid: 0,
                num_items: 1,
                min_bid_to_lock_for_main: 0,
                over_bid_to_win_main: 0,
                duration_ms: 60_000,
            },
        );
    }
    run(
        &mut ledger,
        &mut wal,
        t,
        Command::PlaceBid {
            auction_id: "a1".into(),
            player: 1,
            amount: 30,
            for_main: true,
        },
    );
    run(
        &mut ledger,
        &mut wal,
        t,
        Command::PlaceBid {
            auction_id: "a1".into(),
            player: 2,
            amount: 20,
            for_main: true,
        },
    );
    run(
        &mut ledger,
        &mut wal,
        t,
        Command::PlaceBid {
            auction_id: "a2".into(),
            player: 3,
            amount: 40,
            for_main: true,
        },
    );
    run(
        &mut ledger,
        &mut wal,
        t,
        Command::PlaceBid {
            auction_id: "a3".into(),
            player: 4,
            amount: 10,
            for_main: false,
        },
    );

    // Close a1, winner announced (display), officer clicks confirm…
    t += 61_000;
    run(
        &mut ledger,
        &mut wal,
        t,
        Command::CloseAuction {
            auction_id: "a1".into(),
            ended_ts_ms: None,
        },
    );

    // -- the 10062 moment: process dies before the confirm lands --------------
    drop(wal);
    drop(ledger);

    // -- Pterodactyl restarts us: boot = replay ------------------------------
    let (mut ledger, mut wal) = boot(dir.path());
    {
        let g = ledger.state().guild(GUILD).unwrap();
        // Nothing was lost: a1 is closed awaiting confirm, a2/a3 still open
        // with their bids intact — the exact opposite of the legacy bot,
        // where every in-memory auction evaporated.
        assert_eq!(g.auctions["a1"].status, AuctionStatus::Closed);
        assert_eq!(g.auctions["a1"].bids.len(), 2);
        assert_eq!(g.auctions["a2"].status, AuctionStatus::Open);
        assert_eq!(g.auctions["a3"].bids.len(), 1);
        assert_eq!(g.active_raid.as_deref(), Some("naggy"));
    }

    // Officer confirms a1 (the debit); scheduler closes and finalizes the rest.
    t += 5_000;
    run(
        &mut ledger,
        &mut wal,
        t,
        Command::FinalizeAuction {
            auction_id: "a1".into(),
            seed: 0xE0,
        },
    );
    run(
        &mut ledger,
        &mut wal,
        t,
        Command::CloseAuction {
            auction_id: "a2".into(),
            ended_ts_ms: None,
        },
    );
    run(
        &mut ledger,
        &mut wal,
        t,
        Command::FinalizeAuction {
            auction_id: "a2".into(),
            seed: 2,
        },
    );
    run(
        &mut ledger,
        &mut wal,
        t,
        Command::CloseAuction {
            auction_id: "a3".into(),
            ended_ts_ms: None,
        },
    );
    run(
        &mut ledger,
        &mut wal,
        t,
        Command::FinalizeAuction {
            auction_id: "a3".into(),
            seed: 3,
        },
    );
    t += 361_000;
    run(
        &mut ledger,
        &mut wal,
        t,
        Command::Tick {
            players_present: RAIDERS.to_vec(),
        },
    );
    run(
        &mut ledger,
        &mut wal,
        t,
        Command::EndRaid {
            players_present: RAIDERS.to_vec(),
            reason: "officer".into(),
        },
    );

    // -- the books, provably --------------------------------------------------
    let g = ledger.state().guild(GUILD).unwrap();
    // 50 + 1 (start) + 4 ticks = 55 earned each.
    assert_eq!(g.balance(1), 55 - 30, "P1 won Cloak for 30");
    assert_eq!(g.balance(2), 55, "P2 lost the Cloak bid, never charged");
    assert_eq!(
        g.balance(3),
        55 - 40,
        "P3 won Blade for 40 — the long-auction class debit"
    );
    assert_eq!(g.balance(4), 55 - 10, "P4 won Ring for 10 (ALT)");
    for a in ["a1", "a2", "a3"] {
        assert_eq!(g.auctions[a].status, AuctionStatus::Finalized);
        assert_eq!(g.auctions[a].winners.len(), 1);
    }
    assert!(g.raids["naggy"].entries.len() == 6 && !g.raids["naggy"].active);

    // Full-log replay determinism, end to end.
    drop(wal);
    let (replayed, _) = boot(dir.path());
    assert_eq!(replayed, ledger);
}
