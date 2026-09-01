//! The guild's attendance rule — Zig's roster sheet, matched 240/240 on
//! 2026-09-01 (docs/attendance.md). These pin the parts a plausible
//! re-implementation gets wrong: which weeks count, which two are dropped,
//! how ties among them break, and that the result is floored.

#![allow(clippy::unwrap_used)]

use nocturnal_core::event::ImportedAttendance;
use nocturnal_core::{Actor, Command, Ctx, Ledger};

const GUILD: u64 = 42;
const DAY: i64 = 86_400_000;
const WEEK: i64 = 7 * DAY;
const ME: u64 = 7;
const OTHER: u64 = 8;

fn ctx(now_ms: i64) -> Ctx {
    Ctx {
        guild: GUILD,
        actor: Actor::System,
        now_ms,
    }
}

/// One raid in week `week` (Monday-based, UTC) with `held` ticks, `me` of
/// them attended; `extras` are non-tick entries that must not count.
fn raid(l: &mut Ledger, id: &str, week: i64, held: u64, me: u64, extras: &[(&str, i64)]) {
    let at = week * WEEK + DAY; // a Tuesday-ish: the epoch shift keeps it inside `week`
    let mut entries: Vec<ImportedAttendance> = (0..held)
        .map(|i| ImportedAttendance {
            players: if i < me { vec![ME, OTHER] } else { vec![OTHER] },
            comment: if i == 0 {
                "Start".into()
            } else {
                "Tick".into()
            },
            ts_ms: at + i as i64 * 60_000,
            amount: 1,
        })
        .collect();
    for (comment, amount) in extras {
        entries.push(ImportedAttendance {
            players: vec![ME, OTHER],
            comment: (*comment).into(),
            ts_ms: at + 3_600_000,
            amount: *amount,
        });
    }
    let envs = l
        .propose(
            &ctx(at),
            &Command::ImportRaid {
                raid_id: id.into(),
                name: format!("Raid {id}"),
                date_ms: at,
                entries,
                tick_interval_ms: 60_000,
                dkp_per_tick: 1,
                event_id: None,
            },
        )
        .unwrap();
    l.commit(&envs);
}

#[test]
fn ten_weeks_drop_two_worst_ties_by_ticks_held_then_floor() {
    let mut l = Ledger::new();
    // An 11th, older week the rule must ignore (it would drag ME to the floor).
    raid(&mut l, "w0", 0, 100, 0, &[]);
    for w in 1..=7 {
        raid(&mut l, &format!("w{w}"), w, 10, 10, &[]);
    }
    // Three 0 % weeks: 20, 12 and 15 ticks held. Two get dropped — the two
    // with MORE ticks held (20 and 15), so the 12-tick week stays in.
    raid(&mut l, "w8", 8, 20, 0, &[]);
    raid(&mut l, "w9", 9, 12, 0, &[("End", 0), ("Bonus", 5)]);
    raid(&mut l, "w10", 10, 15, 0, &[]);
    let now = 10 * WEEK + 2 * DAY; // inside week 10: the partial week counts
    let g = l.state().guild(GUILD).unwrap();
    // 70 attended of 70 + 12 held = 85.36…, floored.
    assert_eq!(g.attendance_pct(ME, now), 85.0);
    assert_eq!(g.attendance_pct(OTHER, now), 100.0);
    // A player who was nowhere: the eleventh week is still ignored, the two
    // largest 0 % weeks go, and the rest is 0 of 82.
    assert_eq!(g.attendance_pct(999, now), 0.0);
}

#[test]
fn eight_weeks_or_fewer_keep_everything_and_floor() {
    let mut l = Ledger::new();
    raid(&mut l, "a", 1, 9, 7, &[]);
    let g = l.state().guild(GUILD).unwrap();
    assert_eq!(
        g.attendance_pct(ME, 2 * WEEK),
        77.0,
        "7/9 = 77.7 floors, never rounds"
    );
    raid(&mut l, "b", 2, 1, 0, &[]);
    let g = l.state().guild(GUILD).unwrap();
    assert_eq!(
        g.attendance_pct(ME, 3 * WEEK),
        70.0,
        "two weeks: nothing dropped, 7/10"
    );
}

#[test]
fn only_dkp_bearing_ticks_count() {
    let mut l = Ledger::new();
    raid(
        &mut l,
        "a",
        1,
        4,
        2,
        &[("End", 0), ("Bonus", 10), ("Bonus", 10)],
    );
    let g = l.state().guild(GUILD).unwrap();
    assert_eq!(
        g.attendance_pct(ME, 2 * WEEK),
        50.0,
        "2 of 4 ticks; the awards are not ticks"
    );
}

#[test]
fn nothing_possible_is_100_and_the_future_is_invisible() {
    let mut l = Ledger::new();
    let g = l.state().guild(GUILD);
    assert!(g.is_none() || g.unwrap().attendance_pct(ME, 0) == 100.0);
    raid(&mut l, "a", 5, 10, 0, &[]);
    let g = l.state().guild(GUILD).unwrap();
    assert_eq!(
        g.attendance_pct(ME, WEEK),
        100.0,
        "the only raid is after `now`"
    );
}
