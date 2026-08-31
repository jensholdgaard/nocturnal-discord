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
    #[serde(default)]
    pub aa: HashMap<String, i64>,
    #[serde(default)]
    pub equipment: Vec<Slot>,
    /// When the client sent it (ms since epoch).
    #[serde(default)]
    pub reported_ms: i64,
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
    aa: HashMap<String, i64>,
    #[serde(default)]
    equipment: Vec<Slot>,
}

/// An Ourios record, only the parts we read. The body arrives either as the
/// original text (`line`) or, for rows the miner kept verbatim, as a string.
#[derive(Deserialize)]
struct Record {
    #[serde(default)]
    time_unix_nano: serde_json::Value,
    #[serde(default)]
    body: serde_json::Value,
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
                    aa: b.aa,
                    equipment: b.equipment,
                    reported_ms,
                },
            );
        }
    }
    out
}

/// Ask Ourios for recent profile events. Failure is an empty map and a
/// debug line: the site keeps whatever it rendered last time.
pub async fn fetch_profiles(query_url: &str, tenant: &str) -> HashMap<String, Profile> {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    let query = r#"event_name == "everquest.character.profile" | range(-30d, now) | limit 2000"#;
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
            tracing::debug!(status = %r.status(), "ourios profile query refused");
            return HashMap::new();
        }
        Err(e) => {
            tracing::debug!(error = %e, "ourios unreachable; profiles unchanged");
            return HashMap::new();
        }
    };
    let records = body["records"].as_array().cloned().unwrap_or_default();
    latest_per_character(&records)
}

/// Profiles with their items resolved, as the site payload wants them:
/// `{ "<name lower>": profile }` plus `gear_items: { "<id>": ItemSummary }`
/// for every item any profile wears. Misses are fetched once and cached.
pub async fn render(profiles: &HashMap<String, Profile>, mirror: &ItemMirror) -> serde_json::Value {
    let mut gear: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for p in profiles.values() {
        for s in &p.equipment {
            let Some(id) = s.id else { continue };
            let key = id.to_string();
            if gear.contains_key(&key) {
                continue;
            }
            if let Some(row) = mirror.get(id).await {
                if let Ok(v) = serde_json::to_value(ItemSummary::from_row(&row)) {
                    gear.insert(key, v);
                }
            }
        }
    }
    serde_json::json!({
        "profiles": profiles,
        "gear_items": gear,
    })
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
    fn a_verbatim_string_body_is_read_too() {
        let mut r = rec("Shaku", 60, 10);
        let text = r["body"]["line"].as_str().unwrap().to_owned();
        r["body"] = serde_json::Value::String(text);
        assert_eq!(latest_per_character(&[r]).len(), 1);
    }
}
