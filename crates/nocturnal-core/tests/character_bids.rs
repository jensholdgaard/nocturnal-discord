//! Character bids and attendance requirements (2026-09-05).
//!
//! A bid may name one of the player's roster characters; the MAIN button
//! allows the ranked ones (Main and Second), the ALT button every character
//! (a main may always bid at alt priority, 2026-09-09). Officers can also
//! require an attendance percentage per side. Both are ledger rules, so a
//! forged pick and a stale client get the same answer as the buttons.

#![allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion

use nocturnal_core::event::{ConfigPatch, Flavor};
use nocturnal_core::{Actor, Command, Ctx, Item, Ledger, MainRank, Rejection, RosterCharacter};

const GUILD: u64 = 42;
const P: u64 = 7;
const NOW: i64 = 1_700_000_000_000;

fn ctx(now_ms: i64) -> Ctx {
    Ctx {
        guild: GUILD,
        actor: Actor::User(P),
        now_ms,
    }
}

fn exec(l: &mut Ledger, now_ms: i64, cmd: Command) -> Result<(), Rejection> {
    let envs = l.propose(&ctx(now_ms), &cmd)?;
    l.commit(&envs);
    Ok(())
}

fn toon(name: &str, class: &str, main: Option<MainRank>) -> RosterCharacter {
    RosterCharacter {
        name: name.into(),
        class: class.into(),
        level: 60,
        aa: None,
        profile_url: None,
        access: vec![],
        main,
    }
}

fn bid(character: Option<&str>, for_main: bool) -> Command {
    Command::PlaceBid {
        auction_id: "au-1".into(),
        player: P,
        amount: 10,
        for_main,
        character: character.map(str::to_owned),
    }
}

/// A player with 100 DKP, three roster characters and one open auction.
fn ledger() -> Ledger {
    let mut l = Ledger::new();
    exec(
        &mut l,
        NOW,
        Command::ImportPlayer {
            player: P,
            balance: 100,
            characters: vec![],
            creation_ts_ms: 1,
            log: vec![],
            legacy_id: None,
        },
    )
    .unwrap();
    // Ranked rows are the officers' doing (2026-09-08): seeded as the system,
    // the way a sheet import wrote them, since a member's own write may not
    // carry a rank.
    for c in [
        toon("Vexira", "Wizard", Some(MainRank::Main)),
        toon("Solenne", "Enchanter", Some(MainRank::Second)),
        toon("Thurgo", "Warrior", None),
    ] {
        let system = Ctx {
            guild: GUILD,
            actor: Actor::System,
            now_ms: NOW,
        };
        let envs = l
            .propose(
                &system,
                &Command::SetRosterCharacter {
                    player: P,
                    character: c,
                    replace: false,
                },
            )
            .unwrap();
        l.commit(&envs);
    }
    exec(
        &mut l,
        NOW,
        Command::OpenAuction {
            auction_id: "au-1".into(),
            item: Item {
                id: "26780".into(),
                name: "Tome of Secrets".into(),
                url: None,
                data: None,
                image: None,
            },
            flavor: Flavor::Short,
            min_bid: 1,
            num_items: 1,
            min_bid_to_lock_for_main: 0,
            over_bid_to_win_main: 0,
            duration_ms: 60_000,
            debit_dkp: true,
        },
    )
    .unwrap();
    l
}

fn config(l: &mut Ledger, patch: ConfigPatch) {
    exec(l, NOW, Command::UpdateConfig { patch }).unwrap();
}

#[test]
fn the_main_button_offers_the_main_and_the_alt_button_the_rest() {
    let l = ledger();
    let g = l.state().guild(GUILD).unwrap();
    let names = |for_main: bool| -> Vec<String> {
        g.bid_characters(P, for_main)
            .iter()
            .map(|c| c.name.clone())
            .collect()
    };
    assert_eq!(names(true), vec!["Solenne", "Vexira"], "main and second");
    assert_eq!(
        names(false),
        vec!["Solenne", "Thurgo", "Vexira"],
        "every character may bid as an alt"
    );
}

#[test]
fn a_bid_carries_its_character_into_the_winner_line() {
    let mut l = ledger();
    exec(&mut l, NOW, bid(Some("Vexira"), true)).unwrap();
    let g = l.state().guild(GUILD).unwrap();
    assert_eq!(
        g.auctions["au-1"].bids[0].character.as_deref(),
        Some("Vexira")
    );
    let winners = nocturnal_core::compute_winners(g, "au-1", 1);
    assert_eq!(winners[0].character.as_deref(), Some("Vexira"));
}

#[test]
fn a_bid_without_a_character_is_still_a_bid() {
    // Off by default, and every bid from before the feature: nothing named.
    let mut l = ledger();
    exec(&mut l, NOW, bid(None, true)).unwrap();
    let g = l.state().guild(GUILD).unwrap();
    assert_eq!(g.auctions["au-1"].bids[0].character, None);
}

#[test]
fn the_side_decides_which_characters_a_bid_may_name() {
    let mut l = ledger();
    assert_eq!(
        exec(&mut l, NOW, bid(Some("Thurgo"), true)),
        Err(Rejection::CharacterNotEligible {
            name: "Thurgo".into(),
            for_main: true
        }),
        "an unranked character cannot bid as MAIN"
    );
    exec(&mut l, NOW, bid(Some("Vexira"), false)).expect("a main may bid as ALT (2026-09-09)");
    let mut l = ledger();
    exec(&mut l, NOW, bid(Some("Solenne"), true)).expect("a second main bids as MAIN");
    assert_eq!(
        exec(&mut l, NOW, bid(Some("Nobody"), true)),
        Err(Rejection::RosterCharacterMissing {
            name: "Nobody".into()
        }),
        "a character not on the row"
    );
    exec(&mut l, NOW, bid(Some("thurgo"), false)).unwrap();
    let g = l.state().guild(GUILD).unwrap();
    assert_eq!(
        g.auctions["au-1"].bids[0].character.as_deref(),
        Some("thurgo"),
        "names match case-insensitively and are stored as typed"
    );
}

#[test]
fn an_attendance_requirement_refuses_the_side_it_names() {
    let mut l = ledger();
    // No raids at all → attendance is 100 %, so any requirement passes.
    config(
        &mut l,
        ConfigPatch {
            main_bid_min_attendance: Some(50),
            ..Default::default()
        },
    );
    exec(&mut l, NOW, bid(None, true)).unwrap();

    // One raid week the player missed entirely: 0 %.
    exec(
        &mut l,
        NOW,
        Command::StartRaid {
            raid_id: "r1".into(),
            name: "Vulak".into(),
            tick_interval_ms: 60_000,
            dkp_per_tick: 1,
            players_present: vec![P + 1],
            event_id: None,
        },
    )
    .unwrap();
    // A main bid under the main line is taken as an alt bid (2026-09-09),
    // not refused: the member can still bid, at alt priority.
    exec(&mut l, NOW + 2, bid(Some("Vexira"), true)).unwrap();
    let g = l.state().guild(GUILD).unwrap();
    let landed = g.auctions["au-1"].bids.last().unwrap();
    assert!(!landed.for_main, "landed on the ALT side");
    assert_eq!(landed.character.as_deref(), Some("Vexira"));
    assert_eq!(landed.attendance as i64, 0);
    // The ALT side has its own threshold, unset here.
    exec(&mut l, NOW + 2, bid(None, false)).unwrap();
    config(
        &mut l,
        ConfigPatch {
            alt_bid_min_attendance: Some(10),
            ..Default::default()
        },
    );
    assert!(matches!(
        exec(&mut l, NOW + 3, bid(None, false)),
        Err(Rejection::AttendanceBelowMinimum {
            for_main: false,
            ..
        })
    ));
}

#[test]
fn the_requirement_is_a_percentage() {
    let mut l = ledger();
    for (patch, setting) in [
        (
            ConfigPatch {
                main_bid_min_attendance: Some(101),
                ..Default::default()
            },
            "mainbidminra",
        ),
        (
            ConfigPatch {
                alt_bid_min_attendance: Some(-1),
                ..Default::default()
            },
            "altbidminra",
        ),
    ] {
        match exec(&mut l, NOW, Command::UpdateConfig { patch }) {
            Err(Rejection::InvalidConfig { setting: s, .. }) => assert_eq!(s, setting),
            other => panic!("accepted a bad percentage: {other:?}"),
        }
    }
}

/// Bubblie, 2026-09-08: a level-20 alt won a bid. A minimum level per side,
/// 0 by default, gates only bids that name a character (the ledger knows no
/// level for a bare bid) and reads the roster's level.
#[test]
fn a_minimum_level_refuses_the_side_it_names() {
    let mut l = ledger();
    // Thurgo the warrior is level 60 in the fixture; a low alt is added.
    let system = Ctx {
        guild: GUILD,
        actor: Actor::System,
        now_ms: NOW,
    };
    let envs = l
        .propose(
            &system,
            &Command::SetRosterCharacter {
                player: P,
                character: toon("Mymm", "Rogue", None),
                replace: false,
            },
        )
        .unwrap();
    l.commit(&envs);
    let mut low = l.state().guild(GUILD).unwrap().roster[&P]["mymm"].clone();
    low.level = 20;
    let envs = l
        .propose(
            &system,
            &Command::SetRosterCharacter {
                player: P,
                character: low,
                replace: true,
            },
        )
        .unwrap();
    l.commit(&envs);
    config(
        &mut l,
        ConfigPatch {
            alt_bid_min_level: Some(55),
            ..Default::default()
        },
    );
    assert_eq!(
        exec(&mut l, NOW, bid(Some("Mymm"), false)),
        Err(Rejection::CharacterBelowMinLevel {
            name: "Mymm".into(),
            level: 20,
            required: 55,
            for_main: false
        })
    );
    exec(&mut l, NOW, bid(Some("Thurgo"), false)).expect("a 60 alt bids");
    exec(&mut l, NOW, bid(None, false)).expect("a bare bid names no level to check");
    assert!(
        matches!(
            exec(
                &mut l,
                NOW,
                Command::UpdateConfig {
                    patch: ConfigPatch {
                        alt_bid_min_level: Some(70),
                        ..Default::default()
                    }
                }
            ),
            Err(Rejection::InvalidConfig { .. })
        ),
        "65 is the cap"
    );
}

#[test]
fn the_toggle_is_config_like_any_other() {
    let mut l = ledger();
    assert!(!l.state().guild(GUILD).unwrap().config.character_bids);
    config(
        &mut l,
        ConfigPatch {
            character_bids: Some(true),
            ..Default::default()
        },
    );
    assert!(l.state().guild(GUILD).unwrap().config.character_bids);
    config(
        &mut l,
        ConfigPatch {
            min_bid: Some(3),
            ..Default::default()
        },
    );
    assert!(
        l.state().guild(GUILD).unwrap().config.character_bids,
        "an unrelated patch leaves the toggle alone"
    );
}
