//! `/backup` gives back what the migration was given.
//!
//! The guild's roster page parses these two files, and the legacy bot is what
//! taught it the shape — so "close enough" is not a passing grade. The test is
//! the round trip: legacy documents → genesis events → projection → export,
//! compared against the documents we started from.

#![allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion

use nocturnal_core::Ledger;
use nocturnal_migrate::{export, genesis_commands, run_genesis, LegacyPlayer, LegacyRaid};
use serde_json::Value;

const NOW: i64 = 1_713_000_000_000;

const PLAYERS: &str = r#"[
  {"_id":"6619000000000000000000a1","player":"111","guild":"42","current":666,
   "creationDate":1712944829568,"characters":["Dest"],
   "log":[
     {"dkp":700,"comment":"Tick","date":1712944829568,"raid":{"_id":"661976bd","name":"Plane of Air"}},
     {"dkp":-34,"comment":"Symbol of Veeshan","date":1712950000000,"raid":null,
      "item":{"id":"20847","name":"Symbol of Veeshan","data":"WT: 5","image":"https://x/icon.png","url":"https://x/item"}}
   ]},
  {"_id":"6619000000000000000000a2","player":"222","guild":"42","current":10,
   "creationDate":1712944829568,
   "log":[{"dkp":10,"comment":"Start","date":1712944829568,"raid":{"_id":"661976bd","name":"Plane of Air"}}]}
]"#;

const RAIDS: &str = r#"[
  {"_id":"661976bd","guild":"42","name":"Plane of Air","date":1712944829490,
   "tickDuration":360000,"dkpsPerTick":1,"active":false,"deprecated":false,
   "eventId":"1234567890",
   "attendance":[{"players":["111","222"],"comment":"Start","date":1712944829490,"dkps":1}]}
]"#;

/// Import, then export, then compare — field by field, both directions, so a
/// dropped key fails as loudly as an invented one.
fn round_trip(players_json: &str, raids_json: &str) -> (Value, Value) {
    let players: Vec<LegacyPlayer> = serde_json::from_str(players_json).unwrap();
    let raids: Vec<LegacyRaid> = serde_json::from_str(raids_json).unwrap();
    let (guild, commands, _) = genesis_commands(&players, &raids, None);

    let mut ledger = Ledger::new();
    let (_, _, mismatches) = run_genesis(&mut ledger, guild, &commands, &players, NOW);
    assert_eq!(
        mismatches, 0,
        "the import itself disagreed with the snapshot"
    );

    let g = ledger.state().guild(guild).unwrap();
    (
        serde_json::to_value(export::players(g, guild)).unwrap(),
        serde_json::to_value(export::raids(g, guild, NOW)).unwrap(),
    )
}

#[test]
fn an_exported_player_is_the_document_it_came_from() {
    let (players, _) = round_trip(PLAYERS, RAIDS);
    let expected: Value = serde_json::from_str(PLAYERS).unwrap();
    assert_eq!(players, expected, "the player documents changed shape");
}

#[test]
fn an_exported_raid_is_the_document_it_came_from() {
    let (_, raids) = round_trip(PLAYERS, RAIDS);
    let expected: Value = serde_json::from_str(RAIDS).unwrap();
    assert_eq!(raids, expected, "the raid documents changed shape");
}

/// `deprecated` is stored in legacy and derived here. A raid outside the
/// window has to come back marked, or the roster page counts attendance
/// against raids the bot itself no longer counts.
#[test]
fn deprecation_is_derived_from_the_window_not_from_a_stored_flag() {
    let old_raid = RAIDS.replace("\"date\":1712944829490", "\"date\":1");
    let players = PLAYERS
        .replace("1712944829568", "1")
        .replace("1712950000000", "2");
    let (_, raids) = round_trip(&players, &old_raid);
    assert_eq!(raids[0]["deprecated"], Value::Bool(true));

    let (_, fresh) = round_trip(PLAYERS, RAIDS);
    assert_eq!(fresh[0]["deprecated"], Value::Bool(false));
}

/// Absent keys must stay absent. `characters` is on 3 of 281 real players and
/// `item` on 2 % of log lines; emitting them as `[]`/`null` everywhere would
/// quadruple the file and hand the roster page keys it has never seen.
#[test]
fn keys_the_legacy_documents_omit_are_omitted() {
    let minimal = r#"[{"player":"333","guild":"42","current":0,"creationDate":1,"log":[]}]"#;
    let (players, _) = round_trip(minimal, "[]");
    let obj = players[0].as_object().unwrap();
    assert!(!obj.contains_key("characters"), "{obj:?}");
    assert!(!obj.contains_key("_id"), "no Mongo id to invent: {obj:?}");
    assert_eq!(obj["log"], serde_json::json!([]));
}

/// The real snapshot, when it is on this machine. `localdata/` is gitignored
/// (it holds live player ids), so CI runs the miniature above and a developer
/// with the dump gets the whole guild checked instead.
#[test]
fn the_real_snapshot_round_trips() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../localdata/fresh");
    if !dir.join("players.json").exists() {
        eprintln!("skipping: {} not present", dir.display());
        return;
    }
    let players_raw = std::fs::read_to_string(dir.join("players.json")).unwrap();
    let raids_raw = std::fs::read_to_string(dir.join("raids.json")).unwrap();

    let players: Vec<LegacyPlayer> = serde_json::from_str(&players_raw).unwrap();
    let raids: Vec<LegacyRaid> = serde_json::from_str(&raids_raw).unwrap();
    let (guild, commands, _) = genesis_commands(&players, &raids, None);
    let mut ledger = Ledger::new();
    run_genesis(&mut ledger, guild, &commands, &players, NOW);
    let g = ledger.state().guild(guild).unwrap();

    // Players the importer refuses (ids that are not snowflakes) never reach
    // the ledger, so compare against the ones that did.
    let exported: Value = serde_json::to_value(export::players(g, guild)).unwrap();
    let mut expected: Vec<Value> = serde_json::from_str(&players_raw).unwrap();
    // Legacy writes `"item": null` on 980 of 501,506 log lines and omits the
    // key on the other 491,758. The ledger stores one absence, not two, so it
    // gives back the majority spelling. Every JSON reader treats the two
    // identically, and `raid` — which legacy writes on *every* line — is
    // reproduced exactly, null and all.
    for p in &mut expected {
        if let Some(log) = p["log"].as_array_mut() {
            for e in log {
                if e.get("item").is_some_and(Value::is_null) {
                    e.as_object_mut().unwrap().remove("item");
                }
                // 784 lines carry `"raid": {"_id": null, "name": null}` — a
                // reference to nothing, written by a legacy path that built
                // the object before it knew whether there was a raid. It
                // means the same as the 1,720 plain nulls beside it, and that
                // is the spelling it comes back as.
                if e["raid"].get("_id").is_some_and(Value::is_null) {
                    e["raid"] = Value::Null;
                }
            }
        }
    }
    expected.retain(|p| {
        p["player"]
            .as_str()
            .is_some_and(|s| s.parse::<u64>().is_ok())
    });
    expected.sort_by_key(|p| p["player"].as_str().unwrap().parse::<u64>().unwrap());
    assert_eq!(
        exported.as_array().unwrap().len(),
        expected.len(),
        "player count"
    );
    for (got, want) in exported.as_array().unwrap().iter().zip(&expected) {
        let (g, w) = (got.as_object().unwrap(), want.as_object().unwrap());
        for key in w.keys().chain(g.keys()) {
            if key == "log" {
                let (gl, wl) = (g["log"].as_array().unwrap(), w["log"].as_array().unwrap());
                assert_eq!(gl.len(), wl.len(), "player {}: log length", w["player"]);
                for (i, (a, b)) in gl.iter().zip(wl).enumerate() {
                    assert_eq!(a, b, "player {} log[{i}]", w["player"]);
                }
            } else {
                assert_eq!(g.get(key), w.get(key), "player {} field {key}", w["player"]);
            }
        }
    }

    let exported_raids: Value = serde_json::to_value(export::raids(g, guild, NOW)).unwrap();
    assert_eq!(exported_raids.as_array().unwrap().len(), raids.len());
}
