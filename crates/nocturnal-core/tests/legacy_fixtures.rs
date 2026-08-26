//! The legacy bot's jest specs (`Auctions.spec.js`, `DKPManager.spec.js`),
//! ported as the behavioural contract for the new core. Where the legacy
//! implementation was buggy (E3 wrong-array draw), the *intended* behaviour
//! its own tests assert is what we pin.

#![allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion

use nocturnal_core::event::Flavor;
use nocturnal_core::{Actor, Command, Ctx, Item, Ledger};

const GUILD: u64 = 1;
const P1: u64 = 101;
const P2: u64 = 102;
const P3: u64 = 103;

fn ctx(now_ms: i64) -> Ctx {
    Ctx {
        guild: GUILD,
        actor: Actor::System,
        now_ms,
    }
}

fn item() -> Item {
    Item {
        id: "1".into(),
        name: "item".into(),
        url: None,
        data: None,
        image: None,
    }
}

fn give(l: &mut Ledger, player: u64, dkp: i64) {
    l.execute(
        &ctx(1_000),
        &Command::AdjustDkp {
            player,
            delta: dkp,
            comment: "comment".into(),
            item: None,
        },
    )
    .unwrap();
}

fn open_auction(l: &mut Ledger, id: &str, min_bid: i64, num_items: u32, lock: i64, over: i64) {
    l.execute(
        &ctx(2_000),
        &Command::OpenAuction {
            auction_id: id.into(),
            item: item(),
            flavor: Flavor::Short,
            min_bid,
            num_items,
            min_bid_to_lock_for_main: lock,
            over_bid_to_win_main: over,
            duration_ms: 60_000,
        },
    )
    .unwrap();
}

fn bid(l: &mut Ledger, id: &str, player: u64, amount: i64, for_main: bool) {
    l.execute(
        &ctx(3_000),
        &Command::PlaceBid {
            auction_id: id.into(),
            player,
            amount,
            for_main,
        },
    )
    .unwrap();
}

fn finish(l: &mut Ledger, id: &str) -> Vec<(u64, i64, bool)> {
    l.execute(
        &ctx(70_000),
        &Command::CloseAuction {
            auction_id: id.into(),
            ended_ts_ms: None,
        },
    )
    .unwrap();
    l.execute(
        &ctx(70_001),
        &Command::FinalizeAuction {
            auction_id: id.into(),
            seed: 42,
        },
    )
    .unwrap();
    l.state().guild(GUILD).unwrap().auctions[id]
        .winners
        .iter()
        .map(|w| (w.player, w.amount, w.for_main))
        .collect()
}

/// Attendance scaffold: one raid, first entry includes everyone, later
/// entries only some — mirrors the jest raid fixtures.
fn raid_with_entries(l: &mut Ledger, entries: &[&[u64]]) {
    let mut t = 5_000;
    l.execute(
        &ctx(t),
        &Command::StartRaid {
            raid_id: "raid".into(),
            name: "raid".into(),
            tick_interval_ms: 1,
            dkp_per_tick: 0,
            players_present: entries[0].to_vec(),
            event_id: None,
        },
    )
    .unwrap();
    for present in &entries[1..] {
        t += 10;
        l.execute(
            &ctx(t),
            &Command::Tick {
                players_present: present.to_vec(),
            },
        )
        .unwrap();
    }
    l.execute(
        &ctx(t + 10),
        &Command::EndRaid {
            players_present: vec![],
            reason: "officer".into(),
        },
    )
    .unwrap();
}

// --- DKPManager.spec.js ------------------------------------------------------

#[test]
fn add_dkp_credits_and_logs() {
    let mut l = Ledger::new();
    give(&mut l, P1, 13);
    let g = l.state().guild(GUILD).unwrap();
    let p = &g.players[&P1];
    assert_eq!(p.balance, 13);
    assert_eq!(p.log[0].dkp, 13);
    assert_eq!(p.log[0].comment, "comment");
}

#[test]
fn remove_dkp_debits_and_logs_negative() {
    let mut l = Ledger::new();
    give(&mut l, P1, 8);
    l.execute(
        &ctx(1_500),
        &Command::AdjustDkp {
            player: P1,
            delta: -8,
            comment: "Bad loot".into(),
            item: None,
        },
    )
    .unwrap();
    let p = &l.state().guild(GUILD).unwrap().players[&P1];
    assert_eq!(p.balance, 0);
    assert_eq!(p.log[1].dkp, -8);
}

/// listPlayers() fixture: attendance 80 / 100 / 100. Legacy marks raids
/// deprecated with a flag; we derive it from the window, so the fixture sets
/// an explicit `raid_deprecation_ms`. Timeline mirrors the jest spec: the
/// old raid predates the window, and the 4-entry raid predates P3's creation
/// (so none of its entries count as possible for P3).
#[test]
fn attendance_matches_legacy_fixture() {
    let mut l = Ledger::new();
    let now = 1_300_000i64;
    l.execute(
        &ctx(0),
        &Command::UpdateConfig {
            patch: nocturnal_core::event::ConfigPatch {
                raid_deprecation_ms: Some(800_000),
                ..Default::default()
            },
        },
    )
    .unwrap();
    // Creation order (first-touch): P1 oldest, P2 later, P3 newest.
    for (p, ts) in [(P1, 100_000i64), (P2, 500_000), (P3, 1_100_000)] {
        l.execute(
            &ctx(ts),
            &Command::AdjustDkp {
                player: p,
                delta: 1,
                comment: "seed".into(),
                item: None,
            },
        )
        .unwrap();
    }
    let e = |players: Vec<u64>, ts: i64| nocturnal_core::event::ImportedAttendance {
        players,
        comment: "Tick".into(),
        ts_ms: ts,
        amount: 1,
    };
    // Deprecated raid: outside the 800k window relative to `now`.
    l.execute(
        &ctx(400_000),
        &Command::ImportRaid {
            raid_id: "old".into(),
            name: "old".into(),
            date_ms: 400_000,
            entries: vec![e(vec![P1], 400_000)],
            tick_interval_ms: 0,
            dkp_per_tick: 0,
            event_id: None,
        },
    )
    .unwrap();
    // 4-entry raid at 1_000_000: P2 in 3 of 4, P1 in all; predates P3.
    l.execute(
        &ctx(1_000_000),
        &Command::ImportRaid {
            raid_id: "recent".into(),
            name: "Nagafen".into(),
            date_ms: 1_000_000,
            entries: vec![
                e(vec![P1, P2], 1_000_000),
                e(vec![P1, P2], 1_000_001),
                e(vec![P1, P2], 1_000_002),
                e(vec![P1], 1_000_003),
            ],
            tick_interval_ms: 0,
            dkp_per_tick: 0,
            event_id: None,
        },
    )
    .unwrap();
    // Newest raid at 1_200_000: everyone present.
    l.execute(
        &ctx(1_200_000),
        &Command::ImportRaid {
            raid_id: "newest".into(),
            name: "Nagafen".into(),
            date_ms: 1_200_000,
            entries: vec![e(vec![P1, P2, P3], 1_200_000)],
            tick_interval_ms: 0,
            dkp_per_tick: 0,
            event_id: None,
        },
    )
    .unwrap();

    let g = l.state().guild(GUILD).unwrap();
    assert_eq!(g.attendance_pct(P2, now), 80.0);
    assert_eq!(g.attendance_pct(P1, now), 100.0);
    assert_eq!(g.attendance_pct(P3, now), 100.0);
}

#[test]
fn add_by_character_resolves_case_insensitively() {
    let mut l = Ledger::new();
    l.execute(
        &ctx(1_100),
        &Command::LinkCharacter {
            player: P1,
            character: "Destroyer".into(),
        },
    )
    .unwrap();
    l.execute(
        &ctx(1_200),
        &Command::AdjustByCharacter {
            character: "destroyer".into(),
            delta: 11,
            comment: "Emperor kill".into(),
        },
    )
    .unwrap();
    assert_eq!(l.state().guild(GUILD).unwrap().balance(P1), 11);
    // Unregistered character rejected; duplicate registration rejected.
    assert!(l
        .execute(
            &ctx(1_300),
            &Command::AdjustByCharacter {
                character: "Nope".into(),
                delta: 1,
                comment: "x".into()
            }
        )
        .is_err());
    assert!(l
        .execute(
            &ctx(1_400),
            &Command::LinkCharacter {
                player: P2,
                character: "DESTROYER".into()
            }
        )
        .is_err());
}

// --- Auctions.spec.js --------------------------------------------------------

#[test]
fn bid_validation_matches_legacy() {
    let mut l = Ledger::new();
    give(&mut l, P1, 100);
    open_auction(&mut l, "a", 20, 1, 0, 0);
    let place = |l: &mut Ledger, amount: i64| {
        l.execute(
            &ctx(3_000),
            &Command::PlaceBid {
                auction_id: "a".into(),
                player: P1,
                amount,
                for_main: true,
            },
        )
    };
    assert!(place(&mut l, 0).is_err()); // must be > 0
    assert!(place(&mut l, -30).is_err());
    assert!(place(&mut l, 10).is_err()); // below min bid 20
    assert!(place(&mut l, 200).is_err()); // above balance
    assert!(place(&mut l, 20).is_ok());
    // Unknown player cannot bid.
    assert!(l
        .execute(
            &ctx(3_100),
            &Command::PlaceBid {
                auction_id: "a".into(),
                player: 999,
                amount: 20,
                for_main: true
            }
        )
        .is_err());
}

#[test]
fn rebid_replaces_and_winner_is_top_main() {
    // 'should allow to change bids': p1 10 (rebid from 30), p2 20, p3 40 ALT.
    let mut l = Ledger::new();
    for p in [P1, P2, P3] {
        give(&mut l, p, 100);
    }
    open_auction(&mut l, "a", 0, 1, 0, 0);
    bid(&mut l, "a", P1, 30, true);
    bid(&mut l, "a", P2, 20, true);
    bid(&mut l, "a", P3, 40, true);
    bid(&mut l, "a", P1, 10, true);
    bid(&mut l, "a", P3, 40, false); // switches to ALT
    let w = finish(&mut l, "a");
    assert_eq!(w, vec![(P2, 20, true)]);
}

#[test]
fn attendance_breaks_ties() {
    // 'should use attendance when bids are equal': equal 20s, P1 attended more.
    let mut l = Ledger::new();
    for p in [P1, P2, P3] {
        give(&mut l, p, 100);
    }
    raid_with_entries(&mut l, &[&[P1, P2, P3], &[P1]]);
    open_auction(&mut l, "a", 0, 1, 0, 0);
    bid(&mut l, "a", P3, 20, true);
    bid(&mut l, "a", P2, 20, true);
    bid(&mut l, "a", P1, 20, true);
    let w = finish(&mut l, "a");
    assert_eq!(w[0].0, P1);
}

#[test]
fn main_beats_alt() {
    let mut l = Ledger::new();
    give(&mut l, P1, 100);
    give(&mut l, P2, 100);
    open_auction(&mut l, "a", 0, 1, 0, 0);
    bid(&mut l, "a", P2, 20, false);
    bid(&mut l, "a", P1, 20, true);
    assert_eq!(finish(&mut l, "a")[0].0, P1);
}

#[test]
fn multiple_items_top_bids_win() {
    let mut l = Ledger::new();
    for p in [P1, P2, P3] {
        give(&mut l, p, 100);
    }
    open_auction(&mut l, "a", 0, 2, 0, 0);
    bid(&mut l, "a", P2, 10, true);
    bid(&mut l, "a", P1, 20, true);
    bid(&mut l, "a", P3, 5, true);
    let w = finish(&mut l, "a");
    assert_eq!(w, vec![(P1, 20, true), (P2, 10, true)]);
}

#[test]
fn multiple_items_alt_loses_to_mains_but_fills_spare_slots() {
    // ALT 30 loses to MAIN 20 and MAIN 5 when there are exactly 2 items...
    let mut l = Ledger::new();
    for p in [P1, P2, P3] {
        give(&mut l, p, 100);
    }
    open_auction(&mut l, "a", 0, 2, 0, 0);
    bid(&mut l, "a", P2, 30, false);
    bid(&mut l, "a", P1, 20, true);
    bid(&mut l, "a", P3, 5, true);
    let w = finish(&mut l, "a");
    assert_eq!(w, vec![(P1, 20, true), (P3, 5, true)]);

    // ...but an ALT wins when mains don't fill the slots.
    let mut l = Ledger::new();
    give(&mut l, P1, 100);
    give(&mut l, P2, 100);
    open_auction(&mut l, "b", 0, 2, 0, 0);
    bid(&mut l, "b", P1, 20, true);
    bid(&mut l, "b", P2, 5, false);
    let w = finish(&mut l, "b");
    assert_eq!(w, vec![(P1, 20, true), (P2, 5, false)]);
}

#[test]
fn tied_bids_for_two_items_both_win() {
    let mut l = Ledger::new();
    give(&mut l, P1, 100);
    give(&mut l, P2, 100);
    open_auction(&mut l, "a", 15, 2, 0, 0);
    bid(&mut l, "a", P1, 20, true);
    bid(&mut l, "a", P2, 20, true);
    let mut winners: Vec<u64> = finish(&mut l, "a").iter().map(|w| w.0).collect();
    winners.sort_unstable();
    assert_eq!(winners, vec![P1, P2]);
}

#[test]
fn main_below_lock_competes_as_alt() {
    // minBidToLockForMain=20: ALT 18 beats MAIN 15.
    let mut l = Ledger::new();
    give(&mut l, P1, 100);
    give(&mut l, P2, 100);
    open_auction(&mut l, "a", 0, 1, 20, 0);
    bid(&mut l, "a", P1, 18, false);
    bid(&mut l, "a", P2, 15, true);
    assert_eq!(finish(&mut l, "a")[0], (P1, 18, false));
}

#[test]
fn alt_overbid_promotes_to_main() {
    // overBidtoWinMain=100: ALT 200 >= MAIN 25 + 100 wins.
    let mut l = Ledger::new();
    give(&mut l, P1, 300);
    give(&mut l, P2, 300);
    open_auction(&mut l, "a", 0, 1, 20, 100);
    bid(&mut l, "a", P1, 200, false);
    bid(&mut l, "a", P2, 25, true);
    assert_eq!(finish(&mut l, "a")[0], (P1, 200, false));

    // Only ALTs bid: highest simply wins.
    let mut l = Ledger::new();
    give(&mut l, P1, 300);
    give(&mut l, P2, 300);
    open_auction(&mut l, "b", 0, 1, 20, 100);
    bid(&mut l, "b", P1, 200, false);
    bid(&mut l, "b", P2, 25, false);
    assert_eq!(finish(&mut l, "b")[0], (P1, 200, false));
}

// --- integrity: the audit's failure classes ---------------------------------

#[test]
fn finalize_debits_winner_exactly_once() {
    let mut l = Ledger::new();
    give(&mut l, P1, 100);
    open_auction(&mut l, "a", 0, 1, 0, 0);
    bid(&mut l, "a", P1, 60, true);
    finish(&mut l, "a");
    assert_eq!(l.state().guild(GUILD).unwrap().balance(P1), 40);
    // Double finalize is unrepresentable.
    assert!(l
        .execute(
            &ctx(80_000),
            &Command::FinalizeAuction {
                auction_id: "a".into(),
                seed: 1
            }
        )
        .is_err());
    assert_eq!(l.state().guild(GUILD).unwrap().balance(P1), 40);
}

#[test]
fn cross_auction_double_spend_rejected_at_bid_time() {
    let mut l = Ledger::new();
    give(&mut l, P1, 100);
    open_auction(&mut l, "a", 0, 1, 0, 0);
    open_auction(&mut l, "b", 0, 1, 0, 0);
    bid(&mut l, "a", P1, 100, true);
    let r = l.execute(
        &ctx(3_500),
        &Command::PlaceBid {
            auction_id: "b".into(),
            player: P1,
            amount: 100,
            for_main: true,
        },
    );
    assert!(r.is_err(), "audit #46: 100 DKP cannot back two 100 bids");
}

#[test]
fn stale_winner_dropped_if_balance_no_longer_covers() {
    let mut l = Ledger::new();
    give(&mut l, P1, 100);
    give(&mut l, P2, 100);
    open_auction(&mut l, "a", 0, 1, 0, 0);
    bid(&mut l, "a", P1, 80, true);
    bid(&mut l, "a", P2, 50, true);
    l.execute(
        &ctx(70_000),
        &Command::CloseAuction {
            auction_id: "a".into(),
            ended_ts_ms: None,
        },
    )
    .unwrap();
    // Officer strips P1's DKP between close and confirm.
    l.execute(
        &ctx(70_500),
        &Command::AdjustDkp {
            player: P1,
            delta: -100,
            comment: "penalty".into(),
            item: None,
        },
    )
    .unwrap();
    l.execute(
        &ctx(71_000),
        &Command::FinalizeAuction {
            auction_id: "a".into(),
            seed: 7,
        },
    )
    .unwrap();
    let g = l.state().guild(GUILD).unwrap();
    assert_eq!(
        g.auctions["a"].winners[0].player, P2,
        "revalidation drops P1"
    );
    assert_eq!(g.balance(P2), 50);
    assert!(g.balance(P1) >= 0);
}

#[test]
fn second_raid_and_double_tick_rejected() {
    let mut l = Ledger::new();
    l.execute(
        &ctx(1_000),
        &Command::StartRaid {
            raid_id: "r1".into(),
            name: "Naggy".into(),
            tick_interval_ms: 60_000,
            dkp_per_tick: 1,
            players_present: vec![P1],
            event_id: None,
        },
    )
    .unwrap();
    assert!(l
        .execute(
            &ctx(1_500),
            &Command::StartRaid {
                raid_id: "r2".into(),
                name: "Vox".into(),
                tick_interval_ms: 60_000,
                dkp_per_tick: 1,
                players_present: vec![P1],
                event_id: None,
            }
        )
        .is_err());
    // Tick before the interval elapses is refused (double-tick guard).
    assert!(l
        .execute(
            &ctx(30_000),
            &Command::Tick {
                players_present: vec![P1]
            }
        )
        .is_err());
    assert!(l
        .execute(
            &ctx(61_001),
            &Command::Tick {
                players_present: vec![P1]
            }
        )
        .is_ok());
    assert_eq!(l.state().guild(GUILD).unwrap().balance(P1), 2); // Start + 1 tick
}

// --- what the bot must be able to *tell* you --------------------------------

/// Overspending is refused with the numbers needed to explain it: what you
/// have, what is already committed elsewhere, and what you asked for.
#[test]
fn insufficient_balance_carries_the_reservation() {
    use nocturnal_core::Rejection;

    // Plain overspend on a single auction: nothing committed elsewhere.
    let mut l = Ledger::new();
    give(&mut l, P1, 50);
    open_auction(&mut l, "a", 0, 1, 0, 0);
    let err = l
        .execute(
            &ctx(3_000),
            &Command::PlaceBid {
                auction_id: "a".into(),
                player: P1,
                amount: 80,
                for_main: true,
            },
        )
        .unwrap_err();
    assert_eq!(
        err,
        Rejection::InsufficientBalance {
            available: 50,
            committed: 0,
            needed: 80
        }
    );

    // Committed elsewhere: the message can say *why* 40 is too much.
    let mut l = Ledger::new();
    give(&mut l, P1, 100);
    open_auction(&mut l, "a", 0, 1, 0, 0);
    open_auction(&mut l, "b", 0, 1, 0, 0);
    bid(&mut l, "a", P1, 70, true);
    let err = l
        .execute(
            &ctx(3_100),
            &Command::PlaceBid {
                auction_id: "b".into(),
                player: P1,
                amount: 40,
                for_main: true,
            },
        )
        .unwrap_err();
    assert_eq!(
        err,
        Rejection::InsufficientBalance {
            available: 30,
            committed: 70,
            needed: 40
        }
    );

    // Debits report the plain balance, with nothing reserved.
    let mut l = Ledger::new();
    give(&mut l, P1, 10);
    let err = l
        .execute(
            &ctx(3_200),
            &Command::AdjustDkp {
                player: P1,
                delta: -25,
                comment: "loot".into(),
                item: None,
            },
        )
        .unwrap_err();
    assert_eq!(
        err,
        Rejection::InsufficientBalance {
            available: 10,
            committed: 0,
            needed: 25
        }
    );
}

/// Loot won during a raid must be attributed to that raid, or the raid
/// summary and /dkphistory cannot say who won what (legacy passed the active
/// raid to removeDKP; the fold dropped it).
#[test]
fn auction_loot_is_attributed_to_the_active_raid() {
    let mut l = Ledger::new();
    give(&mut l, P1, 100);
    l.execute(
        &ctx(5_000),
        &Command::StartRaid {
            raid_id: "naggy".into(),
            name: "Nagafen".into(),
            tick_interval_ms: 60_000,
            dkp_per_tick: 1,
            players_present: vec![P1],
            event_id: None,
        },
    )
    .unwrap();
    open_auction(&mut l, "a", 0, 1, 0, 0);
    bid(&mut l, "a", P1, 30, true);
    finish(&mut l, "a");

    let g = l.state().guild(GUILD).unwrap();
    let debit = g.players[&P1]
        .log
        .iter()
        .find(|e| e.dkp == -30)
        .expect("winner was charged");
    let raid = debit.raid.as_ref().expect("loot carries its raid");
    assert_eq!(raid.raid_id, "naggy");
    assert_eq!(raid.name, "Nagafen");
    assert_eq!(debit.item.as_ref().map(|i| i.name.as_str()), Some("item"));

    // Outside a raid there is nothing to attribute it to.
    let mut l = Ledger::new();
    give(&mut l, P2, 100);
    open_auction(&mut l, "b", 0, 1, 0, 0);
    bid(&mut l, "b", P2, 10, true);
    finish(&mut l, "b");
    let g = l.state().guild(GUILD).unwrap();
    assert!(g.players[&P2]
        .log
        .iter()
        .find(|e| e.dkp == -10)
        .unwrap()
        .raid
        .is_none());
}
