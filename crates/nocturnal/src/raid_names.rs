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

/// One target: how much player damage it took in the window, and when it
/// first took any — the order a night is told in.
pub struct TargetDamage {
    pub target: String,
    pub damage: f64,
    pub first_seen_ms: i64,
}

/// The name: bosses in the table, in the order they were first engaged, joined the way
/// the guild writes them. A boss that took under 2% of all boss damage is
/// a tag, not a kill, and stays out. Shorthands dedupe (five VT bosses →
/// "VT"). `None` when nothing in the window is in the table.
pub fn pick(rows: &[TargetDamage], bosses: &BTreeMap<String, String>) -> Option<String> {
    let mut hits: Vec<(&str, f64, i64)> = rows
        .iter()
        .filter_map(|r| {
            bosses
                .get(&normalize(&r.target))
                .map(|s| (s.as_str(), r.damage, r.first_seen_ms))
        })
        .collect();
    if hits.is_empty() {
        return None;
    }
    let total: f64 = hits.iter().map(|h| h.1).sum();
    // The order of the night, not of the damage; ties by damage.
    hits.sort_by(|a, b| a.2.cmp(&b.2).then(b.1.total_cmp(&a.1)));
    let mut names: Vec<&str> = Vec::new();
    for (short, dmg, _) in hits {
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

/// Player damage by target over `[start_ms, end_ms]`, from Prometheus, with
/// the time each target first took any: one range query at a step that
/// keeps the window to ~120 points. Any failure is an empty list: naming is
/// best-effort and never blocks anything.
pub async fn targets_in_window(query_url: &str, start_ms: i64, end_ms: i64) -> Vec<TargetDamage> {
    let step_s = ((end_ms - start_ms) / 1000 / 120).clamp(60, 900);
    let query = format!(
        "sum by (everquest_combat_target) (increase(everquest_combat_damage_total{{everquest_combat_direction=\"outgoing\",everquest_combat_source_type=\"player\"}}[{step_s}s]))"
    );
    let range_url = query_url.trim_end_matches("/query").to_owned() + "/query_range";
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let resp = client
        .get(&range_url)
        .query(&[
            ("query", query.as_str()),
            ("start", &(start_ms / 1000).to_string()),
            ("end", &(end_ms / 1000).to_string()),
            ("step", &step_s.to_string()),
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

/// The reduce half of [`targets_in_window`] (a `query_range` matrix),
/// pinned by a fixture: damage is the sum over steps, first-seen the first
/// step with any.
pub fn parse_targets(body: &serde_json::Value) -> Vec<TargetDamage> {
    body["data"]["result"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let target = r["metric"]["everquest_combat_target"].as_str()?.to_owned();
                    let mut damage = 0.0;
                    let mut first_seen_ms: Option<i64> = None;
                    for v in r["values"].as_array()? {
                        let t = v[0].as_f64()?;
                        let d: f64 = v[1].as_str()?.parse().ok()?;
                        if d > 0.0 {
                            damage += d;
                            first_seen_ms.get_or_insert((t * 1000.0) as i64);
                        }
                    }
                    Some(TargetDamage {
                        target,
                        damage,
                        first_seen_ms: first_seen_ms?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every ended raid still on a placeholder name gets one from what it
/// fought. Run at boot and periodically, so a raid names itself even when
/// Prometheus was unreachable at `/endraid`, and last night's raids get
/// their names without anyone typing a command.
pub async fn name_unnamed_raids(
    driver: &crate::driver::DriverHandle,
    ledger_guild: u64,
    query_url: &str,
    bosses_path: &Path,
) -> usize {
    let unnamed: Vec<(String, i64, i64)> = driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .map(|g| {
                    g.raids
                        .iter()
                        .filter(|(_, r)| {
                            !r.active
                                && r.ended_ms.is_some()
                                && nocturnal_core::state::is_placeholder_raid_name(&r.name)
                        })
                        .map(|(id, r)| (id.clone(), r.date_ms, r.ended_ms.unwrap_or(r.date_ms)))
                        .collect()
                })
                .unwrap_or_default()
        })
        .await;
    if unnamed.is_empty() {
        return 0;
    }
    let bosses = load_bosses(bosses_path);
    let mut named = 0;
    for (raid_id, start_ms, end_ms) in unnamed {
        let rows = targets_in_window(query_url, start_ms, end_ms).await;
        let Some(name) = pick(&rows, &bosses) else {
            continue;
        };
        match driver
            .execute(
                ledger_guild,
                nocturnal_core::Actor::System,
                nocturnal_core::Command::RenameRaid {
                    raid_id: raid_id.clone(),
                    name: name.clone(),
                },
            )
            .await
        {
            Ok(_) => {
                tracing::info!(raid = %raid_id, %name, "named a raid from what it fought");
                named += 1;
            }
            Err(e) => tracing::warn!(raid = %raid_id, error = %e, "naming a raid failed"),
        }
    }
    named
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
        at(t, d, 0)
    }

    fn at(t: &str, d: f64, first_seen_ms: i64) -> TargetDamage {
        TargetDamage {
            target: t.into(),
            damage: d,
            first_seen_ms,
        }
    }

    #[test]
    fn the_night_of_2026_09_03_names_itself_and_trash_cannot() {
        // Real numbers: the Ring War trash out-damaged Vulak 2.5×. Order is
        // the order of the night, not of the damage.
        let rows = vec![
            at("#Vyzh`dra the Cursed", 60118.0, 2_000),
            at("Kromrif Veteran", 24569.0, 3_000),
            at("#Vyzh`dra the Exiled", 24400.0, 2_100),
            at("Kromrif Recruit", 23517.0, 3_000),
            at("#a glyph covered serpent", 20562.0, 2_500),
            at("a cerulean warden", 16543.0, 3_100),
            at("Narandi the Wretched", 13112.0, 3_500),
            at("#Vulak`Aerr", 9578.0, 1_000),
        ];
        assert_eq!(pick(&rows, &table()).unwrap(), "Vulak, Cursed & Ring War");
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
            {"metric": {"everquest_combat_target": "#Vulak`Aerr"},
             "values": [[100.0, "0"], [160.0, "4000"], [220.0, "5578"]]},
            {"metric": {"everquest_combat_target": "never hit"}, "values": [[100.0, "0"]]},
        ]}});
        let rows = parse_targets(&body);
        assert_eq!(
            rows.len(),
            1,
            "a target that never took damage is not a row"
        );
        assert_eq!(rows[0].target, "#Vulak`Aerr");
        assert_eq!(rows[0].damage, 9578.0);
        assert_eq!(rows[0].first_seen_ms, 160_000);
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
