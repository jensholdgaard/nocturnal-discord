//! Migration round-trip on a miniature legacy snapshot (same shapes as the
//! real backup files, including the negative-balance and missing-field cases).

#![allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion

use nocturnal_core::Ledger;
use nocturnal_migrate::{genesis_commands, run_genesis, LegacyPlayer, LegacyRaid};

const PLAYERS: &str = r#"[
  {"player":"111","guild":"42","current":666,"creationDate":1712944829568,
   "log":[
     {"dkp":700,"comment":"Tick","date":1712944829568,"raid":{"_id":"661976bd","name":"Plane of Air"}},
     {"dkp":-34,"comment":"Symbol of Veeshan","date":1712950000000,
      "item":{"id":"20847","name":"Symbol of Veeshan","data":"WT: 5","image":"https://x/icon.png","url":"https://x/item"}}
   ]},
  {"player":"222","guild":"42","current":-53,"creationDate":1712944829568,
   "log":[{"dkp":-53,"comment":"double spend","date":1712944829568}]},
  {"player":"not-a-snowflake","guild":"42","current":5,"log":[]}
]"#;

const RAIDS: &str = r#"[
  {"guild":"42","name":"Plane of Air","date":1712944829490,"tickDuration":360000,
   "dkpsPerTick":1,"active":false,"deprecated":true,
   "attendance":[{"players":["111","222"],"comment":"Start","date":1712944829490,"dkps":1}]}
]"#;

#[test]
fn genesis_round_trips_and_verifies() {
    let players: Vec<LegacyPlayer> = serde_json::from_str(PLAYERS).unwrap();
    let raids: Vec<LegacyRaid> = serde_json::from_str(RAIDS).unwrap();
    let (guild, commands, warnings) = genesis_commands(&players, &raids, None);
    assert_eq!(guild, 42);
    assert_eq!(commands.len(), 3, "1 raid + 2 importable players");
    assert_eq!(warnings.len(), 2, "bad id + negative balance: {warnings:?}");

    let mut ledger = Ledger::new();
    let (envelopes, lines, mismatches) =
        run_genesis(&mut ledger, guild, &commands, &players, 1_713_000_000_000);
    assert_eq!(mismatches, 0);
    assert_eq!(lines.len(), 2);
    assert_eq!(envelopes.len(), 3);

    let g = ledger.state().guild(42).unwrap();
    assert_eq!(g.balance(111), 666);
    assert_eq!(g.balance(222), -53, "legacy damage imported honestly");
    let p = &g.players[&111];
    assert_eq!(p.log.len(), 2);
    assert_eq!(p.log[1].item.as_ref().unwrap().name, "Symbol of Veeshan");
    assert_eq!(p.log[0].raid.as_ref().unwrap().name, "Plane of Air");
    assert_eq!(g.raids["legacy-0000"].entries.len(), 1);

    let mut replayed = Ledger::new();
    for env in &envelopes {
        replayed.replay(env);
    }
    assert_eq!(replayed, ledger);
}
