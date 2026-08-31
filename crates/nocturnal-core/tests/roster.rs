//! `roster.character.*` — the guild roster, absorbed from the roster bot.
//!
//! The roster bot's store was a Google Sheet; here it is a projection, and
//! the page is a pure function of it. So the guarantees are the ones the
//! materializer relies on: names are keys, an edit replaces the whole record,
//! a remove removes, refusals are refusals, and replay reproduces it all.

#![allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion

use nocturnal_core::{Actor, Command, Ctx, Ledger, MainRank, Rejection, RosterCharacter};

const GUILD: u64 = 42;
const P: u64 = 7;

fn ctx() -> Ctx {
    Ctx {
        guild: GUILD,
        actor: Actor::User(P),
        now_ms: 1_700_000_000_000,
    }
}

fn exec(l: &mut Ledger, cmd: Command) -> Result<(), Rejection> {
    let envs = l.propose(&ctx(), &cmd)?;
    l.commit(&envs);
    Ok(())
}

fn shaman(name: &str, level: u8) -> RosterCharacter {
    RosterCharacter {
        name: name.into(),
        class: "Shaman".into(),
        level,
        aa: None,
        profile_url: None,
        access: vec![],
        main: None,
    }
}

fn set(c: RosterCharacter, replace: bool) -> Command {
    Command::SetRosterCharacter {
        player: P,
        character: c,
        replace,
    }
}

#[test]
fn add_edit_remove_is_the_whole_lifecycle() {
    let mut l = Ledger::new();
    exec(&mut l, set(shaman("Shaku", 55), false)).unwrap();
    let got = &l.state().guild(GUILD).unwrap().roster[&P]["shaku"];
    assert_eq!(got.level, 55);

    let mut edited = shaman("Shaku", 60);
    edited.aa = Some(120);
    edited.main = Some(MainRank::Main);
    edited.access = vec!["VP".into()];
    exec(&mut l, set(edited, true)).unwrap();
    let got = &l.state().guild(GUILD).unwrap().roster[&P]["shaku"];
    assert_eq!(
        (got.level, got.aa, got.main),
        (60, Some(120), Some(MainRank::Main))
    );
    assert_eq!(got.access, vec!["VP"]);

    exec(
        &mut l,
        Command::RemoveRosterCharacter {
            player: P,
            name: "SHAKU".into(),
        },
    )
    .unwrap();
    assert!(
        !l.state().guild(GUILD).unwrap().roster.contains_key(&P),
        "a member with no characters has no roster row"
    );
}

/// The name is the key, case-insensitively — `Shaku` and `shaku` are one
/// character, as they are one character in the game.
#[test]
fn names_are_keys_regardless_of_case() {
    let mut l = Ledger::new();
    exec(&mut l, set(shaman("Shaku", 55), false)).unwrap();
    let err = exec(&mut l, set(shaman("SHAKU", 60), false)).unwrap_err();
    assert!(
        matches!(err, Rejection::RosterCharacterExists { .. }),
        "{err:?}"
    );
    exec(&mut l, set(shaman("shaku", 60), true)).unwrap();
    assert_eq!(l.state().guild(GUILD).unwrap().roster[&P].len(), 1);
}

/// Legacy semantics kept: `edit` of a character you never added says so,
/// rather than quietly adding it, and `remove` of nothing says so too.
#[test]
fn edit_and_remove_need_an_existing_character() {
    let mut l = Ledger::new();
    let err = exec(&mut l, set(shaman("Ghost", 60), true)).unwrap_err();
    assert!(
        matches!(err, Rejection::RosterCharacterMissing { .. }),
        "{err:?}"
    );
    let err = exec(
        &mut l,
        Command::RemoveRosterCharacter {
            player: P,
            name: "Ghost".into(),
        },
    )
    .unwrap_err();
    assert!(
        matches!(err, Rejection::RosterCharacterMissing { .. }),
        "{err:?}"
    );
}

/// The validation lives in the decide step, so the sheet import is held to
/// the same rules as a member typing — and each refusal names the field.
#[test]
fn values_the_roster_refuses_are_refused_by_field() {
    let cases: Vec<(&str, RosterCharacter)> = vec![
        ("class", {
            let mut c = shaman("A", 60);
            c.class = "Jedi".into();
            c
        }),
        ("level", shaman("A", 0)),
        ("level", shaman("A", 66)),
        ("aa", {
            let mut c = shaman("A", 60);
            c.aa = Some(1001);
            c
        }),
        ("quarmy_link", {
            let mut c = shaman("A", 60);
            c.profile_url = Some("https://quarmy.com.evil.tld/x".into());
            c
        }),
        ("quarmy_link", {
            let mut c = shaman("A", 60);
            c.profile_url = Some("http://quarmy.com/x".into());
            c
        }),
        ("name", shaman("", 60)),
    ];
    for (field, c) in cases {
        let mut l = Ledger::new();
        match exec(&mut l, set(c, false)) {
            Err(Rejection::InvalidRosterEntry { field: f, .. }) => assert_eq!(f, field),
            other => panic!("{field}: {other:?}"),
        }
        assert!(
            l.state().guild(GUILD).is_none() || l.state().guild(GUILD).unwrap().roster.is_empty()
        );
    }
    // And the shape that is fine.
    let mut ok = shaman("Shaku", 60);
    ok.profile_url = Some("https://quarmy.com/character/shaku".into());
    ok.aa = Some(1000);
    let mut l = Ledger::new();
    exec(&mut l, set(ok, false)).unwrap();
}

#[test]
fn replay_reproduces_the_roster_exactly() {
    let mut live = Ledger::new();
    let mut log = Vec::new();
    for cmd in [
        set(shaman("Shaku", 55), false),
        set(shaman("Eklavdra", 25), false),
        set(shaman("Shaku", 60), true),
        Command::RemoveRosterCharacter {
            player: P,
            name: "Eklavdra".into(),
        },
    ] {
        let envs = live.propose(&ctx(), &cmd).unwrap();
        live.commit(&envs);
        log.extend(envs);
    }
    let mut replayed = Ledger::new();
    for e in &log {
        replayed.replay(e);
    }
    assert_eq!(live.state().guild(GUILD), replayed.state().guild(GUILD));
}
