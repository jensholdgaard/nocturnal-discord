//! Character profiles — what Quarmy shows, from our own pipe.
//!
//! A member's Zeal sends `everquest.character.profile` events (identity,
//! base stats, AA, the 21 equipment slots by item id) through the gateway
//! into Ourios. This module asks Ourios for recent ones, keeps the newest
//! per character, and turns item ids into rows from the item mirror, so the
//! site can draw a gear page with nothing fetched at page time.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::items::{ItemMirror, ItemSummary};

/// One equipped slot as the site renders it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Slot {
    pub slot: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A character as its client last reported it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub name: String,
    pub level: i64,
    pub class: i64,
    pub race: i64,
    pub deity: i64,
    #[serde(default)]
    pub guild: String,
    #[serde(default)]
    pub base_stats: HashMap<String, i64>,
    /// The character sheet as the client drew it (effective stats, resists,
    /// hp/max_hp, mana/max_mana, ac, atk). Empty from builds before it was
    /// sent; the page falls back to base + item sums.
    pub sheet: HashMap<String, i64>,
    #[serde(default)]
    pub aa: HashMap<String, i64>,
    #[serde(default)]
    pub equipment: Vec<Slot>,
    /// Trained abilities as the client reports them: `[client_index, rank]`
    /// pairs, the same numbers `/outputfile quarmy` writes. Names are a
    /// mapping the site owns, once it is proven against the client's table.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aa_abilities: Vec<(u16, u8)>,
    /// When the client sent it (ms since epoch).
    #[serde(default)]
    pub reported_ms: i64,
    /// The Discord username the gateway stamped on the event — the member
    /// behind the bearer token, a fact rather than a claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reporter: Option<String>,
}

/// The body of a profile event, as Zeal builds it. Everything but the
/// timestamp, which is the record's.
#[derive(Deserialize)]
struct Body {
    name: String,
    level: i64,
    class: i64,
    race: i64,
    #[serde(default)]
    deity: i64,
    #[serde(default)]
    guild: String,
    #[serde(default)]
    base_stats: HashMap<String, i64>,
    #[serde(default)]
    sheet: HashMap<String, i64>,
    #[serde(default)]
    aa: HashMap<String, i64>,
    #[serde(default)]
    equipment: Vec<Slot>,
    #[serde(default)]
    aa_abilities: Vec<(u16, u8)>,
}

/// An Ourios record, only the parts we read. The body arrives either as the
/// original text (`line`) or, for rows the miner kept verbatim, as a string.
#[derive(Deserialize)]
struct Record {
    #[serde(default)]
    time_unix_nano: serde_json::Value,
    #[serde(default)]
    body: serde_json::Value,
    #[serde(default)]
    attributes: serde_json::Value,
}

/// One string attribute out of Ourios' `[{key, value: {stringValue}}]` list.
fn string_attr(attrs: &serde_json::Value, key: &str) -> Option<String> {
    attrs.as_array()?.iter().find_map(|a| {
        (a["key"].as_str() == Some(key)).then(|| {
            a["value"]["stringValue"]
                .as_str()
                .or_else(|| a["value"].as_str())
                .map(String::from)
        })?
    })
}

fn body_text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(o) => o
            .get("line")
            .or_else(|| o.get("body"))
            .or_else(|| o.get("text"))
            .and_then(|x| x.as_str())
            .map(String::from),
        _ => None,
    }
}

fn nanos(v: &serde_json::Value) -> i64 {
    match v {
        serde_json::Value::Number(n) => n.as_i64().unwrap_or(0),
        serde_json::Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

/// Newest profile per character from a page of Ourios records. Pure.
pub fn latest_per_character(records: &[serde_json::Value]) -> HashMap<String, Profile> {
    let mut out: HashMap<String, Profile> = HashMap::new();
    for r in records {
        let Ok(rec) = serde_json::from_value::<Record>(r.clone()) else {
            continue;
        };
        let Some(text) = body_text(&rec.body) else {
            continue;
        };
        let Ok(b) = serde_json::from_str::<Body>(&text) else {
            continue;
        };
        let reported_ms = nanos(&rec.time_unix_nano) / 1_000_000;
        let key = b.name.to_lowercase();
        let newer = out.get(&key).map_or(true, |p| reported_ms > p.reported_ms);
        if newer {
            out.insert(
                key,
                Profile {
                    name: b.name,
                    level: b.level,
                    class: b.class,
                    race: b.race,
                    deity: b.deity,
                    guild: b.guild,
                    base_stats: b.base_stats,
                    sheet: b.sheet,
                    aa: b.aa,
                    equipment: b.equipment,
                    aa_abilities: b.aa_abilities,
                    reported_ms,
                    reporter: string_attr(&rec.attributes, "everquest.reporter"),
                },
            );
        }
    }
    out
}

/// EverQuest class ids as the client reports them, to the roster's names.
pub fn class_name(id: i64) -> Option<&'static str> {
    Some(match id {
        1 => "Warrior",
        2 => "Cleric",
        3 => "Paladin",
        4 => "Ranger",
        5 => "Shadow Knight",
        6 => "Druid",
        7 => "Monk",
        8 => "Bard",
        9 => "Rogue",
        10 => "Shaman",
        11 => "Necromancer",
        12 => "Wizard",
        13 => "Magician",
        14 => "Enchanter",
        15 => "Beastlord",
        _ => return None,
    })
}

/// What the roster should record for a profile, given what it holds now.
/// `None` when nothing would change — the ledger must not fill with
/// identical events every half hour. Manual fields (main, access, link)
/// are carried over untouched: the game does not know them.
pub fn roster_update(
    profile: &Profile,
    existing: Option<&nocturnal_core::RosterCharacter>,
) -> Option<(nocturnal_core::RosterCharacter, bool)> {
    let class = class_name(profile.class)?.to_owned();
    let level = u8::try_from(profile.level)
        .ok()
        .filter(|l| (1..=65).contains(l))?;
    let aa = profile
        .aa
        .get("spent")
        .copied()
        .and_then(|a| u16::try_from(a).ok())
        .filter(|a| *a >= 1);
    let next = nocturnal_core::RosterCharacter {
        name: profile.name.clone(),
        class,
        level,
        aa: aa.or(existing.and_then(|e| e.aa)),
        profile_url: existing.and_then(|e| e.profile_url.clone()),
        access: existing.map(|e| e.access.clone()).unwrap_or_default(),
        main: existing.and_then(|e| e.main),
    };
    match existing {
        Some(e) if *e == next => None,
        Some(_) => Some((next, true)),
        None => Some((next, false)),
    }
}

/// Ask Ourios for recent profile events. Failure is an empty map and a
/// debug line: the site keeps whatever it rendered last time.
pub async fn fetch_profiles(query_url: &str, tenant: &str) -> HashMap<String, Profile> {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .build()
    {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    // Seven days, not thirty: every extra day is more S3 row groups the
    // querier reads cold, and a profile older than a week is refreshed the
    // moment its owner zones anyway. The 45s timeout rides out a flush.
    let query = r#"event_name == "everquest.character.profile" | range(-7d, now) | limit 2000"#;
    let resp = client
        .post(query_url)
        .header("content-type", "application/json")
        .header("x-ourios-tenant", tenant)
        .json(&serde_json::json!({ "query": query }))
        .send()
        .await;
    let body: serde_json::Value = match resp {
        Ok(r) if r.status().is_success() => r.json().await.unwrap_or_default(),
        Ok(r) => {
            tracing::warn!(status = %r.status(), "ourios refused the profile query; keeping the previous profiles");
            return HashMap::new();
        }
        Err(e) => {
            tracing::warn!(error = %e, "ourios unreachable for profiles; keeping the previous ones");
            return HashMap::new();
        }
    };
    let records = body["records"].as_array().cloned().unwrap_or_default();
    latest_per_character(&records)
}

/// One member's telemetry footprint, for `/dpsstatus`.
pub struct ReporterStatus {
    pub reporter: String,
    pub version: String,
    pub last_seen_ms: i64,
    pub count: usize,
}

/// Who has sent a character profile lately, their Zeal build
/// (`service.version`) and when last seen — the officer view of who is
/// reporting and who needs to update. Newest first; empty on any Ourios
/// failure (logged, never surfaced as a crash). A wider window than the site
/// fetch: this is a roll-call, so `-14d` catches members who raid weekly.
pub async fn reporter_status(query_url: &str, tenant: &str) -> Vec<ReporterStatus> {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let query = r#"event_name == "everquest.character.profile" | range(-14d, now) | limit 5000"#;
    let resp = client
        .post(query_url)
        .header("content-type", "application/json")
        .header("x-ourios-tenant", tenant)
        .json(&serde_json::json!({ "query": query }))
        .send()
        .await;
    let body: serde_json::Value = match resp {
        Ok(r) if r.status().is_success() => r.json().await.unwrap_or_default(),
        Ok(r) => {
            tracing::warn!(status = %r.status(), "ourios refused the dpsstatus query");
            return Vec::new();
        }
        Err(e) => {
            tracing::warn!(error = %e, "ourios unreachable for dpsstatus");
            return Vec::new();
        }
    };
    let records = body["records"].as_array().cloned().unwrap_or_default();
    aggregate_reporters(&records)
}

/// The reduce half of [`reporter_status`], split out so a fixture can pin the
/// attribute-shape handling without a live Ourios.
pub fn aggregate_reporters(records: &[serde_json::Value]) -> Vec<ReporterStatus> {
    fn attr(r: &serde_json::Value, key: &str) -> Option<String> {
        for group in ["attributes", "resource_attributes"] {
            if let Some(arr) = r[group].as_array() {
                for a in arr {
                    if a["key"].as_str() == Some(key) {
                        let v = &a["value"];
                        return v["stringValue"]
                            .as_str()
                            .or_else(|| v["intValue"].as_str())
                            .map(str::to_owned)
                            .or_else(|| v.as_str().map(str::to_owned));
                    }
                }
            }
        }
        None
    }
    fn seen_ms(r: &serde_json::Value) -> i64 {
        let t = &r["time_unix_nano"];
        let nanos = t
            .as_i64()
            .or_else(|| t.as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(0);
        nanos / 1_000_000
    }
    let mut by: std::collections::BTreeMap<String, ReporterStatus> =
        std::collections::BTreeMap::new();
    for r in records {
        let reporter = attr(r, "everquest.reporter")
            .or_else(|| attr(r, "everquest.character.name"))
            .unwrap_or_else(|| "unknown".to_owned());
        let version = attr(r, "service.version").unwrap_or_else(|| "?".to_owned());
        let ts = seen_ms(r);
        let e = by.entry(reporter.clone()).or_insert(ReporterStatus {
            reporter,
            version: version.clone(),
            last_seen_ms: 0,
            count: 0,
        });
        e.count += 1;
        // The build shown is the one from the most recent profile.
        if ts >= e.last_seen_ms {
            e.last_seen_ms = ts;
            e.version = version;
        }
    }
    let mut out: Vec<ReporterStatus> = by.into_values().collect();
    out.sort_by_key(|r| std::cmp::Reverse(r.last_seen_ms));
    out
}

/// Write what the clients reported into the roster: a character the member
/// never added appears, a level that moved is updated, and nothing else is
/// touched. `players` maps reporter username → player id; profiles whose
/// reporter is unknown are left alone rather than guessed.
pub async fn sync_roster(
    driver: &crate::driver::DriverHandle,
    ledger_guild: u64,
    profiles: &HashMap<String, Profile>,
    players: &HashMap<String, u64>,
) -> usize {
    use nocturnal_core::{Actor, Command};
    let mut written = 0;
    for p in profiles.values() {
        let Some(player) = p
            .reporter
            .as_ref()
            .and_then(|r| players.get(&r.to_lowercase()))
            .copied()
        else {
            // Not silent: a profile that maps to nobody is the thing an
            // operator needs to see, and at boot the member cache is cold.
            tracing::info!(
                character = %p.name,
                reporter = p.reporter.as_deref().unwrap_or("<none>"),
                "profile has no matching member yet; the next render retries"
            );
            continue;
        };
        let key = p.name.to_lowercase();
        let existing = driver
            .query(move |l| {
                l.state()
                    .guild(ledger_guild)
                    .and_then(|g| g.roster.get(&player))
                    .and_then(|cs| cs.get(&key))
                    .cloned()
            })
            .await;
        let Some((character, replace)) = roster_update(p, existing.as_ref()) else {
            continue;
        };
        match driver
            .execute(
                ledger_guild,
                Actor::System,
                Command::SetRosterCharacter {
                    player,
                    character,
                    replace,
                },
            )
            .await
        {
            Ok(_) => written += 1,
            Err(e) => {
                tracing::debug!(error = %e, character = %p.name, "profile not applied to the roster")
            }
        }
    }
    written
}

/// Profiles keyed by lowercase name, and every worn item's summary keyed by
/// id — typed, for the page server. Misses are fetched once and cached.
pub async fn resolve(
    profiles: &HashMap<String, Profile>,
    mirror: &ItemMirror,
) -> (
    std::collections::BTreeMap<String, Profile>,
    std::collections::BTreeMap<String, ItemSummary>,
) {
    let mut gear = std::collections::BTreeMap::new();
    for p in profiles.values() {
        for s in &p.equipment {
            let Some(id) = s.id else { continue };
            let key = id.to_string();
            if gear.contains_key(&key) {
                continue;
            }
            if let Some(row) = mirror.get(id).await {
                gear.insert(key, ItemSummary::from_row(&row));
            }
        }
    }
    let profiles: std::collections::BTreeMap<String, Profile> = profiles
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    (profiles, gear)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn rec(name: &str, level: i64, t: i64) -> serde_json::Value {
        let body = serde_json::json!({
            "name": name, "level": level, "class": 10, "race": 2, "deity": 201, "guild": "Nocturnal",
            "base_stats": {"str": 75, "sta": 80},
            "aa": {"spent": 40, "unspent": 2},
            "equipment": [{"slot": "Head", "id": 30563, "name": "Wistful Tunic of the Void"}, {"slot": "Face"}],
        });
        serde_json::json!({
            "time_unix_nano": t * 1_000_000,
            "event_name": "everquest.character.profile",
            "body": {"kind": "rendered", "line": body.to_string(), "reconstruction": "faithful"},
        })
    }

    #[test]
    fn the_newest_report_per_character_wins() {
        let m = latest_per_character(&[
            rec("Shaku", 59, 1000),
            rec("Shaku", 60, 2000),
            rec("Eklavdra", 25, 1500),
            rec("shaku", 58, 500),
        ]);
        assert_eq!(m.len(), 2);
        let s = &m["shaku"];
        assert_eq!((s.level, s.reported_ms, s.equipment.len()), (60, 2000, 2));
        assert_eq!(s.equipment[0].id, Some(30563));
        assert_eq!(s.equipment[1].id, None, "an empty slot is still a slot");
        assert_eq!(s.aa["spent"], 40);
    }

    #[test]
    fn the_reporter_is_read_from_the_attribute_list() {
        let mut r = rec("Shaku", 60, 10);
        r["attributes"] = serde_json::json!([
            {"key": "everquest.reporter", "value": {"stringValue": "bisben_"}},
            {"key": "everquest.character.level", "value": {"stringValue": "60"}}
        ]);
        let m = latest_per_character(&[r]);
        assert_eq!(m["shaku"].reporter.as_deref(), Some("bisben_"));
    }

    #[test]
    fn the_roster_changes_only_when_the_game_says_something_new() {
        let m = latest_per_character(&[rec("Shaku", 60, 10)]);
        let p = &m["shaku"];
        // New character: added, as reported.
        let (c, replace) = roster_update(p, None).unwrap();
        assert_eq!(
            (c.class.as_str(), c.level, c.aa, replace),
            ("Shaman", 60, Some(40), false)
        );
        // Same again: nothing to write.
        assert!(roster_update(p, Some(&c)).is_none());
        // Level moved: an edit, and the manual fields survive.
        let mut had = c.clone();
        had.level = 59;
        had.main = Some(nocturnal_core::MainRank::Main);
        had.access = vec!["VP".into()];
        had.profile_url = Some("https://quarmy.com/c/shaku".into());
        let (c2, replace) = roster_update(p, Some(&had)).unwrap();
        assert!(replace);
        assert_eq!(
            (c2.level, c2.main, c2.access.len(), c2.profile_url.is_some()),
            (60, Some(nocturnal_core::MainRank::Main), 1, true)
        );
        // An unknown class id is not a roster entry.
        let mut weird = p.clone();
        weird.class = 99;
        assert!(roster_update(&weird, None).is_none());
    }

    #[test]
    fn a_verbatim_string_body_is_read_too() {
        let mut r = rec("Shaku", 60, 10);
        let text = r["body"]["line"].as_str().unwrap().to_owned();
        r["body"] = serde_json::Value::String(text);
        assert_eq!(latest_per_character(&[r]).len(), 1);
    }
}

#[cfg(test)]
mod reporter_status_tests {
    use super::aggregate_reporters;
    use serde_json::json;

    #[test]
    fn newest_profile_wins_and_rows_sort_by_recency() {
        let rec = |reporter: &str, ver: &str, nanos: i64| {
            json!({
                "time_unix_nano": nanos,
                "attributes": [{"key": "everquest.reporter", "value": {"stringValue": reporter}}],
                "resource_attributes": [{"key": "service.version", "value": {"stringValue": ver}}],
            })
        };
        let rows = aggregate_reporters(&[
            rec("zig", "1.4.5+aaa", 1_000_000_000),
            rec("bisben", "1.4.5+bbb", 3_000_000_000),
            rec("zig", "1.4.5+ccc", 2_000_000_000), // newer than zig's first
        ]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].reporter, "bisben", "most recently seen first");
        assert_eq!(rows[1].reporter, "zig");
        assert_eq!(
            rows[1].version, "1.4.5+ccc",
            "the build from zig's newest profile"
        );
        assert_eq!(rows[1].count, 2);
        assert_eq!(rows[1].last_seen_ms, 2_000);
    }

    #[test]
    fn nanos_as_string_and_missing_reporter_are_handled() {
        let rows = aggregate_reporters(&[json!({
            "time_unix_nano": "1500000000",
            "attributes": [{"key": "everquest.character.name", "value": {"stringValue": "Solo"}}],
            "resource_attributes": [],
        })]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reporter, "Solo", "falls back to character name");
        assert_eq!(rows[0].version, "?");
        assert_eq!(rows[0].last_seen_ms, 1_500);
    }
}
