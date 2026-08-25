//! `telemetry.*` — the dpsbot successor's ledger half (M8).
//!
//! The derived files (`tokens.txt`, the Perses provisioning YAMLs) are a pure
//! function of this projection, so every guarantee the materializer relies on
//! has to hold here first.

#![allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion

use nocturnal_core::{Actor, Command, Ctx, Ledger, Rejection};

const GUILD: u64 = 42;

fn ctx() -> Ctx {
    Ctx {
        guild: GUILD,
        actor: Actor::System,
        now_ms: 1_700_000_000_000,
    }
}

/// Run a command all the way through decide → commit.
fn exec(ledger: &mut Ledger, cmd: Command) -> Result<(), Rejection> {
    let envelopes = ledger.propose(&ctx(), &cmd)?;
    ledger.commit(&envelopes);
    Ok(())
}

fn issue(user: &str, token_fp: &str, role: &str) -> Command {
    Command::IssueToken {
        username: user.to_owned(),
        token_fp: token_fp.to_owned(),
        role: role.to_owned(),
    }
}

#[test]
fn issue_then_refresh_then_revoke_walks_the_whole_lifecycle() {
    let mut l = Ledger::new();
    exec(&mut l, issue("ziglax", "zzz", "viewer")).unwrap();

    let grant = l
        .state()
        .guild(GUILD)
        .unwrap()
        .telemetry
        .get("ziglax")
        .cloned();
    assert_eq!(grant.as_ref().map(|g| g.token_fp.as_str()), Some("zzz"));
    assert_eq!(grant.as_ref().map(|g| g.role.as_str()), Some("viewer"));

    exec(
        &mut l,
        Command::RefreshAccess {
            username: "ziglax".into(),
            role: "editor".into(),
        },
    )
    .unwrap();
    let g = l.state().guild(GUILD).unwrap();
    assert_eq!(g.telemetry["ziglax"].role, "editor");
    assert_eq!(
        g.telemetry["ziglax"].token_fp, "zzz",
        "a role refresh must never rotate the token — the member's client keeps using the old one"
    );

    exec(
        &mut l,
        Command::RevokeToken {
            username: "ziglax".into(),
        },
    )
    .unwrap();
    assert!(!l
        .state()
        .guild(GUILD)
        .unwrap()
        .telemetry
        .contains_key("ziglax"));
}

/// Issuing twice is the legacy bot's "you already have a token" path. It must
/// be a typed refusal, not a second token that silently orphans the first —
/// the orphan would stay valid in `tokens.txt` forever.
#[test]
fn a_second_issue_is_refused_rather_than_orphaning_the_first_token() {
    let mut l = Ledger::new();
    exec(&mut l, issue("magis", "aaa", "viewer")).unwrap();
    let err = exec(&mut l, issue("magis", "bbb", "editor")).unwrap_err();
    assert!(
        matches!(err, Rejection::AlreadyProvisioned { .. }),
        "got {err:?}"
    );
    assert_eq!(
        l.state().guild(GUILD).unwrap().telemetry["magis"].token_fp,
        "aaa",
        "the original token survived"
    );
}

/// Refresh and revoke against someone who has nothing are refusals, not
/// no-ops, so `/dpsrevoke` on a stranger says so instead of appearing to work.
#[test]
fn refresh_and_revoke_require_an_existing_grant() {
    let mut l = Ledger::new();
    for cmd in [
        Command::RefreshAccess {
            username: "nobody".into(),
            role: "viewer".into(),
        },
        Command::RevokeToken {
            username: "nobody".into(),
        },
    ] {
        let err = exec(&mut l, cmd).unwrap_err();
        assert!(
            matches!(err, Rejection::NotProvisioned { .. }),
            "got {err:?}"
        );
    }
}

/// The materializer deletes a member's line from a file it shares with service
/// credentials. It may only do that for names the ledger owns, so the managed
/// set has to outlive the grant — otherwise a revoked member's stale line
/// becomes indistinguishable from the bot's own token and is kept forever.
#[test]
fn a_revoked_member_stays_in_the_managed_set() {
    let mut l = Ledger::new();
    exec(&mut l, issue("ziglax", "zzz", "viewer")).unwrap();
    exec(
        &mut l,
        Command::RevokeToken {
            username: "ziglax".into(),
        },
    )
    .unwrap();

    let g = l.state().guild(GUILD).unwrap();
    assert!(!g.telemetry.contains_key("ziglax"), "the grant is gone");
    assert!(
        g.telemetry_managed.contains("ziglax"),
        "the name must stay managed, or its line can never be removed"
    );
}

/// Re-issuing after a revoke is the documented recovery path ("ask an officer
/// to /dpsrevoke you first"), so it must be allowed and must land a new token.
#[test]
fn a_member_can_be_reissued_after_a_revoke() {
    let mut l = Ledger::new();
    exec(&mut l, issue("magis", "aaa", "viewer")).unwrap();
    exec(
        &mut l,
        Command::RevokeToken {
            username: "magis".into(),
        },
    )
    .unwrap();
    exec(&mut l, issue("magis", "ccc", "editor")).unwrap();

    let g = l.state().guild(GUILD).unwrap();
    assert_eq!(g.telemetry["magis"].token_fp, "ccc");
    assert_eq!(g.telemetry["magis"].role, "editor");
}

/// The projection is the only input the files have, so replaying the same log
/// must reproduce it exactly — including the managed set, whose whole job is
/// to survive events that remove things.
#[test]
fn replay_reproduces_the_projection_exactly() {
    let mut live = Ledger::new();
    let mut log = Vec::new();
    for cmd in [
        issue("ziglax", "zzz", "viewer"),
        issue("magis", "mmm", "viewer"),
        Command::RefreshAccess {
            username: "ziglax".into(),
            role: "editor".into(),
        },
        Command::RevokeToken {
            username: "magis".into(),
        },
    ] {
        let envelopes = live.propose(&ctx(), &cmd).unwrap();
        live.commit(&envelopes);
        log.extend(envelopes);
    }

    let mut replayed = Ledger::new();
    for env in &log {
        replayed.replay(env);
    }
    assert_eq!(
        live.state().guild(GUILD),
        replayed.state().guild(GUILD),
        "replay diverged from the live projection"
    );
}
