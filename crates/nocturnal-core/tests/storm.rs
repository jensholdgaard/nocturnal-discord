//! Regression guard for the 2026-08-22 stresstest grind: `decide` must never
//! deep-copy guild state. With ~600k imported log entries, the legacy-killer
//! storm (4 auctions × 40 bidders) has to stay fast even in a debug build —
//! before the borrow fix this test took minutes, not milliseconds.

#![allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion

use nocturnal_core::event::{Flavor, ImportedLogEntry};
use nocturnal_core::{Actor, Command, Ctx, Item, Ledger};

const GUILD: u64 = 1;

fn ctx(now_ms: i64) -> Ctx {
    Ctx {
        guild: GUILD,
        actor: Actor::System,
        now_ms,
    }
}

#[test]
fn bid_storm_is_fast_on_heavy_history() {
    let mut ledger = Ledger::new();
    // 300 players × 2000 historical entries ≈ the fresh production import.
    for p in 0..300u64 {
        let log: Vec<ImportedLogEntry> = (0..2000)
            .map(|i| ImportedLogEntry {
                dkp: 1,
                comment: "Tick".into(),
                ts_ms: 1_000_000 + i,
                raid: None,
                item: None,
            })
            .collect();
        ledger
            .execute(
                &ctx(2_000_000),
                &Command::ImportPlayer {
                    player: 100 + p,
                    balance: 2000,
                    characters: vec![],
                    creation_ts_ms: 1_000_000,
                    log,
                },
            )
            .unwrap();
    }

    let t0 = std::time::Instant::now();
    for a in 0..4 {
        ledger
            .execute(
                &ctx(3_000_000),
                &Command::OpenAuction {
                    auction_id: format!("storm-{a}"),
                    item: Item {
                        id: a.to_string(),
                        name: "Storm".into(),
                        url: None,
                        data: None,
                        image: None,
                    },
                    flavor: Flavor::Short,
                    min_bid: 0,
                    num_items: 1,
                    min_bid_to_lock_for_main: 0,
                    over_bid_to_win_main: 0,
                    duration_ms: 60_000,
                },
            )
            .unwrap();
    }
    for p in 0..40u64 {
        for a in 0..4 {
            ledger
                .execute(
                    &ctx(3_000_100),
                    &Command::PlaceBid {
                        auction_id: format!("storm-{a}"),
                        player: 100 + p,
                        amount: 1,
                        for_main: true,
                    },
                )
                .unwrap();
        }
    }
    for a in 0..4 {
        ledger
            .execute(
                &ctx(3_100_000),
                &Command::CloseAuction {
                    auction_id: format!("storm-{a}"),
                },
            )
            .unwrap();
        ledger
            .execute(
                &ctx(3_100_001),
                &Command::CancelAuction {
                    auction_id: format!("storm-{a}"),
                    reason: "storm".into(),
                },
            )
            .unwrap();
    }
    let elapsed = t0.elapsed();
    // Debug builds are slow; the pre-fix behaviour was minutes. Generous bound.
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "storm took {elapsed:?} — decide is copying state again?"
    );
}
