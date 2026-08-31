//! A raid's window is /startraid to /endraid — the envelope timestamps —
//! not first tick to last tick plus a guess. The site and the raid info
//! series both rely on `ended_ms`.

#![allow(clippy::unwrap_used)]

use nocturnal_core::{Actor, Command, Ctx, Ledger};

const GUILD: u64 = 42;

fn ctx(now_ms: i64) -> Ctx {
    Ctx {
        guild: GUILD,
        actor: Actor::User(1),
        now_ms,
    }
}

#[test]
fn ending_a_raid_records_when() {
    let mut l = Ledger::new();
    let run = |l: &mut Ledger, now: i64, cmd: Command| {
        let envs = l.propose(&ctx(now), &cmd).unwrap();
        l.commit(&envs);
    };
    run(
        &mut l,
        1_000_000,
        Command::StartRaid {
            raid_id: "r1".into(),
            name: "Vulak".into(),
            tick_interval_ms: 60_000,
            dkp_per_tick: 1,
            players_present: vec![1],
            event_id: None,
        },
    );
    let g = l.state().guild(GUILD).unwrap();
    let id = g.active_raid.clone().unwrap();
    assert_eq!(g.raids[&id].ended_ms, None, "still running");

    run(
        &mut l,
        5_000_000,
        Command::EndRaid {
            players_present: vec![1],
            reason: "done".into(),
        },
    );
    let g = l.state().guild(GUILD).unwrap();
    assert_eq!(
        g.raids[&id].ended_ms,
        Some(5_000_000),
        "the /endraid timestamp"
    );
    assert!(!g.raids[&id].active);
}
