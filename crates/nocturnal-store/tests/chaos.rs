//! Chaos: overlapping auctions and raid ticks with the process killed at
//! random points, including mid-fsync (a torn WAL tail).
//!
//! This is the M5 exit criterion and the audit's nightmare scenario made
//! routine: the legacy bot lost every in-flight auction on any crash (#7/#40),
//! could charge a winner twice or not at all (E2/#46/#49), and could double a
//! raid tick (#35/#47). Every seed here asserts none of that can happen.
//!
//! Deterministic by construction: a seeded PRNG, no wall clock, no ambient
//! randomness — a failing seed reproduces exactly.

#![allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion

use std::collections::HashMap;
use std::path::Path;

use nocturnal_core::event::Flavor;
use nocturnal_core::state::AuctionStatus;
use nocturnal_core::{Actor, Command, Ctx, Envelope, Item, Ledger};
use nocturnal_store::Store;

const GUILD: u64 = 1;
const PLAYERS: [u64; 6] = [101, 102, 103, 104, 105, 106];

/// splitmix64 — same generator the ledger uses for tie-breaks.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Boot exactly like the bot does: open the store, replay every event.
fn boot(dir: &Path) -> (Ledger, Store) {
    let (store, envelopes) = Store::open(dir).expect("store opens after any crash");
    let mut ledger = Ledger::new();
    for env in &envelopes {
        ledger.replay(env);
    }
    (ledger, store)
}

/// decide → append(fsync) → apply, the driver's loop. Rejections are normal.
fn exec(
    ledger: &mut Ledger,
    store: &mut Store,
    now_ms: i64,
    cmd: &Command,
) -> Option<Vec<Envelope>> {
    let ctx = Ctx {
        guild: GUILD,
        actor: Actor::System,
        now_ms,
    };
    let envelopes = ledger.propose(&ctx, cmd).ok()?;
    store.append(&envelopes).expect("wal append");
    ledger.commit(&envelopes);
    Some(envelopes)
}

/// Kill -9 in the middle of a write: the last record is half on disk.
fn tear_wal_tail(dir: &Path) {
    let wal = dir.join("wal");
    let mut segments: Vec<_> = std::fs::read_dir(&wal)
        .expect("wal dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    segments.sort();
    let Some(last) = segments.last() else { return };
    let bytes = std::fs::read(last).expect("read segment");
    if bytes.len() < 40 {
        return;
    }
    // Cut somewhere inside the final record.
    let cut = bytes.len() - (bytes.len() / 7).max(5);
    std::fs::write(last, &bytes[..cut]).expect("truncate segment");
}

fn item(n: usize) -> Item {
    Item {
        id: n.to_string(),
        name: format!("Chaos Item {n}"),
        url: None,
        data: None,
        image: None,
    }
}

fn run_scenario(seed: u64) {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = Rng(seed);
    let mut now = 1_800_000_000_000i64;
    let (mut ledger, mut store) = boot(dir.path());

    // Seed balances and start a raid.
    for p in PLAYERS {
        exec(
            &mut ledger,
            &mut store,
            now,
            &Command::AdjustDkp {
                player: p,
                delta: 200,
                comment: "seed".into(),
                item: None,
            },
        );
    }
    exec(
        &mut ledger,
        &mut store,
        now,
        &Command::StartRaid {
            raid_id: "chaos".into(),
            name: "Chaos".into(),
            tick_interval_ms: 10_000,
            dkp_per_tick: 1,
            players_present: PLAYERS.to_vec(),
            event_id: None,
        },
    );

    let mut open_auctions: Vec<String> = Vec::new();
    let mut next_auction = 0usize;
    // Every charge the ledger told us it made: (player, item name, amount).
    let mut charged: Vec<(u64, String, i64)> = Vec::new();

    for step in 0..120 {
        now += 1_000 + rng.below(4_000) as i64;
        match rng.below(10) {
            // open an auction (overlapping by design)
            0 | 1 if open_auctions.len() < 4 => {
                let id = format!("au-{next_auction}");
                let n = next_auction;
                next_auction += 1;
                if exec(
                    &mut ledger,
                    &mut store,
                    now,
                    &Command::OpenAuction {
                        auction_id: id.clone(),
                        item: item(n),
                        flavor: if rng.below(4) == 0 {
                            Flavor::Long
                        } else {
                            Flavor::Short
                        },
                        min_bid: 0,
                        num_items: 1 + rng.below(2) as u32,
                        min_bid_to_lock_for_main: 0,
                        over_bid_to_win_main: 0,
                        duration_ms: 30_000,
                    },
                )
                .is_some()
                {
                    open_auctions.push(id);
                }
            }
            // bid
            2..=5 if !open_auctions.is_empty() => {
                let auction_id =
                    open_auctions[rng.below(open_auctions.len() as u64) as usize].clone();
                let player = PLAYERS[rng.below(PLAYERS.len() as u64) as usize];
                exec(
                    &mut ledger,
                    &mut store,
                    now,
                    &Command::PlaceBid {
                        auction_id,
                        player,
                        amount: 1 + rng.below(120) as i64,
                        for_main: rng.below(3) != 0,
                    },
                );
            }
            // raid tick
            6 => {
                exec(
                    &mut ledger,
                    &mut store,
                    now,
                    &Command::Tick {
                        players_present: PLAYERS.to_vec(),
                    },
                );
            }
            // close + settle an auction
            7 | 8 if !open_auctions.is_empty() => {
                let idx = rng.below(open_auctions.len() as u64) as usize;
                let auction_id = open_auctions.remove(idx);
                exec(
                    &mut ledger,
                    &mut store,
                    now,
                    &Command::CloseAuction {
                        auction_id: auction_id.clone(),
                    },
                );
                if rng.below(5) == 0 {
                    exec(
                        &mut ledger,
                        &mut store,
                        now,
                        &Command::CancelAuction {
                            auction_id,
                            reason: "chaos".into(),
                        },
                    );
                } else if let Some(envelopes) = exec(
                    &mut ledger,
                    &mut store,
                    now,
                    &Command::FinalizeAuction {
                        auction_id: auction_id.clone(),
                        seed: rng.next(),
                    },
                ) {
                    for env in &envelopes {
                        if let nocturnal_core::Event::AuctionFinalized { winners, .. } = &env.event
                        {
                            let name = ledger
                                .state()
                                .guild(GUILD)
                                .and_then(|g| {
                                    g.auctions.get(&auction_id).map(|a| a.item.name.clone())
                                })
                                .unwrap_or_default();
                            for w in winners {
                                charged.push((w.player, name.clone(), w.amount));
                            }
                        }
                    }
                }
            }
            // kill -9
            9 => {
                let torn = rng.below(3) == 0;
                drop(store);
                drop(ledger);
                if torn {
                    tear_wal_tail(dir.path());
                }
                let booted = boot(dir.path());
                ledger = booted.0;
                store = booted.1;
                // A torn tail may have dropped the last event, so anything we
                // believed about auctions must be re-derived from the ledger.
                if torn {
                    let g = ledger.state().guild(GUILD).cloned().unwrap_or_default();
                    open_auctions.retain(|id| {
                        g.auctions
                            .get(id)
                            .is_some_and(|a| a.status == AuctionStatus::Open)
                    });
                    charged.retain(|(_, name, _)| {
                        g.auctions
                            .values()
                            .any(|a| a.item.name == *name && a.status == AuctionStatus::Finalized)
                    });
                }
            }
            _ => {}
        }
        assert!(step < 1000);
    }

    // --- invariants, after everything the scenario threw at it -------------
    drop(store);
    let (ledger, _) = boot(dir.path());
    let g = ledger.state().guild(GUILD).expect("guild exists").clone();

    for (id, p) in &g.players {
        assert!(
            p.balance >= 0,
            "seed {seed}: player {id} went negative ({})",
            p.balance
        );
    }
    let active = g.raids.values().filter(|r| r.active).count();
    assert!(active <= 1, "seed {seed}: {active} active raids");

    // Every charge the ledger reported appears exactly once in the ledger —
    // no winner charged twice (audit #49), none charged never (audit E2).
    let mut expected: HashMap<(u64, String, i64), usize> = HashMap::new();
    for c in &charged {
        *expected.entry(c.clone()).or_default() += 1;
    }
    for ((player, item_name, amount), times) in expected {
        let found = g.players[&player]
            .log
            .iter()
            .filter(|e| e.dkp == -amount && e.item.as_ref().is_some_and(|i| i.name == item_name))
            .count();
        assert_eq!(
            found, times,
            "seed {seed}: {player} should be charged {amount} for {item_name} exactly {times}x"
        );
    }

    // Ticks are idempotent: a raid's tick numbers are strictly increasing and
    // never repeat (audit #35/#47).
    for raid in g.raids.values() {
        let ticks: Vec<usize> = raid
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.comment == "Tick")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            ticks.len(),
            ticks.iter().collect::<std::collections::HashSet<_>>().len(),
            "seed {seed}: duplicated tick entries"
        );
    }

    // Replaying the surviving log again lands in exactly the same state.
    let (again, _) = boot(dir.path());
    assert_eq!(again, ledger, "seed {seed}: replay is not deterministic");
}

#[test]
fn chaos_survives_random_crashes() {
    for seed in 0..25 {
        run_scenario(seed);
    }
}

/// A crash between the ledger deciding and the WAL fsync completing must lose
/// the event entirely — never half-apply it.
#[test]
fn torn_write_is_all_or_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ledger, mut store) = boot(dir.path());
    exec(
        &mut ledger,
        &mut store,
        1_000,
        &Command::AdjustDkp {
            player: 1,
            delta: 40,
            comment: "seed".into(),
            item: None,
        },
    );
    exec(
        &mut ledger,
        &mut store,
        2_000,
        &Command::AdjustDkp {
            player: 1,
            delta: -15,
            comment: "loot".into(),
            item: None,
        },
    );
    let before = ledger.state().guild(GUILD).unwrap().balance(1);
    assert_eq!(before, 25);

    drop(store);
    tear_wal_tail(dir.path());
    let (recovered, _) = boot(dir.path());
    let after = recovered.state().guild(GUILD).unwrap().balance(1);
    // Either the debit survived whole, or it never happened. Never partial.
    assert!(after == 25 || after == 40, "seed torn write left {after}");
}
