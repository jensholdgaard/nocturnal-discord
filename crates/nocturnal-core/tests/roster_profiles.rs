//! Uploaded character profiles (2026-09-08): a member's Zeal export, stored
//! as the JSON a client's profile event would carry.

#![allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion

use nocturnal_core::{Actor, Command, Ctx, Ledger, ProfileSource, Rejection, RosterCharacter};

const GUILD: u64 = 42;
const P: u64 = 7;
const NOW: i64 = 1_700_000_000_000;

fn exec(l: &mut Ledger, now_ms: i64, cmd: Command) -> Result<(), Rejection> {
    let ctx = Ctx {
        guild: GUILD,
        actor: Actor::User(P),
        now_ms,
    };
    let envs = l.propose(&ctx, &cmd)?;
    l.commit(&envs);
    Ok(())
}

fn body(name: &str, head: i64) -> String {
    format!(
        r#"{{"name":"{name}","level":60,"class":14,"race":12,"equipment":[{{"slot":"Head","id":{head},"name":"x"}}]}}"#
    )
}

fn upload(name: &str, body: String) -> Command {
    Command::UploadRosterProfile {
        player: P,
        name: name.into(),
        source: ProfileSource::QuarmyFile,
        body,
    }
}

fn ledger_with_ziglax() -> Ledger {
    let mut l = Ledger::new();
    exec(
        &mut l,
        NOW,
        Command::SetRosterCharacter {
            player: P,
            character: RosterCharacter {
                name: "Ziglax".into(),
                class: "Enchanter".into(),
                level: 60,
                aa: None,
                profile_url: None,
                access: vec![],
                main: None,
            },
            replace: false,
        },
    )
    .unwrap();
    l
}

#[test]
fn an_upload_needs_the_character_on_the_row() {
    let mut l = ledger_with_ziglax();
    assert_eq!(
        exec(&mut l, NOW, upload("Zigmar", body("Zigmar", 1))),
        Err(Rejection::RosterCharacterMissing {
            name: "Zigmar".into()
        })
    );
}

#[test]
fn the_body_must_be_a_profile_for_that_character() {
    let mut l = ledger_with_ziglax();
    let mut bad = |cmd: Command| {
        matches!(
            exec(&mut l, NOW, cmd),
            Err(Rejection::InvalidRosterEntry {
                field: "profile",
                ..
            })
        )
    };
    assert!(bad(upload("Ziglax", "not json".into())), "not JSON");
    assert!(
        bad(upload("Ziglax", body("Zigmar", 1))),
        "another character's"
    );
    assert!(
        bad(upload("Ziglax", r#"{"name":"Ziglax"}"#.into())),
        "no equipment"
    );
    assert!(
        bad(upload(
            "Ziglax",
            format!(
                r#"{{"name":"Ziglax","equipment":[],"pad":"{}"}}"#,
                "x".repeat(70_000)
            )
        )),
        "too large"
    );
}

#[test]
fn the_newest_upload_wins_and_case_does_not_matter() {
    let mut l = ledger_with_ziglax();
    exec(&mut l, NOW, upload("ziglax", body("Ziglax", 1))).unwrap();
    exec(&mut l, NOW + 1000, upload("Ziglax", body("ziglax", 2))).unwrap();
    let g = l.state().guild(GUILD).unwrap();
    let p = &g.profiles["ziglax"];
    assert_eq!(p.uploaded_ms, NOW + 1000);
    assert!(p.body.contains(r#""id":2"#));
    assert_eq!(p.source, ProfileSource::QuarmyFile);
    assert_eq!(p.player, P);
    assert_eq!(g.profiles.len(), 1);
}
