//! `config.updated` — the settings every other behaviour reads.
//!
//! A bad value here is not felt here: a zero tick interval surfaces at
//! `/startraid`, a deprecation window of nothing empties `/listplayersdkps`,
//! and a raid channel listed twice doubles everyone's tick. So
//! the guarantees are that the defaults are the *fixed* ones (the legacy
//! fallbacks were wrong), that a patch touches only what it names, and that
//! the values the ledger refuses are refused where they are typed.

#![allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion

use nocturnal_core::event::ConfigPatch;
use nocturnal_core::state::{GuildConfig, DAY_MS};
use nocturnal_core::{Actor, Command, Ctx, Ledger, Rejection, Secret};

const GUILD: u64 = 42;

fn ctx() -> Ctx {
    Ctx {
        guild: GUILD,
        actor: Actor::System,
        now_ms: 1_700_000_000_000,
    }
}

fn apply(ledger: &mut Ledger, patch: ConfigPatch) -> Result<(), Rejection> {
    let envelopes = ledger.propose(&ctx(), &Command::UpdateConfig { patch })?;
    ledger.commit(&envelopes);
    Ok(())
}

fn config(ledger: &Ledger) -> GuildConfig {
    ledger
        .state()
        .guild(GUILD)
        .map(|g| g.config.clone())
        .unwrap_or_default()
}

/// Audit S9: the legacy raid-deprecation fallback was 90 *milliseconds*, so
/// every raid was stale the moment it ended and attendance read as zero. The
/// rewrite's defaults are the intended ones, and this is the test that says so.
#[test]
fn the_defaults_are_the_fixed_ones_not_the_legacy_fallbacks() {
    let cfg = GuildConfig::default();
    assert_eq!(
        cfg.raid_deprecation_ms,
        90 * DAY_MS,
        "90 days, not the legacy 90 milliseconds"
    );
    assert_eq!(cfg.tick_duration_ms, 6 * 60_000);
    assert_eq!(cfg.bid_time_s, 60);
    assert_eq!(cfg.raidhelper_event_dkp, 5);
    assert_eq!(cfg.min_bid, 0);
}

/// `/configure` re-sends every required option on each invocation, so a patch
/// that names one setting must leave the rest exactly as they were — otherwise
/// changing the bid time would quietly reset the officer role.
#[test]
fn a_patch_changes_only_what_it_names() {
    let mut l = Ledger::new();
    apply(
        &mut l,
        ConfigPatch {
            admin_role: Some(111),
            raid_channel: Some(222),
            bid_time_s: Some(90),
            ..Default::default()
        },
    )
    .unwrap();
    apply(
        &mut l,
        ConfigPatch {
            bid_time_s: Some(120),
            ..Default::default()
        },
    )
    .unwrap();

    let cfg = config(&l);
    assert_eq!(cfg.bid_time_s, 120, "the named setting changed");
    assert_eq!(cfg.admin_role, Some(111), "the officer role survived");
    assert_eq!(cfg.raid_channel, Some(222), "the raid channel survived");
    assert_eq!(
        cfg.tick_duration_ms,
        6 * 60_000,
        "an untouched setting kept its default"
    );
}

/// The officer role is the gate on every `restricted` command, so handing it
/// to a different role has to take effect from the projection immediately —
/// there is no cache and no 1 h polling loop any more.
#[test]
fn the_officer_role_can_be_handed_over() {
    let mut l = Ledger::new();
    apply(
        &mut l,
        ConfigPatch {
            admin_role: Some(111),
            ..Default::default()
        },
    )
    .unwrap();
    apply(
        &mut l,
        ConfigPatch {
            admin_role: Some(999),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(config(&l).admin_role, Some(999));
}

#[test]
fn values_that_would_break_something_later_are_refused_now() {
    let cases: &[(&str, ConfigPatch)] = &[
        (
            "tickduration",
            ConfigPatch {
                tick_duration_ms: Some(0),
                ..Default::default()
            },
        ),
        (
            "raiddeprecationtime",
            ConfigPatch {
                raid_deprecation_ms: Some(0),
                ..Default::default()
            },
        ),
        (
            "bidtime",
            ConfigPatch {
                bid_time_s: Some(29),
                ..Default::default()
            },
        ),
        (
            "bidtime",
            ConfigPatch {
                bid_time_s: Some(1001),
                ..Default::default()
            },
        ),
        (
            "minbid",
            ConfigPatch {
                min_bid: Some(-1),
                ..Default::default()
            },
        ),
        (
            "overbidtowinmain",
            ConfigPatch {
                over_bid_to_win_main: Some(-5),
                ..Default::default()
            },
        ),
        (
            "raidhelpereventdkp",
            ConfigPatch {
                raidhelper_event_dkp: Some(-1),
                ..Default::default()
            },
        ),
        (
            "raidhelperapikey",
            ConfigPatch {
                raidhelper_api_key: Some(Secret::from("   ".to_owned())),
                ..Default::default()
            },
        ),
        (
            "raidhelperapikey",
            ConfigPatch {
                raidhelper_api_key: Some(Secret::from("abc \n".to_owned())),
                ..Default::default()
            },
        ),
    ];

    for (expected, patch) in cases {
        let mut l = Ledger::new();
        match apply(&mut l, patch.clone()) {
            Err(Rejection::InvalidConfig { setting, .. }) => {
                assert_eq!(&setting, expected, "wrong setting named")
            }
            other => panic!("{expected} was not refused: {other:?}"),
        }
        assert_eq!(
            config(&l),
            GuildConfig::default(),
            "{expected}: a refused patch must not be partially applied"
        );
    }
}

/// The two voice channels are both counted for attendance, so pointing them at
/// the same place awards a doubled tick to everyone in it. (The legacy
/// `second_raid_channel` patch; #11's list is checked the same way below.)
/// The invariant spans
/// two settings, which means it can only be checked against the merged result:
/// set one today and the other tomorrow and no single call sees both.
#[test]
fn the_two_raid_channels_cannot_collide_even_across_separate_calls() {
    let mut l = Ledger::new();
    apply(
        &mut l,
        ConfigPatch {
            raid_channel: Some(500),
            ..Default::default()
        },
    )
    .unwrap();
    let err = apply(
        &mut l,
        ConfigPatch {
            second_raid_channel: Some(500),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        matches!(err, Rejection::InvalidConfig { setting, .. } if setting == "secondraidchannel"),
        "got {err:?}"
    );
    assert!(config(&l).extra_raid_channels.is_empty());

    apply(
        &mut l,
        ConfigPatch {
            second_raid_channel: Some(501),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(config(&l).extra_raid_channels, vec![501]);
}

fn add_channel(l: &mut Ledger, channel: u64) -> Result<(), Rejection> {
    let envelopes = l.propose(&ctx(), &Command::AddRaidChannel { channel })?;
    l.commit(&envelopes);
    Ok(())
}

fn remove_channel(l: &mut Ledger, channel: u64) -> Result<(), Rejection> {
    let envelopes = l.propose(&ctx(), &Command::RemoveRaidChannel { channel })?;
    l.commit(&envelopes);
    Ok(())
}

fn with_raid_channel(channel: u64) -> Ledger {
    let mut l = Ledger::new();
    apply(
        &mut l,
        ConfigPatch {
            raid_channel: Some(channel),
            ..Default::default()
        },
    )
    .unwrap();
    l
}

/// #11: Plane of Time splits the raid into more groups than a raid channel
/// and one second channel could hold. Officers add as many as they need, in
/// order, and remove any but the raid channel itself.
#[test]
fn a_raid_counts_as_many_voice_channels_as_officers_add() {
    let mut l = with_raid_channel(500);
    for c in [501, 502, 503] {
        add_channel(&mut l, c).unwrap();
    }
    assert_eq!(config(&l).raid_channels(), vec![500, 501, 502, 503]);

    remove_channel(&mut l, 502).unwrap();
    assert_eq!(config(&l).raid_channels(), vec![500, 501, 503]);

    let err = remove_channel(&mut l, 500).unwrap_err();
    assert!(matches!(err, Rejection::InvalidConfig { .. }), "{err:?}");
    let err = remove_channel(&mut l, 999).unwrap_err();
    assert!(matches!(err, Rejection::InvalidConfig { .. }), "{err:?}");
    assert_eq!(config(&l).raid_channels(), vec![500, 501, 503]);
}

/// A channel counted twice pays its members twice per tick, whichever way the
/// duplicate arrives: added again, added when it is the raid channel, or the
/// raid channel moved onto an extra one.
#[test]
fn no_channel_is_ever_a_raid_channel_twice() {
    let mut l = with_raid_channel(500);
    add_channel(&mut l, 501).unwrap();
    assert!(add_channel(&mut l, 501).is_err(), "added twice");
    assert!(add_channel(&mut l, 500).is_err(), "the raid channel itself");
    assert!(
        apply(
            &mut l,
            ConfigPatch {
                raid_channel: Some(501),
                ..Default::default()
            },
        )
        .is_err(),
        "the raid channel moved onto an extra one"
    );
    assert!(
        apply(
            &mut l,
            ConfigPatch {
                extra_raid_channels: Some(vec![502, 502]),
                ..Default::default()
            },
        )
        .is_err(),
        "a list naming one twice"
    );
    assert_eq!(config(&l).raid_channels(), vec![500, 501]);
}

#[test]
fn a_raid_counts_at_most_ten_voice_channels() {
    let mut l = with_raid_channel(500);
    for c in 501..510 {
        add_channel(&mut l, c).unwrap();
    }
    assert_eq!(config(&l).raid_channels().len(), 10);
    assert!(add_channel(&mut l, 510).is_err());
}

/// Ledgers written before #11 set `second_raid_channel`. Replayed, that is a
/// one-entry list, so deploying the change leaves the live config as it was,
/// and a later legacy write still replaces the earlier one as it used to.
#[test]
fn a_legacy_second_channel_replays_as_the_only_extra_one() {
    let mut l = with_raid_channel(500);
    for second in [501, 502] {
        apply(
            &mut l,
            ConfigPatch {
                second_raid_channel: Some(second),
                ..Default::default()
            },
        )
        .unwrap();
    }
    assert_eq!(config(&l).raid_channels(), vec![500, 502]);

    let old: ConfigPatch = serde_json::from_str(r#"{"second_raid_channel":503}"#).unwrap();
    assert_eq!(old.second_raid_channel, Some(503));
    apply(&mut l, old).unwrap();
    assert_eq!(config(&l).raid_channels(), vec![500, 503]);
}

/// The config is a fold like everything else, so a restart has to land on the
/// same settings — including the order-dependent ones, where the last write
/// wins.
#[test]
fn replay_reproduces_the_configuration() {
    let mut live = Ledger::new();
    let mut log = Vec::new();
    for patch in [
        ConfigPatch {
            admin_role: Some(111),
            raid_channel: Some(222),
            ..Default::default()
        },
        ConfigPatch {
            admin_role: Some(333),
            bid_time_s: Some(45),
            ..Default::default()
        },
        ConfigPatch {
            raidhelper_api_key: Some(Secret::from("key-abc".to_owned())),
            ..Default::default()
        },
    ] {
        let envelopes = live
            .propose(&ctx(), &Command::UpdateConfig { patch })
            .unwrap();
        live.commit(&envelopes);
        log.extend(envelopes);
    }

    let mut replayed = Ledger::new();
    for env in &log {
        replayed.replay(env);
    }
    assert_eq!(live.state().guild(GUILD), replayed.state().guild(GUILD));
    assert_eq!(config(&replayed).admin_role, Some(333), "last write wins");
}

/// Nothing debug-logs a command today, but a `?patch` added later, a panic
/// message, or a span attribute would put the value in the log pipeline —
/// which ships off the box.
#[test]
fn the_api_key_does_not_survive_a_debug_format() {
    let secret = "rh_live_do_not_print_me";
    let patch = ConfigPatch {
        raidhelper_api_key: Some(Secret::from(secret.to_owned())),
        ..Default::default()
    };
    let rendered = format!("{patch:?}");
    assert!(
        !rendered.contains(secret),
        "the key reached a Debug string: {rendered}"
    );
    assert!(rendered.contains("redacted"), "{rendered}");

    let mut l = Ledger::new();
    apply(&mut l, patch).unwrap();
    let stored = config(&l).raidhelper_api_key.unwrap();
    assert_eq!(stored.as_str(), secret, "but the bot can still use it");
    assert!(!format!("{:?}", config(&l)).contains(secret));
}
