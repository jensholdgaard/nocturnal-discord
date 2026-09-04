//! Which raids are one night. Named the same and back-to-back: yes. An
//! unnamed (placeholder-named) false start next to a raid: yes. Different
//! names, or too far apart: no.

#![allow(clippy::unwrap_used)]

use nocturnal_core::state::{is_placeholder_raid_name, same_raid, Raid, SAME_RAID_GAP_MS};

fn raid(name: &str, start: i64, end: i64) -> Raid {
    Raid {
        name: name.into(),
        date_ms: start,
        tick_interval_ms: 60_000,
        dkp_per_tick: 1,
        active: false,
        tick_no: 0,
        event_id: None,
        entries: Vec::new(),
        ended_ms: Some(end),
    }
}

#[test]
fn placeholder_names_are_the_discord_markup_or_nothing() {
    assert!(is_placeholder_raid_name("<t:1788459140:D>"));
    assert!(is_placeholder_raid_name("  <t:1:d> "));
    assert!(is_placeholder_raid_name(""));
    assert!(!is_placeholder_raid_name("Seru & Emp"));
    assert!(!is_placeholder_raid_name("<t:not closed"));
}

#[test]
fn two_unnamed_back_to_back_raids_are_one_night() {
    // 2026-09-03: a false start at 18:11 (one tick), the real one at 18:12.
    let a = raid("<t:1788459116:D>", 1_000, 60_000);
    let b = raid("<t:1788459140:D>", 70_000, 3_600_000);
    assert!(same_raid(&a, &b));
    assert!(same_raid(&b, &a));
}

#[test]
fn an_unnamed_false_start_joins_the_named_raid_that_follows() {
    let a = raid("<t:1788459116:D>", 1_000, 60_000);
    let b = raid("Seru & Emp", 70_000, 3_600_000);
    assert!(same_raid(&a, &b));
}

#[test]
fn different_names_or_a_long_gap_stay_apart() {
    let a = raid("Seru", 1_000, 60_000);
    let b = raid("Emp", 70_000, 3_600_000);
    assert!(!same_raid(&a, &b));
    let c = raid("Seru", 60_000 + SAME_RAID_GAP_MS + 1, 7_200_000);
    assert!(!same_raid(&a, &c), "same name, but past the gap");
}
