//! A false `/startraid` folded into the real raid: a pure relabel. Entries
//! and log lines move, balances do not, and the phantom is gone.

#![allow(clippy::unwrap_used)]

use nocturnal_core::{Actor, Command, Ctx, Ledger, Rejection};

const GUILD: u64 = 42;

fn ctx(now_ms: i64) -> Ctx {
    Ctx {
        guild: GUILD,
        actor: Actor::User(1),
        now_ms,
    }
}

fn run(l: &mut Ledger, now: i64, cmd: Command) {
    let envs = l.propose(&ctx(now), &cmd).unwrap();
    l.commit(&envs);
}

fn start(l: &mut Ledger, now: i64, id: &str, players: Vec<u64>) {
    run(
        l,
        now,
        Command::StartRaid {
            raid_id: id.into(),
            name: "Seru & Emp".into(),
            tick_interval_ms: 60_000,
            dkp_per_tick: 1,
            players_present: players,
            event_id: None,
        },
    );
}

fn end(l: &mut Ledger, now: i64) {
    run(
        l,
        now,
        Command::EndRaid {
            players_present: vec![],
            reason: "officer".into(),
        },
    );
}

/// The Aug 31 2026 shape: a 3-minute phantom with a Start tick and one
/// loot debit, then the real raid.
fn phantom_then_real() -> Ledger {
    let mut l = Ledger::new();
    start(&mut l, 1_000, "phantom", vec![1, 2]);
    // Seed a balance the Sigil Earring can be paid from.
    run(
        &mut l,
        1_200,
        Command::AdjustDkp {
            player: 1,
            delta: 100,
            comment: "seed".into(),
            item: None,
        },
    );
    run(
        &mut l,
        1_500,
        Command::AdjustDkp {
            player: 1,
            delta: -42,
            comment: "Sigil Earring".into(),
            item: None,
        },
    );
    end(&mut l, 2_000);
    start(&mut l, 3_000, "real", vec![1, 2, 3]);
    run(
        &mut l,
        70_000,
        Command::Tick {
            players_present: vec![1, 2, 3],
        },
    );
    end(&mut l, 80_000);
    l
}

#[test]
fn merge_relabels_everything_and_moves_no_dkp() {
    let mut l = phantom_then_real();
    let before: Vec<i64> = (1..=3)
        .map(|p| l.state().guild(GUILD).unwrap().players[&p].balance)
        .collect();

    run(
        &mut l,
        90_000,
        Command::MergeRaid {
            from: "phantom".into(),
            into: "real".into(),
        },
    );
    let g = l.state().guild(GUILD).unwrap();

    assert!(!g.raids.contains_key("phantom"), "the phantom is gone");
    let real = &g.raids["real"];
    assert_eq!(
        real.date_ms, 1_000,
        "the raid now starts at the false start"
    );
    assert_eq!(
        real.entries.iter().map(|e| e.ts_ms).collect::<Vec<_>>(),
        vec![1_000, 2_000, 3_000, 70_000, 80_000],
        "entries merged in time order (adjustments are log lines, not entries)"
    );
    assert!(!real.active);
    assert_eq!(real.ended_ms, Some(80_000), "the real raid's end is kept");

    let after: Vec<i64> = (1..=3).map(|p| g.players[&p].balance).collect();
    assert_eq!(before, after, "a merge moves no DKP");

    let p1 = &g.players[&1];
    assert!(
        p1.log
            .iter()
            .all(|e| e.raid.as_ref().map(|r| r.raid_id.as_str()) == Some("real")),
        "every line, the loot debit included, now points at the real raid: {:?}",
        p1.log
    );
    let debit = p1.log.iter().find(|e| e.dkp == -42).unwrap();
    assert_eq!(debit.raid.as_ref().unwrap().name, "Seru & Emp");
}

#[test]
fn merge_refuses_active_same_and_unknown_raids() {
    let mut l = phantom_then_real();
    let merge = |l: &Ledger, from: &str, into: &str| {
        l.propose(
            &ctx(90_000),
            &Command::MergeRaid {
                from: from.into(),
                into: into.into(),
            },
        )
        .err()
    };
    assert_eq!(merge(&l, "real", "real"), Some(Rejection::SameRaid));
    assert_eq!(merge(&l, "nope", "real"), Some(Rejection::RaidNotFound));
    assert_eq!(merge(&l, "phantom", "nope"), Some(Rejection::RaidNotFound));

    start(&mut l, 100_000, "live", vec![1]);
    assert_eq!(
        merge(&l, "phantom", "live"),
        Some(Rejection::RaidStillActive {
            name: "Seru & Emp".into()
        }),
        "a running raid is not a merge target"
    );
}
