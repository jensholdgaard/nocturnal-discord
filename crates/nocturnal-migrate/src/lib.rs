//! Legacy `nocturnal-dkp-bot` data → genesis events (M2).
//!
//! Input: the `{guild}_players.json` / `{guild}_raids.json` collection dumps
//! the legacy `/backup` command produces (raw `find().toArray()` output).
//! Output: `player.imported` / `raid.imported` genesis envelopes appended to a
//! fresh WAL, plus a verification report proving every replayed balance
//! matches the snapshot to the point (hazard B10).

use serde::Deserialize;

use nocturnal_core::event::{ImportedAttendance, ImportedLogEntry, Item, RaidRef};
use nocturnal_core::{Actor, Command, Ctx, Envelope, Ledger};

// ---- legacy document shapes (as serialized by the Node mongodb driver) -----

#[derive(Deserialize)]
pub struct LegacyPlayer {
    pub player: String,
    pub guild: String,
    #[serde(default)]
    pub current: i64,
    #[serde(default, rename = "creationDate")]
    pub creation_date: i64,
    #[serde(default)]
    pub characters: Vec<String>,
    #[serde(default)]
    pub log: Vec<LegacyLogEntry>,
}

#[derive(Deserialize)]
pub struct LegacyLogEntry {
    #[serde(default)]
    pub dkp: i64,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub date: i64,
    #[serde(default)]
    pub raid: Option<LegacyRaidRef>,
    #[serde(default)]
    pub item: Option<LegacyItem>,
}

#[derive(Deserialize)]
pub struct LegacyRaidRef {
    #[serde(rename = "_id")]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Deserialize)]
pub struct LegacyItem {
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
}

#[derive(Deserialize)]
pub struct LegacyRaid {
    #[serde(rename = "_id", default)]
    pub id: Option<String>,
    pub guild: String,
    pub name: String,
    #[serde(default)]
    pub date: i64,
    #[serde(default)]
    pub attendance: Vec<LegacyAttendance>,
}

#[derive(Deserialize)]
pub struct LegacyAttendance {
    #[serde(default)]
    pub players: Vec<String>,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub date: i64,
    #[serde(default)]
    pub dkps: i64,
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
                    raid: e.raid.as_ref().and_then(|r| {
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
