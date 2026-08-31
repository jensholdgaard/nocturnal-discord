//! The roster page payload — what the Google Sheet used to be.
//!
//! `nocturnal-roster` (index.html) renders whatever JSON its `SCRIPT_URL`
//! returns, in the shape Apps Script produced from the sheet: a grid of
//! `values`, per-cell `notes` and `links`, a `styleDict` with a per-cell
//! `styleIndex`, and `headerHeights`. This module produces that shape from
//! the ledger, so the page keeps its matrix view and its formatting and
//! nothing in it changes but one constant.
//!
//! The look is a captured theme (`deploy/roster-theme.json`): the sheet's
//! style dictionary and which style each row kind and column used, taken
//! once from the live sheet. Styles are data, so the page looks the same on
//! day one and can be re-themed without touching code.
//!
//! Layout, row by row, exactly as the page's constants expect (HEADER_IDX=4,
//! FROZEN_ROWS=5, FROZEN_COLS=3):
//!   0  Nocturnal            · class names across the class columns
//!   1  Roster               · characters of that class
//!   2  Gnomes never forget  · "M"  · active level-60 mains of that class
//!   3  Raid force : N       · "M2" · active level-60 second mains
//!   4  the header
//!   5+ one row per member: name · DKP · RA · one cell per class · role · activity

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use nocturnal_core::state::GuildState;
use nocturnal_core::{GuildId, MainRank, PlayerId, RosterCharacter, CLASSES};
use nocturnal_telemetry::attr;
use poise::serenity_prelude as serenity;
use serde::Deserialize;

use crate::driver::DriverHandle;

/// What the page needs to know about a Discord member. Kept by player id.
#[derive(Debug, Clone)]
pub struct MemberInfo {
    pub display_name: String,
    /// The Discord username — what Perses' login reports as the viewer, so
    /// the site can find "me" in site.json.
    pub username: String,
    /// "Guild Leader" / "Officer" / "Member" / "" (not in the guild).
    pub guild_role: String,
    pub in_guild: bool,
}

/// The captured sheet theme. See the module doc.
#[derive(Debug, Deserialize)]
struct Theme {
    #[serde(rename = "styleDict")]
    style_dict: Vec<serde_json::Value>,
    preamble: Vec<Vec<usize>>,
    header: Vec<usize>,
    column: Vec<usize>,
    #[serde(rename = "headerHeights")]
    header_heights: Vec<f64>,
    #[serde(rename = "preambleValues")]
    preamble_values: Vec<Vec<String>>,
    header_row: Vec<String>,
    #[serde(rename = "classCellStyles")]
    class_cell_styles: HashMap<String, Vec<(usize, usize)>>,
}

fn theme() -> &'static Theme {
    static THEME: std::sync::OnceLock<Theme> = std::sync::OnceLock::new();
    THEME.get_or_init(|| {
        #[allow(clippy::expect_used)]
        serde_json::from_str(include_str!("../../../deploy/roster-theme.json"))
            .expect("deploy/roster-theme.json is valid — checked by a test")
    })
}

/// Column layout, shared with the sheet the page was built for.
const COL_NAME: usize = 0;
const COL_DKP: usize = 1;
const COL_RA: usize = 2;
const COL_CLASS0: usize = 3;
const COL_ROLE: usize = 18;
const COL_ACTIVITY: usize = 19;
const COLS: usize = 20;
/// A member counts toward the "active mains" headcounts above this.
const ACTIVE_MAIN_ATTENDANCE: f64 = 20.0;

/// Days since the Unix epoch → (year, month, day). Howard Hinnant's civil
/// algorithm; here so a date can be printed without pulling in a calendar
/// crate for one column.
fn civil_from_ms(ms: i64) -> (i64, u32, u32) {
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `26-08-27`, the sheet's Activity spelling.
fn activity_date(ms: i64) -> String {
    let (y, m, d) = civil_from_ms(ms);
    format!("{:02}-{m:02}-{d:02}", y.rem_euclid(100))
}

fn cell_text(c: &RosterCharacter) -> String {
    let rank = match c.main {
        Some(MainRank::Main) => "M-",
        Some(MainRank::Second) => "M2-",
        None => "",
    };
    format!("{} ({rank}{})", c.name, c.level)
}

fn cell_note(c: &RosterCharacter) -> String {
    let mut lines = Vec::new();
    if let Some(aa) = c.aa {
        lines.push(format!("AA: {aa}"));
    }
    if !c.access.is_empty() {
        lines.push(format!("Access: {}", c.access.join(", ")));
    }
    lines.join("\n")
}

fn class_style(t: &Theme, c: Option<&RosterCharacter>, column_default: usize) -> usize {
    let Some(c) = c else {
        return column_default;
    };
    let key = match (c.main, c.level) {
        (Some(MainRank::Main), _) => "M-",
        (Some(MainRank::Second), _) => "M2-",
        (None, 60) => "60",
        _ => "other",
    };
    t.class_cell_styles
        .get(key)
        .and_then(|v| v.first())
        .map_or(column_default, |(idx, _)| *idx)
}

/// One member's row, before it becomes cells.
struct Row<'a> {
    name: String,
    dkp: i64,
    attendance: f64,
    role: String,
    last_active_ms: i64,
    by_class: BTreeMap<&'static str, Vec<&'a RosterCharacter>>,
}

/// Build the payload. Pure: ledger state + member names in, JSON out, so it
/// is testable without Discord and reproducible from a replay.
pub fn render(
    g: &GuildState,
    members: &HashMap<u64, MemberInfo>,
    now_ms: i64,
) -> serde_json::Value {
    let t = theme();

    // Who gets a row: everyone on the roster, plus everyone the bot counts as
    // raiding — the sheet had both (the latter added from the DKP import).
    let mut ids: Vec<PlayerId> = g.roster.keys().copied().collect();
    for (id, _) in g.raiding_players(now_ms) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let mut rows: Vec<Row> = ids
        .into_iter()
        .map(|id| {
            let m = members.get(&id);
            let mut by_class: BTreeMap<&'static str, Vec<&RosterCharacter>> = BTreeMap::new();
            if let Some(chars) = g.roster.get(&id) {
                for c in chars.values() {
                    if let Some(k) = CLASSES.iter().find(|k| **k == c.class) {
                        by_class.entry(k).or_default().push(c);
                    }
                }
            }
            Row {
                name: m
                    .map(|m| m.display_name.clone())
                    .unwrap_or_else(|| "unknown".to_owned()),
                dkp: g.balance(id),
                attendance: g.attendance_pct(id, now_ms),
                // Someone who left keeps their row (their DKP is still real)
                // but is marked, so a raid planner does not count on them.
                role: match m {
                    Some(m) if m.in_guild => m.guild_role.clone(),
                    Some(_) => "Left".to_owned(),
                    None => String::new(),
                },
                last_active_ms: g
                    .players
                    .get(&id)
                    .and_then(|p| p.log.last())
                    .map_or(0, |e| e.ts_ms),
                by_class,
            }
        })
        .collect();
    rows.sort_by_key(|r| r.name.to_lowercase());

    // Headcounts for the preamble.
    let mut total = vec![0usize; CLASSES.len()];
    let mut mains = vec![0usize; CLASSES.len()];
    let mut seconds = vec![0usize; CLASSES.len()];
    let mut raid_force = 0usize;
    for r in &rows {
        let active = r.attendance >= ACTIVE_MAIN_ATTENDANCE;
        let mut counted = false;
        for (i, k) in CLASSES.iter().enumerate() {
            for c in r.by_class.get(k).into_iter().flatten() {
                total[i] += 1;
                if active && c.level == 60 {
                    match c.main {
                        Some(MainRank::Main) => {
                            mains[i] += 1;
                            counted = true;
                        }
                        Some(MainRank::Second) => seconds[i] += 1,
                        None => {}
                    }
                }
            }
        }
        if counted {
            raid_force += 1;
        }
    }

    let blank = || vec![String::new(); COLS];
    let mut values: Vec<Vec<String>> = Vec::new();
    let mut notes: Vec<Vec<String>> = Vec::new();
    let mut links: Vec<Vec<Option<String>>> = Vec::new();
    let mut style_index: Vec<Vec<usize>> = Vec::new();

    // Preamble: the captured text in the label columns, live counts in the
    // class columns.
    for (r, pre) in t.preamble_values.iter().enumerate().take(4) {
        let mut v = blank();
        for (slot, text) in v.iter_mut().zip(pre.iter()).take(3) {
            *slot = text.clone();
        }
        if r == 3 {
            v[0] = format!("Raid force : {raid_force}");
        }
        for (i, k) in CLASSES.iter().enumerate() {
            v[COL_CLASS0 + i] = match r {
                0 => (*k).to_owned(),
                1 => total[i].to_string(),
                2 => mains[i].to_string(),
                _ => seconds[i].to_string(),
            };
        }
        values.push(v);
        notes.push(blank());
        links.push(vec![None; COLS]);
        style_index.push(
            (0..COLS)
                .map(|c| {
                    t.preamble
                        .get(r)
                        .and_then(|p| p.get(c))
                        .copied()
                        .unwrap_or(0)
                })
                .collect(),
        );
    }
    // Header.
    values.push(
        (0..COLS)
            .map(|c| t.header_row.get(c).cloned().unwrap_or_default())
            .collect(),
    );
    notes.push(blank());
    links.push(vec![None; COLS]);
    style_index.push(
        (0..COLS)
            .map(|c| t.header.get(c).copied().unwrap_or(0))
            .collect(),
    );
    // Members.
    for r in &rows {
        let mut v = blank();
        let mut n = blank();
        let mut l = vec![None; COLS];
        let mut s: Vec<usize> = (0..COLS)
            .map(|c| t.column.get(c).copied().unwrap_or(0))
            .collect();
        v[COL_NAME] = r.name.clone();
        v[COL_DKP] = r.dkp.to_string();
        v[COL_RA] = format!("{}%", r.attendance.floor() as i64);
        v[COL_ROLE] = r.role.clone();
        v[COL_ACTIVITY] = if r.last_active_ms > 0 {
            activity_date(r.last_active_ms)
        } else {
            String::new()
        };
        for (i, k) in CLASSES.iter().enumerate() {
            let col = COL_CLASS0 + i;
            let chars = r.by_class.get(k).map(Vec::as_slice).unwrap_or(&[]);
            if chars.is_empty() {
                continue;
            }
            v[col] = chars
                .iter()
                .map(|c| cell_text(c))
                .collect::<Vec<_>>()
                .join(", ");
            n[col] = chars
                .iter()
                .map(|c| cell_note(c))
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            l[col] = chars.iter().find_map(|c| c.profile_url.clone());
            s[col] = class_style(t, chars.first().copied(), s[col]);
        }
        values.push(v);
        notes.push(n);
        links.push(l);
        style_index.push(s);
    }

    serde_json::json!({
        "values": values,
        "notes": notes,
        "links": links,
        "styleDict": t.style_dict,
        "styleIndex": style_index,
        "headerHeights": t.header_heights,
        "generatedAt": now_ms,
    })
}

/// The site's data: the last raids with who came and what dropped (with what
/// it cost — the guild keeps its loot history), and each member's own standing
/// keyed by Discord username so the page shows the viewer theirs. Behind the
/// login. What is deliberately absent is any per-item price aggregate: that
/// is a bidding guide, and the guild does not want one.
pub fn render_site(
    g: &GuildState,
    members: &HashMap<u64, MemberInfo>,
    now_ms: i64,
) -> serde_json::Value {
    let name = |id: &PlayerId| {
        members
            .get(id)
            .map(|m| m.display_name.clone())
            .unwrap_or_else(|| "unknown".to_owned())
    };
    let mut raids: Vec<(&String, &nocturnal_core::state::Raid)> = g.raids.iter().collect();
    raids.sort_by_key(|(_, r)| std::cmp::Reverse(r.date_ms));
    let raids: Vec<serde_json::Value> = raids
        .iter()
        .take(8)
        .map(|(id, r)| {
            let mut loot: Vec<(i64, String, String, i64)> = Vec::new();
            for (pid, p) in &g.players {
                for e in &p.log {
                    if e.dkp < 0 && e.raid.as_ref().map(|x| &x.raid_id) == Some(*id) {
                        let item = e
                            .item
                            .as_ref()
                            .map(|i| i.name.clone())
                            .unwrap_or_else(|| e.comment.clone());
                        loot.push((e.ts_ms, item, name(pid), -e.dkp));
                    }
                }
            }
            loot.sort();
            let attendees: std::collections::BTreeSet<PlayerId> =
                r.entries.iter().flat_map(|e| e.players.iter().copied()).collect();
            serde_json::json!({
                "id": id, "name": r.name, "date_ms": r.date_ms,
                "start_ms": r.entries.first().map_or(r.date_ms, |e| e.ts_ms),
                "end_ms": r.entries.last().map_or(r.date_ms, |e| e.ts_ms),
                "ticks": r.entries.iter().filter(|e| e.comment == "Tick" || e.comment == "Start").count(),
                "dkp_per_tick": r.dkp_per_tick,
                "attendees": attendees.iter().map(name).collect::<Vec<_>>(),
                "loot": loot.into_iter().map(|(ts, item, winner, cost)| serde_json::json!({"ts_ms": ts, "item": item, "winner": winner, "cost": cost})).collect::<Vec<_>>(),
            })
        })
        .collect();
    let recent: Vec<String> = raids
        .iter()
        .filter_map(|r| r["id"].as_str().map(String::from))
        .collect();
    let mut me = serde_json::Map::new();
    for (id, p) in g.raiding_players(now_ms) {
        let Some(m) = members.get(&id) else { continue };
        let attended = recent
            .iter()
            .filter(|rid| {
                g.raids
                    .get(*rid)
                    .is_some_and(|r| r.entries.iter().any(|e| e.players.contains(&id)))
            })
            .count();
        let history: Vec<serde_json::Value> = p
            .log
            .iter()
            .rev()
            .take(12)
            .map(|e| {
                serde_json::json!({
                    "dkp": e.dkp, "comment": e.comment, "ts_ms": e.ts_ms,
                    "raid": e.raid.as_ref().map(|r| r.name.clone()),
                    "item": e.item.as_ref().map(|i| i.name.clone()),
                })
            })
            .collect();
        let chars: Vec<serde_json::Value> = g
            .roster
            .get(&id)
            .map(|cs| {
                cs.values()
                    .map(|c| serde_json::json!({"name": c.name, "class": c.class, "level": c.level, "main": c.main}))
                    .collect()
            })
            .unwrap_or_default();
        me.insert(
            m.username.clone(),
            serde_json::json!({
                "name": m.display_name, "dkp": p.balance, "attendance": g.attendance_pct(id, now_ms),
                "raids_attended": attended, "last_active_ms": p.log.last().map_or(0, |e| e.ts_ms),
                "history": history, "characters": chars,
            }),
        );
    }
    serde_json::json!({
        "generatedAt": now_ms,
        "avgAttendance": g.average_attendance(now_ms),
        "raids": raids,
        "members": me,
    })
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(".roster.json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Fill in any member the cache does not know. One REST call per unknown
/// player, so the first run after boot is the slow one and every later run
/// is a handful of new names at most.
async fn resolve_members(
    http: &serenity::Http,
    guild: serenity::GuildId,
    cache: &std::sync::Mutex<HashMap<u64, MemberInfo>>,
    ids: &[PlayerId],
) {
    let missing: Vec<PlayerId> = {
        let c = cache.lock().map(|c| c.clone()).unwrap_or_default();
        ids.iter()
            .copied()
            .filter(|id| !c.contains_key(id))
            .collect()
    };
    if missing.is_empty() {
        return;
    }
    let roles: HashMap<u64, String> = match guild.roles(http).await {
        Ok(r) => r.into_iter().map(|(id, r)| (id.get(), r.name)).collect(),
        Err(_) => HashMap::new(),
    };
    for id in missing {
        let info = match guild.member(http, serenity::UserId::new(id)).await {
            Ok(m) => {
                let names: Vec<&str> = m
                    .roles
                    .iter()
                    .filter_map(|r| roles.get(&r.get()).map(String::as_str))
                    .collect();
                let guild_role = if names.contains(&"Guild Leader") {
                    "Guild Leader"
                } else if names.contains(&"Officer") {
                    "Officer"
                } else {
                    "Member"
                };
                MemberInfo {
                    display_name: m.display_name().to_string(),
                    username: m.user.name.clone(),
                    guild_role: guild_role.to_owned(),
                    in_guild: true,
                }
            }
            Err(_) => match serenity::UserId::new(id).to_user(http).await {
                Ok(u) => MemberInfo {
                    display_name: u.global_name.clone().unwrap_or_else(|| u.name.clone()),
                    username: u.name,
                    guild_role: String::new(),
                    in_guild: false,
                },
                Err(_) => continue,
            },
        };
        if let Ok(mut c) = cache.lock() {
            c.insert(id, info);
        }
    }
}

/// Re-render the page payload from the ledger and write it. Never fatal:
/// the roster command that triggered it has already succeeded in the ledger,
/// and the page is derived state that the next change re-derives.
pub async fn rematerialize(
    http: &serenity::Http,
    discord_guild: u64,
    driver: &DriverHandle,
    out: &Path,
    members_cache: &std::sync::Mutex<HashMap<u64, MemberInfo>>,
    ledger_guild: GuildId,
) {
    let now = crate::discord::chrono_now_ms();
    let ids: Vec<PlayerId> = driver
        .query(move |l| {
            l.state().guild(ledger_guild).map_or(Vec::new(), |g| {
                let mut ids: Vec<PlayerId> = g.roster.keys().copied().collect();
                ids.extend(g.raiding_players(now).map(|(id, _)| id));
                ids
            })
        })
        .await;
    resolve_members(
        http,
        serenity::GuildId::new(discord_guild),
        members_cache,
        &ids,
    )
    .await;
    let members = members_cache.lock().map(|m| m.clone()).unwrap_or_default();
    let both = driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .map(|g| (render(g, &members, now), render_site(g, &members, now)))
        })
        .await;
    let Some((json, site)) = both else {
        return;
    };
    let site_path = out.with_file_name("site.json");
    if let Err(e) = serde_json::to_vec(&site)
        .map_err(std::io::Error::other)
        .and_then(|b| write_atomic(&site_path, &b))
    {
        tracing::warn!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "site.json not written");
    }
    match serde_json::to_vec(&json)
        .map_err(std::io::Error::other)
        .and_then(|b| write_atomic(out, &b))
    {
        Ok(()) => tracing::info!(
            { attr::NOCTURNAL_ROSTER_ROWS } = json["values"]
                .as_array()
                .map_or(0, |v| v.len().saturating_sub(5)),
            "roster page rewritten"
        ),
        Err(e) => {
            tracing::warn!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "roster page not written; the ledger is still correct")
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_theme_loads() {
        let t = theme();
        assert_eq!(t.header_row.len(), COLS);
        assert_eq!(t.column.len(), COLS);
        assert!(t.style_dict.len() > 10);
    }

    #[test]
    fn dates_print_as_the_sheet_printed_them() {
        // 2026-08-27T20:00:13Z, the newest raid in the fixture dump.
        assert_eq!(activity_date(1_787_853_613_551), "26-08-27");
        assert_eq!(activity_date(0), "70-01-01");
    }

    /// The page reads the layout by fixed indices, so every row must be
    /// exactly COLS wide and the header must sit at row 4.
    #[test]
    fn the_payload_has_the_shape_the_page_expects() {
        use nocturnal_core::{Actor, Command, Ctx, Ledger};
        let mut l = Ledger::new();
        let ctx = Ctx {
            guild: 1,
            actor: Actor::User(7),
            now_ms: 1_787_853_613_551,
        };
        let c = RosterCharacter {
            name: "Shaku".into(),
            class: "Shaman".into(),
            level: 60,
            aa: Some(12),
            profile_url: Some("https://quarmy.com/c/shaku".into()),
            access: vec!["VP".into()],
            main: Some(MainRank::Main),
        };
        let envs = l
            .propose(
                &ctx,
                &Command::SetRosterCharacter {
                    player: 7,
                    character: c,
                    replace: false,
                },
            )
            .unwrap();
        l.commit(&envs);
        let mut members = HashMap::new();
        members.insert(
            7,
            MemberInfo {
                display_name: "Asberdies".into(),
                username: "asberdies".into(),
                guild_role: "Officer".into(),
                in_guild: true,
            },
        );
        let j = render(l.state().guild(1).unwrap(), &members, ctx.now_ms);
        let values = j["values"].as_array().unwrap();
        assert_eq!(values.len(), 6, "4 preamble + header + 1 member");
        assert!(values.iter().all(|r| r.as_array().unwrap().len() == COLS));
        assert_eq!(values[4][0], "Discord profile");
        let row = values[5].as_array().unwrap();
        assert_eq!(row[COL_NAME], "Asberdies");
        let shaman_col = COL_CLASS0 + CLASSES.iter().position(|k| *k == "Shaman").unwrap();
        assert_eq!(row[shaman_col], "Shaku (M-60)");
        assert_eq!(j["notes"][5][shaman_col], "AA: 12\nAccess: VP");
        assert_eq!(j["links"][5][shaman_col], "https://quarmy.com/c/shaku");
        assert_eq!(row[COL_ROLE], "Officer");
        // A 60 main with 100% attendance (no raids yet => 100) counts toward the headcounts.
        assert_eq!(values[2][shaman_col], "1", "active level-60 mains");
        assert_eq!(values[3][0], "Raid force : 1");
        for key in ["styleDict", "styleIndex", "headerHeights", "notes", "links"] {
            assert!(j[key].is_array(), "{key}");
        }
        assert_eq!(j["styleIndex"].as_array().unwrap().len(), values.len());
    }
}
