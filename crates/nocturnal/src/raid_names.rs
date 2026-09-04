//! Naming a raid from what it fought.
//!
//! `/startraid` without a name leaves a placeholder. At `/endraid` the bot
//! asks Prometheus which targets took player damage during the raid and
//! keeps the ones in the boss table — a curated NPC → shorthand map — so a
//! night reads "Vulak, Cursed & Ring War", and Kromrif Veterans, wardens and
//! every other trash mob can never make the name, however much damage they
//! took (on 2026-09-03 the Ring War trash out-damaged Vulak 2.5×, because
//! few reporters were on Vulak).

use std::collections::BTreeMap;
use std::path::Path;

/// The boss table as officers keep it: `NPC name: shorthand`. Lookups
/// ignore case, whitespace and the `#` the client prefixes on instanced
/// spawns. Unreadable file → empty table → no name.
pub fn load_bosses(path: &Path) -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let Ok(raw) = serde_yaml_ng::from_str::<BTreeMap<String, String>>(&text) else {
        tracing::warn!(path = %path.display(), "raid boss table is not a name: shorthand map");
        return BTreeMap::new();
    };
    raw.into_iter()
        .map(|(k, v)| (normalize(&k), v.trim().to_owned()))
        .collect()
}

fn normalize(name: &str) -> String {
    name.trim().trim_start_matches('#').trim().to_lowercase()
}

/// One target and how much player damage it took in the window.
pub struct TargetDamage {
    pub target: String,
    pub damage: f64,
}

/// The name: bosses in the table, ordered by damage taken, joined the way
/// the guild writes them. A boss that took under 2% of all boss damage is
/// a tag, not a kill, and stays out. Shorthands dedupe (five VT bosses →
/// "VT"). `None` when nothing in the window is in the table.
pub fn pick(rows: &[TargetDamage], bosses: &BTreeMap<String, String>) -> Option<String> {
    let mut hits: Vec<(&str, f64)> = rows
        .iter()
        .filter_map(|r| {
            bosses
                .get(&normalize(&r.target))
                .map(|s| (s.as_str(), r.damage))
        })
        .collect();
    if hits.is_empty() {
        return None;
    }
    hits.sort_by(|a, b| b.1.total_cmp(&a.1));
    let total: f64 = hits.iter().map(|h| h.1).sum();
    let mut names: Vec<&str> = Vec::new();
    for (short, dmg) in hits {
        if dmg < total * 0.02 || names.contains(&short) {
            continue;
        }
        names.push(short);
    }
    Some(match names.len() {
        0 => return None,
        1 => names[0].to_owned(),
        2 => format!("{} & {}", names[0], names[1]),
        n => format!("{} & {}", names[..n - 1].join(", "), names[n - 1]),
    })
}

/// Player damage by target over `[start_ms, end_ms]`, from Prometheus. Any
/// failure is an empty list: naming is best-effort and never blocks
/// `/endraid`.
pub async fn targets_in_window(query_url: &str, start_ms: i64, end_ms: i64) -> Vec<TargetDamage> {
    let window_s = ((end_ms - start_ms) / 1000).max(60);
    let query = format!(
        "topk(40, sum by (everquest_combat_target) (increase(everquest_combat_damage_total{{everquest_combat_direction=\"outgoing\",everquest_combat_source_type=\"player\"}}[{window_s}s])))"
    );
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let resp = client
        .get(query_url)
        .query(&[
            ("query", query.as_str()),
            ("time", &(end_ms / 1000).to_string()),
        ])
        .send()
        .await;
    let body: serde_json::Value = match resp {
        Ok(r) if r.status().is_success() => r.json().await.unwrap_or_default(),
        Ok(r) => {
            tracing::warn!(status = %r.status(), "prometheus refused the raid-name query");
            return Vec::new();
        }
        Err(e) => {
            tracing::warn!(error = %e, "prometheus unreachable for the raid name");
            return Vec::new();
        }
    };
    parse_targets(&body)
}

/// The reduce half of [`targets_in_window`], pinned by a fixture.
pub fn parse_targets(body: &serde_json::Value) -> Vec<TargetDamage> {
    body["data"]["result"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let target = r["metric"]["everquest_combat_target"].as_str()?.to_owned();
                    let damage = r["value"][1].as_str()?.parse::<f64>().ok()?;
                    Some(TargetDamage { target, damage })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn table() -> BTreeMap<String, String> {
        [
            ("Vulak`Aerr", "Vulak"),
            ("Vyzh`dra the Cursed", "Cursed"),
            ("Narandi the Wretched", "Ring War"),
            ("Dain Frostreaver IV", "Ring War"),
            ("Kaas Thox Xi Ans Dyek", "VT"),
            ("Thall Va Kelun", "VT"),
        ]
        .into_iter()
        .map(|(k, v)| (normalize(k), v.to_owned()))
        .collect()
    }

    fn row(t: &str, d: f64) -> TargetDamage {
        TargetDamage {
            target: t.into(),
            damage: d,
        }
    }

    #[test]
    fn the_night_of_2026_09_03_names_itself_and_trash_cannot() {
        // Real numbers: the Ring War trash out-damaged Vulak 2.5×.
        let rows = vec![
            row("#Vyzh`dra the Cursed", 60118.0),
            row("Kromrif Veteran", 24569.0),
            row("#Vyzh`dra the Exiled", 24400.0),
            row("Kromrif Recruit", 23517.0),
            row("#a glyph covered serpent", 20562.0),
            row("a cerulean warden", 16543.0),
            row("Narandi the Wretched", 13112.0),
            row("#Vulak`Aerr", 9578.0),
        ];
        assert_eq!(pick(&rows, &table()).unwrap(), "Cursed, Ring War & Vulak");
    }

    #[test]
    fn shorthands_dedupe_and_tags_drop_out() {
        let rows = vec![
            row("Kaas Thox Xi Ans Dyek", 50000.0),
            row("Thall Va Kelun", 40000.0),
            row("#Vulak`Aerr", 100.0), // someone tagged Vulak once: 0.1%
        ];
        assert_eq!(pick(&rows, &table()).unwrap(), "VT");
        assert_eq!(pick(&[row("a cerulean warden", 9e9)], &table()), None);
        assert_eq!(
            pick(
                &[
                    row("Dain Frostreaver IV", 10.0),
                    row("Narandi the Wretched", 10.0)
                ],
                &table()
            )
            .unwrap(),
            "Ring War"
        );
    }

    #[test]
    fn prometheus_rows_parse_and_the_table_loads_loosely() {
        let body = json!({"data": {"result": [
            {"metric": {"everquest_combat_target": "#Vulak`Aerr"}, "value": [1.0, "9578"]},
            {"metric": {"everquest_combat_target": "x"}, "value": [1.0, "NaN"]},
        ]}});
        let rows = parse_targets(&body);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].target, "#Vulak`Aerr");
        let dir = std::env::temp_dir().join(format!("bosses-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("b.yaml");
        std::fs::write(&p, "Vulak`Aerr: Vulak\n\"Lord Inquisitor Seru\": Seru\n").unwrap();
        let t = load_bosses(&p);
        assert_eq!(t.get("vulak`aerr").map(String::as_str), Some("Vulak"));
        assert_eq!(
            t.get("lord inquisitor seru").map(String::as_str),
            Some("Seru")
        );
        assert!(load_bosses(&dir.join("missing.yaml")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
