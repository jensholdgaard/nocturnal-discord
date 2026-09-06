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
    /// The last step in which it took any: with no lockout notice, the best
    /// guess at when it died.
    pub last_seen_ms: i64,
}

/// A boss from the table that took a real share of the night: the kill
/// list and the raid's name are both built from these.
pub struct BossHit<'a> {
    pub target: &'a str,
    pub short: &'a str,
    pub last_seen_ms: i64,
}

/// The bosses in the table that took at least 2% of all boss damage, in the
/// order they were first engaged. A boss under that is a tag, not a kill.
pub fn boss_hits<'a>(
    rows: &'a [TargetDamage],
    bosses: &'a BTreeMap<String, String>,
) -> Vec<BossHit<'a>> {
    let mut hits: Vec<(&TargetDamage, &str)> = rows
        .iter()
        .filter_map(|r| bosses.get(&normalize(&r.target)).map(|s| (r, s.as_str())))
        .collect();
    if hits.is_empty() {
        return Vec::new();
    }
    let total: f64 = hits.iter().map(|h| h.0.damage).sum();
    hits.sort_by(|a, b| {
        a.0.first_seen_ms
            .cmp(&b.0.first_seen_ms)
            .then(b.0.damage.total_cmp(&a.0.damage))
    });
    hits.into_iter()
        .filter(|(r, _)| r.damage >= total * 0.02)
        .map(|(r, short)| BossHit {
            target: r.target.as_str(),
            short,
            last_seen_ms: r.last_seen_ms,
        })
        .collect()
}

/// The name: bosses in the table, in the order they were first engaged, joined the way
/// the guild writes them. A boss that took under 2% of all boss damage is
/// a tag, not a kill, and stays out. Shorthands dedupe (five VT bosses →
/// "VT"). `None` when nothing in the window is in the table.
pub fn pick(rows: &[TargetDamage], bosses: &BTreeMap<String, String>) -> Option<String> {
    let mut names: Vec<&str> = Vec::new();
    for h in boss_hits(rows, bosses) {
        if !names.contains(&h.short) {
            names.push(h.short);
        }
    }
    Some(match names.len() {
        0 => return None,
        1 => names[0].to_owned(),
        2 => format!("{} & {}", names[0], names[1]),
        n => format!("{} & {}", names[..n - 1].join(", "), names[n - 1]),
    })
}

/// The kill list for a raid: every boss hit, timed by the server's lockout
/// notice when a reporter received one (`Lockout`, exact), otherwise by the
/// last damage the boss took (`Damage`, a guess within a scrape step). The
/// name is the guild's shorthand, so "Narandi the Wretched" is a kill under
/// "Ring War".
pub fn kills(
    rows: &[TargetDamage],
    lockouts: &BTreeMap<String, i64>,
    deaths: &BTreeMap<String, Vec<i64>>,
    bosses: &BTreeMap<String, String>,
) -> Vec<nocturnal_core::RaidKill> {
    // The death packet is the server saying "this spawn died": every one of
    // them is a kill, whether or not any meter saw the damage. A table boss
    // with deaths takes nothing from the other two sources.
    let mut out: Vec<nocturnal_core::RaidKill> = deaths
        .iter()
        .filter_map(|(target, times)| bosses.get(target).map(|short| (target, short, times)))
        .flat_map(|(target, short, times)| {
            times.iter().map(move |t| nocturnal_core::RaidKill {
                target: target.clone(),
                name: short.clone(),
                killed_ms: *t,
                evidence: nocturnal_core::KillEvidence::Death,
            })
        })
        .collect();
    let dead = |name: &str| deaths.contains_key(&normalize(name));
    out.extend(
        boss_hits(rows, bosses)
            .into_iter()
            .filter(|h| !dead(h.target))
            .map(|h| {
                let clean = h.target.trim().trim_start_matches('#').trim();
                match lockouts.get(&normalize(clean)) {
                    Some(kill_ms) => nocturnal_core::RaidKill {
                        target: clean.to_owned(),
                        name: h.short.to_owned(),
                        killed_ms: *kill_ms,
                        evidence: nocturnal_core::KillEvidence::Lockout,
                    },
                    None => nocturnal_core::RaidKill {
                        target: clean.to_owned(),
                        name: h.short.to_owned(),
                        killed_ms: h.last_seen_ms,
                        evidence: nocturnal_core::KillEvidence::Damage,
                    },
                }
            }),
    );
    // A lockout notice for a boss nobody's meter saw take damage (every
    // reporter was elsewhere) is still a kill.
    for (target, kill_ms) in lockouts {
        if out.iter().any(|k| normalize(&k.target) == *target) {
            continue;
        }
        if let Some(short) = bosses.get(target) {
            out.push(nocturnal_core::RaidKill {
                target: target.clone(),
                name: short.clone(),
                killed_ms: *kill_ms,
                evidence: nocturnal_core::KillEvidence::Lockout,
            });
        }
    }
    out.sort_by_key(|k| k.killed_ms);
    out
}

/// Deaths from the death packet, as the meters reported them into Ourios,
/// in `[start_ms, end_ms]`: by normalized target, the kill times, one per
/// individual spawn. Several meters in the zone report the same death, so
/// a spawn id seen within a minute of itself is one kill. Ourios is asked
/// for the raid's window exactly (RFC 3339 bounds); failure is empty.
pub async fn deaths_in_window(
    query_url: &str,
    tenant: &str,
    start_ms: i64,
    end_ms: i64,
) -> BTreeMap<String, Vec<i64>> {
    let (Some(from), Some(to)) = (rfc3339(start_ms), rfc3339(end_ms + 60_000)) else {
        return BTreeMap::new();
    };
    let query =
        format!("event_name == \"everquest.combat.death\" | range({from}, {to}) | limit 20000");
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .build()
    {
        Ok(c) => c,
        Err(_) => return BTreeMap::new(),
    };
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
            tracing::warn!(status = %r.status(), "ourios refused the death query");
            return BTreeMap::new();
        }
        Err(e) => {
            tracing::warn!(error = %e, "ourios unreachable for deaths");
            return BTreeMap::new();
        }
    };
    let records = body["records"].as_array().cloned().unwrap_or_default();
    parse_deaths(&records, start_ms, end_ms)
}

/// An RFC 3339 instant at second precision, UTC, without a date crate: the
/// civil-from-days arithmetic (Hinnant), pinned by a test against a known
/// instant. `None` before the epoch.
fn rfc3339(ms: i64) -> Option<String> {
    if ms < 0 {
        return None;
    }
    let secs = ms / 1000;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    Some(format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z"))
}

/// The reduce half of [`deaths_in_window`], pinned by a test: records
/// outside the window are dropped, and the same spawn id reported by several
/// meters within a minute is one death.
pub fn parse_deaths(
    records: &[serde_json::Value],
    start_ms: i64,
    end_ms: i64,
) -> BTreeMap<String, Vec<i64>> {
    // (target, spawn id) -> kill times, deduped by the minute
    let mut seen: BTreeMap<(String, String), Vec<i64>> = BTreeMap::new();
    for r in records {
        let attrs = &r["attributes"];
        let Some(target) = crate::profiles::string_attr(attrs, "everquest.combat.target") else {
            continue;
        };
        let spawn = crate::profiles::string_attr(attrs, "everquest.spawn.id").unwrap_or_default();
        let ms = crate::profiles::nanos(&r["time_unix_nano"]) / 1_000_000;
        if ms < start_ms || ms > end_ms + 60_000 {
            continue;
        }
        let times = seen.entry((normalize(&target), spawn)).or_default();
        if times.iter().any(|t| (t - ms).abs() < 60_000) {
            continue;
        }
        times.push(ms);
    }
    let mut out: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for ((target, _), times) in seen {
        out.entry(target).or_default().extend(times);
    }
    for times in out.values_mut() {
        times.sort_unstable();
    }
    out
}

/// Kill timestamps from the server's lockout notices in `[start_ms, end_ms]`,
/// by normalized target: the newest value of the kill gauge per target at
/// the end of the window, kept only if the kill itself fell inside it.
pub async fn lockouts_in_window(
    query_url: &str,
    start_ms: i64,
    end_ms: i64,
) -> BTreeMap<String, i64> {
    let range_s = ((end_ms - start_ms) / 1000).max(60);
    let query = format!(
        "max by (everquest_raid_target) (max_over_time(everquest_raid_kill_timestamp_seconds[{range_s}s]))"
    );
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(_) => return BTreeMap::new(),
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
        _ => return BTreeMap::new(),
    };
    parse_lockouts(&body, start_ms, end_ms)
}

/// The reduce half of [`lockouts_in_window`] (an instant vector), pinned by
/// a test: a kill outside the window is a different night's.
pub fn parse_lockouts(
    body: &serde_json::Value,
    start_ms: i64,
    end_ms: i64,
) -> BTreeMap<String, i64> {
    body["data"]["result"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let target = r["metric"]["everquest_raid_target"].as_str()?;
                    let secs: f64 = r["value"][1].as_str()?.parse().ok()?;
                    let ms = (secs * 1000.0) as i64;
                    (start_ms..=end_ms + 60_000)
                        .contains(&ms)
                        .then(|| (normalize(target), ms))
                })
                .collect()
        })
        .unwrap_or_default()
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
/// step with any. Rows that are the same NPC — the client prefixes `#` on
/// an instanced spawn for some samples and not others — are one target,
/// or a boss would be two kills and half its damage.
pub fn parse_targets(body: &serde_json::Value) -> Vec<TargetDamage> {
    let raw = parse_target_rows(body);
    let mut merged: BTreeMap<String, TargetDamage> = BTreeMap::new();
    for r in raw {
        let key = normalize(&r.target);
        match merged.get_mut(&key) {
            Some(m) => {
                m.damage += r.damage;
                m.first_seen_ms = m.first_seen_ms.min(r.first_seen_ms);
                m.last_seen_ms = m.last_seen_ms.max(r.last_seen_ms);
            }
            None => {
                merged.insert(
                    key,
                    TargetDamage {
                        target: r.target.trim().trim_start_matches('#').trim().to_owned(),
                        ..r
                    },
                );
            }
        }
    }
    merged.into_values().collect()
}

fn parse_target_rows(body: &serde_json::Value) -> Vec<TargetDamage> {
    body["data"]["result"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let target = r["metric"]["everquest_combat_target"].as_str()?.to_owned();
                    let mut damage = 0.0;
                    let mut first_seen_ms: Option<i64> = None;
                    let mut last_seen_ms: i64 = 0;
                    for v in r["values"].as_array()? {
                        let t = v[0].as_f64()?;
                        let d: f64 = v[1].as_str()?.parse().ok()?;
                        if d > 0.0 {
                            damage += d;
                            first_seen_ms.get_or_insert((t * 1000.0) as i64);
                            last_seen_ms = (t * 1000.0) as i64;
                        }
                    }
                    Some(TargetDamage {
                        target,
                        damage,
                        first_seen_ms: first_seen_ms?,
                        last_seen_ms,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Work out and record what one raid killed. Returns the kills recorded, or
/// `None` when nothing could be established or nothing changed. Best-effort
/// like naming: a Prometheus outage means the next pass tries again.
#[allow(clippy::too_many_arguments)] // one per source of truth: ledger, the two stores, the table
pub async fn record_kills(
    driver: &crate::driver::DriverHandle,
    ledger_guild: u64,
    raid_id: &str,
    start_ms: i64,
    end_ms: i64,
    query_url: &str,
    ourios: Option<&(String, String)>,
    bosses: &BTreeMap<String, String>,
) -> Option<Vec<nocturnal_core::RaidKill>> {
    let rows = targets_in_window(query_url, start_ms, end_ms).await;
    let lockouts = lockouts_in_window(query_url, start_ms, end_ms).await;
    let deaths = match ourios {
        Some((url, tenant)) => deaths_in_window(url, tenant, start_ms, end_ms).await,
        None => BTreeMap::new(),
    };
    let list = kills(&rows, &lockouts, &deaths, bosses);
    if list.is_empty() {
        return None;
    }
    match driver
        .execute(
            ledger_guild,
            nocturnal_core::Actor::System,
            nocturnal_core::Command::RecordRaidKills {
                raid_id: raid_id.to_owned(),
                kills: list.clone(),
            },
        )
        .await
    {
        Ok(_) => {
            tracing::info!(raid = %raid_id, kills = list.len(), "recorded what a raid killed");
            Some(list)
        }
        Err(crate::driver::ExecError::Rejected(nocturnal_core::Rejection::NothingToRecord)) => None,
        Err(e) => {
            tracing::warn!(raid = %raid_id, error = %e, "recording a raid's kills failed");
            None
        }
    }
}

/// How far back the periodic pass looks for raids without a kill list:
/// long enough to catch a night Prometheus missed, short enough that the
/// pre-telemetry history is not re-queried every half hour.
const KILL_LOOKBACK_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Every ended raid of the last month gets a pass, recorded or not: a
/// lockout notice that arrived late, a reporter's data that Prometheus had
/// not ingested at `/endraid`, or a better table entry all improve the
/// list, and the ledger takes only a list that differs (NothingToRecord
/// otherwise), so this is two Prometheus queries per raid per half hour and
/// no events.
pub async fn record_recent_kills(
    driver: &crate::driver::DriverHandle,
    ledger_guild: u64,
    query_url: &str,
    ourios: Option<&(String, String)>,
    bosses_path: &Path,
    now_ms: i64,
) -> usize {
    let pending: Vec<(String, i64, i64)> = driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .map(|g| {
                    g.raids
                        .iter()
                        .filter(|(_, r)| {
                            !r.active && r.ended_ms.is_some_and(|e| now_ms - e < KILL_LOOKBACK_MS)
                        })
                        .map(|(id, r)| (id.clone(), r.date_ms, r.ended_ms.unwrap_or(r.date_ms)))
                        .collect()
                })
                .unwrap_or_default()
        })
        .await;
    if pending.is_empty() {
        return 0;
    }
    let bosses = load_bosses(bosses_path);
    let mut recorded = 0;
    for (raid_id, start_ms, end_ms) in pending {
        if record_kills(
            driver,
            ledger_guild,
            &raid_id,
            start_ms,
            end_ms,
            query_url,
            ourios,
            &bosses,
        )
        .await
        .is_some()
        {
            recorded += 1;
        }
    }
    recorded
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
            last_seen_ms: first_seen_ms + 600_000,
        }
    }

    /// The kill list of that same night: three bosses, the Ring War as two
    /// NPCs under one name; a lockout notice makes Vulak's time exact, the
    /// rest are timed by their last damage.
    #[test]
    fn kills_follow_the_hits_and_take_the_lockout_time_when_there_is_one() {
        use nocturnal_core::KillEvidence;
        let rows = vec![
            at("#Vyzh`dra the Cursed", 60118.0, 2_000),
            at("Kromrif Veteran", 24569.0, 3_000),
            at("Narandi the Wretched", 13112.0, 3_500),
            at("#Vulak`Aerr", 9578.0, 1_000),
            at("Dain Frostreaver IV", 30.0, 3_600), // a tag: 0.03 %
        ];
        let mut lockouts = BTreeMap::new();
        lockouts.insert(normalize("Vulak`Aerr"), 1_500_000i64);
        let k = kills(&rows, &lockouts, &BTreeMap::new(), &table());
        let got: Vec<(&str, &str, i64, KillEvidence)> = k
            .iter()
            .map(|k| (k.target.as_str(), k.name.as_str(), k.killed_ms, k.evidence))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "Vyzh`dra the Cursed",
                    "Cursed",
                    602_000,
                    KillEvidence::Damage
                ),
                (
                    "Narandi the Wretched",
                    "Ring War",
                    603_500,
                    KillEvidence::Damage
                ),
                ("Vulak`Aerr", "Vulak", 1_500_000, KillEvidence::Lockout),
            ]
        );
        // A lockout for a boss no meter saw is still a kill; trash never is.
        lockouts.insert(normalize("Thall Va Kelun"), 700_000);
        lockouts.insert(normalize("a cerulean warden"), 800_000);
        let k = kills(&rows, &lockouts, &BTreeMap::new(), &table());
        assert!(k
            .iter()
            .any(|k| k.target == "thall va kelun" && k.name == "VT"));
        assert!(!k.iter().any(|k| k.target.contains("warden")));
    }

    /// The death packet beats both other sources, and counts individuals: the
    /// Cursed died twice tonight, and the second one is a kill of its own.
    #[test]
    fn deaths_win_and_count_each_spawn() {
        use nocturnal_core::KillEvidence;
        let rows = vec![
            at("#Vyzh`dra the Cursed", 60118.0, 2_000),
            at("#Vulak`Aerr", 9578.0, 1_000),
        ];
        let mut lockouts = BTreeMap::new();
        lockouts.insert(normalize("Vyzh`dra the Cursed"), 900_000i64);
        let mut deaths = BTreeMap::new();
        deaths.insert(
            normalize("Vyzh`dra the Cursed"),
            vec![650_000i64, 3_000_000],
        );
        deaths.insert(normalize("a cerulean warden"), vec![700_000]);
        let k = kills(&rows, &lockouts, &deaths, &table());
        let got: Vec<(&str, i64, KillEvidence)> = k
            .iter()
            .map(|k| (k.name.as_str(), k.killed_ms, k.evidence))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Vulak", 601_000, KillEvidence::Damage),
                ("Cursed", 650_000, KillEvidence::Death),
                ("Cursed", 3_000_000, KillEvidence::Death),
            ]
        );
    }

    /// Several meters report one death; one spawn id within a minute is one.
    #[test]
    fn death_records_dedupe_by_spawn_and_stay_in_the_window() {
        let rec = |t: i64, target: &str, spawn: &str| {
            json!({
                "time_unix_nano": t * 1_000_000,
                "attributes": [
                    {"key": "everquest.combat.target", "value": {"stringValue": target}},
                    {"key": "everquest.spawn.id", "value": {"stringValue": spawn}},
                ]
            })
        };
        let records = vec![
            rec(500_000, "Vulak`Aerr", "40211"),
            rec(500_400, "Vulak`Aerr", "40211"), // another reporter, same death
            rec(4_000_000, "Vulak`Aerr", "40999"), // a respawn, much later
            rec(50_000, "Vulak`Aerr", "1"),      // before the raid
            rec(600_000, "a cerulean warden", "7"),
        ];
        let d = parse_deaths(&records, 100_000, 5_000_000);
        assert_eq!(d.get("vulak`aerr"), Some(&vec![500_000, 4_000_000]));
        assert_eq!(d.get("a cerulean warden"), Some(&vec![600_000]));
    }

    #[test]
    fn rfc3339_matches_a_known_instant() {
        // 2026-09-05T22:26:53Z: the instant Prometheus logged its tracer install.
        assert_eq!(
            rfc3339(1_788_647_213_000).as_deref(),
            Some("2026-09-05T22:26:53Z")
        );
        assert_eq!(rfc3339(0).as_deref(), Some("1970-01-01T00:00:00Z"));
        assert_eq!(
            rfc3339(951_782_400_000).as_deref(),
            Some("2000-02-29T00:00:00Z")
        );
        assert!(rfc3339(-1).is_none());
    }

    #[test]
    fn lockout_rows_parse_and_stay_inside_the_window() {
        let body = json!({"data": {"result": [
            {"metric": {"everquest_raid_target": "Vulak`Aerr"}, "value": [200.0, "150"]},
            {"metric": {"everquest_raid_target": "Kaas Thox Xi Ans Dyek"}, "value": [200.0, "50"]},
        ]}});
        let l = parse_lockouts(&body, 100_000, 200_000);
        assert_eq!(l.get("vulak`aerr"), Some(&150_000));
        assert!(
            !l.contains_key("kaas thox xi ans dyek"),
            "killed before the raid started"
        );
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
        assert_eq!(
            rows[0].target, "Vulak`Aerr",
            "the instance marker is dropped"
        );
        assert_eq!(rows[0].damage, 9578.0);
        assert_eq!(rows[0].first_seen_ms, 160_000);
        assert_eq!(rows[0].last_seen_ms, 220_000);
        // The same boss with and without the instance marker is one row.
        let body = json!({"data": {"result": [
            {"metric": {"everquest_combat_target": "#Lord Inquisitor Seru"},
             "values": [[100.0, "4000"], [160.0, "0"]]},
            {"metric": {"everquest_combat_target": "Lord Inquisitor Seru"},
             "values": [[100.0, "0"], [160.0, "6000"], [220.0, "1000"]]},
        ]}});
        let rows = parse_targets(&body);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target, "Lord Inquisitor Seru");
        assert_eq!(rows[0].damage, 11000.0);
        assert_eq!(
            (rows[0].first_seen_ms, rows[0].last_seen_ms),
            (100_000, 220_000)
        );
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
