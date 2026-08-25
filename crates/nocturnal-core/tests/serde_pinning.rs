//! Serde round-trips plus pinned `kind` strings. These JSON shapes are the
//! wire format of the ledger: append-only forever. If a change here breaks a
//! test, that change breaks replay of production logs — version instead.

#![allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion

use nocturnal_core::event::{
    Actor, ConfigPatch, Envelope, Event, Flavor, ImportedAttendance, ImportedLogEntry, Item,
    RaidRef, Winner,
};

fn item() -> Item {
    Item {
        id: "9".into(),
        name: "Cloak".into(),
        url: Some("https://x".into()),
        data: Some("WT: 5".into()),
        image: None,
    }
}

fn samples() -> Vec<Event> {
    vec![
        Event::CharacterLinked {
            player: 1,
            character: "Dest".into(),
        },
        Event::PlayerImported {
            player: 1,
            balance: 5,
            characters: vec!["Dest".into()],
            creation_ts_ms: 7,
            log: vec![ImportedLogEntry {
                dkp: 5,
                comment: "import".into(),
                ts_ms: 7,
                raid: Some(RaidRef {
                    raid_id: "r".into(),
                    name: "R".into(),
                }),
                item: None,
            }],
        },
        Event::DkpAdjusted {
            player: 1,
            delta: -3,
            comment: "loot".into(),
            raid: None,
            item: Some(item()),
        },
        Event::RaidStarted {
            raid_id: "r".into(),
            name: "Naggy".into(),
            tick_interval_ms: 1,
            dkp_per_tick: 1,
            event_id: Some("e".into()),
        },
        Event::RaidAwarded {
            raid_id: "r".into(),
            players: vec![1, 2],
            amount: 1,
            comment: "Start".into(),
        },
        Event::RaidTicked {
            raid_id: "r".into(),
            tick_no: 3,
            players: vec![1],
            amount: 1,
        },
        Event::RaidEnded {
            raid_id: "r".into(),
            reason: "officer".into(),
        },
        Event::RaidImported {
            raid_id: "r0".into(),
            name: "Old".into(),
            date_ms: 1,
            entries: vec![ImportedAttendance {
                players: vec![1],
                comment: "Tick".into(),
                ts_ms: 1,
                amount: 1,
            }],
        },
        Event::AuctionOpened {
            auction_id: "a".into(),
            item: item(),
            flavor: Flavor::Long,
            min_bid: 0,
            num_items: 2,
            min_bid_to_lock_for_main: 10,
            over_bid_to_win_main: 100,
            deadline_ts_ms: 99,
        },
        Event::BidPlaced {
            auction_id: "a".into(),
            player: 1,
            amount: 5,
            for_main: true,
            attendance: 87.5,
        },
        Event::BidRetracted {
            auction_id: "a".into(),
            player: 1,
        },
        Event::AuctionClosed {
            auction_id: "a".into(),
        },
        Event::AuctionFinalized {
            auction_id: "a".into(),
            winners: vec![Winner {
                player: 1,
                amount: 5,
                for_main: true,
            }],
            seed: 42,
        },
        Event::AuctionCancelled {
            auction_id: "a".into(),
            reason: "officer".into(),
        },
        Event::ConfigUpdated {
            patch: ConfigPatch {
                min_bid: Some(5),
                ..Default::default()
            },
        },
        Event::TelemetryTokenIssued {
            username: "jens".into(),
            token_fp: "t".into(),
            role: "editor".into(),
        },
        Event::TelemetryAccessUpdated {
            username: "jens".into(),
            role: "viewer".into(),
        },
        Event::TelemetryTokenRevoked {
            username: "jens".into(),
        },
    ]
}

/// Wire `kind` strings, pinned. Extending is fine; changing one is a replay break.
const PINNED_KINDS: &[&str] = &[
    "player.character_linked",
    "player.imported",
    "dkp.adjusted",
    "raid.started",
    "raid.awarded",
    "raid.tick",
    "raid.ended",
    "raid.imported",
    "auction.opened",
    "auction.bid_placed",
    "auction.bid_retracted",
    "auction.closed",
    "auction.finalized",
    "auction.cancelled",
    "config.updated",
    "telemetry.token.issued",
    "telemetry.access.updated",
    "telemetry.token.revoked",
];

#[test]
fn every_event_round_trips_and_kind_is_pinned() {
    let events = samples();
    assert_eq!(
        events.len(),
        PINNED_KINDS.len(),
        "add new kinds to PINNED_KINDS"
    );
    for (event, expected_kind) in events.into_iter().zip(PINNED_KINDS) {
        let env = Envelope {
            seq: 3,
            ts_ms: 1_700_000_000_000,
            guild: 42,
            actor: Actor::User(7),
            v: 1,
            correlation_id: Some("c".into()),
            event,
        };
        let json = serde_json::to_string(&env).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["kind"], *expected_kind, "kind string drifted");
        assert_eq!(env.event.kind(), *expected_kind, "Event::kind() drifted");
        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, env, "round-trip changed the event");
    }
}

#[test]
fn v_defaults_to_1_when_absent() {
    let json = r#"{"seq":0,"ts_ms":1,"guild":1,"actor":"system","kind":"auction.closed","payload":{"auction_id":"a"}}"#;
    let env: Envelope = serde_json::from_str(json).unwrap();
    assert_eq!(env.v, 1);
}
