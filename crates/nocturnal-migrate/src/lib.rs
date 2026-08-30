//! Legacy `nocturnal-dkp-bot` data → genesis events (M2).
//!
//! Input: the players/raids collection dumps (raw `find().toArray()` output).
//! The legacy server wrote these to its own disk as `{guild}_players.json` and
//! `{guild}_raids.json`; the zip `/backup` hands out calls them `players.json`
//! and `raids.json`. Either name parses — this reads whatever it is given.
//! Output: `player.imported` / `raid.imported` genesis envelopes appended to a
//! fresh WAL, plus a verification report proving every replayed balance
//! matches the snapshot to the point (hazard B10).

use serde::{Deserialize, Serialize};

use nocturnal_core::event::{ImportedAttendance, ImportedLogEntry, Item, RaidRef};
use nocturnal_core::{Actor, Command, Ctx, Envelope, Ledger};

pub mod export;

// ---- legacy document shapes (as serialized by the Node mongodb driver) -----

#[derive(Deserialize, Serialize)]
pub struct LegacyPlayer {
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub player: String,
    pub guild: String,
    #[serde(default)]
    pub current: i64,
    #[serde(default, rename = "creationDate")]
    pub creation_date: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub characters: Vec<String>,
    #[serde(default)]
    pub log: Vec<LegacyLogEntry>,
}

#[derive(Deserialize, Serialize)]
pub struct LegacyLogEntry {
    #[serde(default)]
    pub dkp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default)]
    pub date: i64,
    // `Option<Option<_>>`: the outer level is "was the key there at all",
    // the inner is `null`. Legacy writes the key on every entry, sometimes
    // null, and the importer has always accepted both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raid: Option<Option<LegacyRaidRef>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<LegacyItem>,
}

#[derive(Deserialize, Serialize)]
pub struct LegacyRaidRef {
    #[serde(rename = "_id", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct LegacyItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct LegacyRaid {
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub guild: String,
    pub name: String,
    #[serde(default)]
    pub date: i64,
    #[serde(default)]
    pub attendance: Vec<LegacyAttendance>,
    #[serde(default, rename = "tickDuration", deserialize_with = "lenient_i64")]
    pub tick_duration: i64,
    #[serde(default, rename = "dkpsPerTick", deserialize_with = "lenient_i64")]
    pub dkps_per_tick: i64,
    #[serde(default, rename = "eventId", skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub deprecated: bool,
}

#[derive(Deserialize, Serialize)]
pub struct LegacyAttendance {
    #[serde(default)]
    pub players: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default)]
    pub date: i64,
    #[serde(default)]
    pub dkps: i64,
}

/// Legacy derived `tickDuration` from a float number of minutes, so the real
/// snapshot contains values like `299879.99999999994` — three of the guild's
/// 506 raids. These are milliseconds; the ledger holds them as integers and
/// rounds, which moves a tick boundary by well under a microsecond.
///
/// Deliberately *not* used for anything that carries DKP: a fractional balance
/// is a corruption worth failing on, not rounding away.
fn lenient_i64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.round() as i64))
            .ok_or_else(|| serde::de::Error::custom(format!("not a number: {n}"))),
        serde_json::Value::Null => Ok(0),
        other => Err(serde::de::Error::custom(format!("not a number: {other}"))),
    }
}

// ---- conversion -------------------------------------------------------------

fn parse_id(s: &str) -> Option<u64> {
    s.parse().ok()
}

fn convert_item(i: &LegacyItem) -> Item {
    let id = match &i.id {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    Item {
        id,
        name: i.name.clone().unwrap_or_default(),
        url: i.url.clone(),
        data: i.data.clone(),
        image: i.image.clone(),
    }
}

/// Build the genesis command stream. Deterministic: same input, same output.
/// `raid_deprecation_days` seeds the guild config (legacy default: 90) —
/// useful when migrating a stale snapshot whose players would otherwise all
/// fall outside the activity window.
pub fn genesis_commands(
    players: &[LegacyPlayer],
    raids: &[LegacyRaid],
    raid_deprecation_days: Option<i64>,
) -> (u64, Vec<Command>, Vec<String>) {
    let mut warnings = Vec::new();
    let guild = players
        .first()
        .map(|p| &p.guild)
        .or_else(|| raids.first().map(|r| &r.guild))
        .and_then(|g| parse_id(g))
        .unwrap_or(0);

    let mut commands = Vec::new();
    if let Some(days) = raid_deprecation_days {
        commands.push(Command::UpdateConfig {
            patch: nocturnal_core::event::ConfigPatch {
                raid_deprecation_ms: Some(days * nocturnal_core::state::DAY_MS),
                ..Default::default()
            },
        });
    }
    for (i, r) in raids.iter().enumerate() {
        let raid_id = r.id.clone().unwrap_or_else(|| format!("legacy-{i:04}"));
        commands.push(Command::ImportRaid {
            raid_id,
            name: r.name.clone(),
            date_ms: r.date,
            tick_interval_ms: r.tick_duration,
            dkp_per_tick: r.dkps_per_tick,
            event_id: r.event_id.clone(),
            entries: r
                .attendance
                .iter()
                .map(|a| ImportedAttendance {
                    players: a.players.iter().filter_map(|p| parse_id(p)).collect(),
                    comment: a.comment.clone().unwrap_or_default(),
                    ts_ms: a.date,
                    amount: a.dkps,
                })
                .collect(),
        });
    }

    for p in players {
        let Some(player) = parse_id(&p.player) else {
            warnings.push(format!("player id not a snowflake, skipped: {}", p.player));
            continue;
        };
        let balance_from_log: i64 = p.log.iter().map(|e| e.dkp).sum();
        if balance_from_log != p.current {
            warnings.push(format!(
                "player {player}: stored balance {} != sum of log {balance_from_log} (importing stored)",
                p.current
            ));
        }
        if p.current < 0 {
            warnings.push(format!(
                "player {player}: negative balance {} carried over from legacy (audit #46 damage)",
                p.current
            ));
        }
        commands.push(Command::ImportPlayer {
            player,
            legacy_id: p.id.clone(),
            balance: p.current,
            characters: p.characters.clone(),
            creation_ts_ms: p.creation_date,
            log: p
                .log
                .iter()
                .map(|e| ImportedLogEntry {
                    dkp: e.dkp,
                    comment: e.comment.clone().unwrap_or_default(),
                    ts_ms: e.date,
                    raid: e.raid.as_ref().and_then(|r| r.as_ref()).and_then(|r| {
                        r.id.as_ref().map(|id| RaidRef {
                            raid_id: id.clone(),
                            name: r.name.clone().unwrap_or_default(),
                        })
                    }),
                    item: e.item.as_ref().map(convert_item),
                })
                .collect(),
        });
    }

    (guild, commands, warnings)
}

/// One line of the verification report.
pub struct Verified {
    pub player: u64,
    pub snapshot: i64,
    pub replayed: i64,
}

/// Execute genesis into a ledger and verify replayed balances against the
/// snapshot. Returns (envelopes, per-player lines, mismatch count).
pub fn run_genesis(
    ledger: &mut Ledger,
    guild: u64,
    commands: &[Command],
    players: &[LegacyPlayer],
    now_ms: i64,
) -> (Vec<Envelope>, Vec<Verified>, usize) {
    let ctx = Ctx {
        guild,
        actor: Actor::System,
        now_ms,
    };
    let mut envelopes = Vec::new();
    for cmd in commands {
        let evs = ledger
            .execute(&ctx, cmd)
            .expect("genesis commands are always accepted");
        envelopes.extend(evs);
    }
    let g = ledger
        .state()
        .guild(guild)
        .expect("guild exists after genesis");
    let mut lines = Vec::new();
    let mut mismatches = 0;
    for p in players {
        let Some(player) = p.player.parse::<u64>().ok() else {
            continue;
        };
        let replayed = g.balance(player);
        if replayed != p.current {
            mismatches += 1;
        }
        lines.push(Verified {
            player,
            snapshot: p.current,
            replayed,
        });
    }
    (envelopes, lines, mismatches)
}
