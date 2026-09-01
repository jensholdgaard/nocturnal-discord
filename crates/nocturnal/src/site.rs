//! The site's data, typed.
//!
//! Every page the bot renders and `site.json` are views of this one struct,
//! built from the ledger on each change. A page is a Maud template over
//! these types, so a wrong field is a compile error rather than a blank tab.

use std::collections::{BTreeMap, HashMap};

use nocturnal_core::state::GuildState;
use nocturnal_core::{MainRank, PlayerId};
use serde::Serialize;

use crate::items::ItemSummary;
use crate::profiles::Profile;
use crate::roster_page::MemberInfo;

#[derive(Debug, Clone, Serialize)]
pub struct LootView {
    pub ts_ms: i64,
    pub item: String,
    pub winner: String,
    pub cost: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RaidView {
    pub id: String,
    /// Ledger ids folded into this one by `GuildState::raid_nights`: a false
    /// `/startraid` redone under the same name minutes later. `/raid/<alias>`
    /// still resolves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub name: String,
    pub date_ms: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    /// `/endraid`'s timestamp is known; otherwise the last tick, and the page says so.
    pub exact: bool,
    pub ticks: usize,
    pub dkp_per_tick: i64,
    pub attendees: Vec<String>,
    /// Roster characters of everyone who attended, lowercased.
    pub attendee_characters: Vec<String>,
    pub loot: Vec<LootView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CharacterView {
    pub name: String,
    pub class: String,
    pub level: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aa: Option<u16>,
    pub main: Option<MainRank>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemberView {
    /// The site's name for them: roster main, then any character, then Discord.
    pub name: String,
    pub discord: String,
    pub dkp: i64,
    pub attendance: f64,
    pub raids_attended: usize,
    pub last_active_ms: i64,
    pub history: Vec<serde_json::Value>,
    pub characters: Vec<CharacterView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersonView {
    pub discord: Option<String>,
    pub characters: Vec<CharacterView>,
    pub raiding: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AwardView {
    pub ts_ms: i64,
    pub raid: String,
    pub winner: String,
    pub cost: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemView {
    pub id: String,
    pub url: Option<String>,
    pub image: Option<String>,
    pub data: Option<String>,
    pub history: Vec<AwardView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpcomingView {
    pub id: String,
    pub title: String,
    pub start_ms: i64,
    pub signups: usize,
}

/// The live snapshot the page server renders from: replaced whole on every
/// render, read by every request. `None` until the first render after boot.
pub type SiteHandle = std::sync::Arc<std::sync::RwLock<Option<std::sync::Arc<SiteData>>>>;

#[derive(Debug, Clone, Serialize, Default)]
pub struct SiteData {
    #[serde(rename = "generatedAt")]
    pub generated_ms: i64,
    #[serde(rename = "avgAttendance")]
    pub avg_attendance: Option<f64>,
    pub raids: Vec<RaidView>,
    pub upcoming: Vec<UpcomingView>,
    /// Keyed by Discord username — what the login reports.
    pub members: BTreeMap<String, MemberView>,
    /// Keyed by the site's name for a person.
    pub people: BTreeMap<String, PersonView>,
    /// Keyed by item name.
    pub items: BTreeMap<String, ItemView>,
    /// Keyed by character name, lowercased.
    pub profiles: BTreeMap<String, Profile>,
    /// Keyed by item id, for the gear pages.
    pub gear_items: BTreeMap<String, ItemSummary>,
}

/// One squished line of a member's ledger: a raid's ticks as one entry
/// ("+33 · Vulak & Ring War · 33 ticks") rather than thirty-three "+1"s,
/// with loot and adjustments kept as their own lines.
pub fn squish_history(
    log: &[nocturnal_core::state::LogEntry],
    max: usize,
) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut i = log.len();
    while i > 0 && out.len() < max {
        i -= 1;
        let e = &log[i];
        let is_tick =
            e.dkp > 0 && (e.comment == "Tick" || e.comment == "Start") && e.raid.is_some();
        if is_tick {
            // Absorb every earlier tick of the same raid.
            let raid = e.raid.as_ref();
            let mut ticks = 1;
            let mut dkp = e.dkp;
            let last_ts = e.ts_ms;
            let mut first_ts = e.ts_ms;
            while i > 0 {
                let f = &log[i - 1];
                let same = f.dkp > 0
                    && (f.comment == "Tick" || f.comment == "Start")
                    && same_raid_ref(f.raid.as_ref(), raid, first_ts - f.ts_ms);
                if !same {
                    break;
                }
                ticks += 1;
                dkp += f.dkp;
                first_ts = f.ts_ms;
                i -= 1;
            }
            out.push(serde_json::json!({
                "kind": "raid", "dkp": dkp, "ticks": ticks, "ts_ms": last_ts,
                "raid": e.raid.as_ref().map(|r| r.name.clone()),
            }));
        } else {
            out.push(serde_json::json!({
                "kind": if e.item.is_some() { "loot" } else { "adjust" },
                "dkp": e.dkp, "comment": e.comment, "ts_ms": e.ts_ms,
                "raid": e.raid.as_ref().map(|r| r.name.clone()),
                "item": e.item.as_ref().map(|i| i.name.clone()),
            }));
        }
    }
    out
}

fn characters_of(g: &GuildState, id: PlayerId) -> Vec<CharacterView> {
    g.roster
        .get(&id)
        .map(|cs| {
            cs.values()
                .map(|c| CharacterView {
                    name: c.name.clone(),
                    class: c.class.clone(),
                    level: c.level,
                    aa: c.aa,
                    main: c.main,
                })
                .collect()
        })
        .unwrap_or_default()
}

impl SiteData {
    /// The site's name for a player: roster main, any roster character, the
    /// Discord display name, "unknown". The first two are ledger data — set
    /// once, verifiable in game — where a Discord name is whatever the
    /// member typed this week and cannot be enforced.
    pub fn name_for(g: &GuildState, members: &HashMap<u64, MemberInfo>, id: PlayerId) -> String {
        if let Some(chars) = g.roster.get(&id) {
            if let Some(main) = chars.values().find(|c| c.main == Some(MainRank::Main)) {
                return main.name.clone();
            }
            if let Some(any) = chars.values().next() {
                return any.name.clone();
            }
        }
        members
            .get(&id)
            .map(|m| m.display_name.clone())
            .unwrap_or_else(|| "unknown".to_owned())
    }

    /// Everything but profiles and gear, which arrive from Ourios and are
    /// attached by the caller.
    pub fn build(
        g: &GuildState,
        members: &HashMap<u64, MemberInfo>,
        now_ms: i64,
        upcoming: Vec<UpcomingView>,
    ) -> SiteData {
        let name = |id: &PlayerId| Self::name_for(g, members, *id);

        let raids: Vec<RaidView> = g
            .raid_nights()
            .iter()
            .take(8)
            .map(|group| {
                // The canonical member is the real raid: most entries, then
                // the earlier one. Everyone else in the group is an alias.
                let (cid, canon) = group
                    .iter()
                    .max_by_key(|(_, r)| (r.entries.len(), std::cmp::Reverse(r.date_ms)))
                    .copied()
                    .expect("groups are never empty");
                let ids: Vec<&String> = group.iter().map(|(id, _)| *id).collect();
                let mut loot: Vec<LootView> = Vec::new();
                for (pid, p) in &g.players {
                    for e in &p.log {
                        if e.dkp < 0
                            && e.raid
                                .as_ref()
                                .is_some_and(|x| ids.iter().any(|i| **i == x.raid_id))
                        {
                            loot.push(LootView {
                                ts_ms: e.ts_ms,
                                item: e
                                    .item
                                    .as_ref()
                                    .map(|i| i.name.clone())
                                    .unwrap_or_else(|| e.comment.clone()),
                                winner: name(pid),
                                cost: -e.dkp,
                            });
                        }
                    }
                }
                loot.sort_by_key(|l| l.ts_ms);
                let mut entries: Vec<&nocturnal_core::state::AttendanceEntry> =
                    group.iter().flat_map(|(_, r)| r.entries.iter()).collect();
                entries.sort_by_key(|e| e.ts_ms);
                let attendees: std::collections::BTreeSet<PlayerId> = entries
                    .iter()
                    .flat_map(|e| e.players.iter().copied())
                    .collect();
                let date_ms = group
                    .iter()
                    .map(|(_, r)| r.date_ms)
                    .min()
                    .unwrap_or(canon.date_ms);
                RaidView {
                    id: cid.clone(),
                    aliases: ids
                        .iter()
                        .filter(|i| **i != cid)
                        .map(|i| (*i).clone())
                        .collect(),
                    name: canon.name.clone(),
                    date_ms,
                    start_ms: entries.first().map_or(date_ms, |e| e.ts_ms),
                    end_ms: group
                        .iter()
                        .map(|(_, r)| {
                            r.ended_ms
                                .unwrap_or_else(|| r.entries.last().map_or(r.date_ms, |e| e.ts_ms))
                        })
                        .max()
                        .unwrap_or(canon.date_ms),
                    exact: group.iter().all(|(_, r)| r.ended_ms.is_some()),
                    ticks: entries
                        .iter()
                        .filter(|e| e.comment == "Tick" || e.comment == "Start")
                        .count(),
                    dkp_per_tick: canon.dkp_per_tick,
                    attendees: attendees.iter().map(name).collect(),
                    attendee_characters: attendees
                        .iter()
                        .filter_map(|id| g.roster.get(id))
                        .flat_map(|cs| cs.values().map(|c| c.name.to_lowercase()))
                        .collect(),
                    loot,
                }
            })
            .collect();

        let mut members_out = BTreeMap::new();
        for (id, p) in g.raiding_players(now_ms) {
            let Some(m) = members.get(&id) else { continue };
            let attended = raids
                .iter()
                .filter(|v| {
                    std::iter::once(&v.id).chain(v.aliases.iter()).any(|rid| {
                        g.raids
                            .get(rid)
                            .is_some_and(|r| r.entries.iter().any(|e| e.players.contains(&id)))
                    })
                })
                .count();
            members_out.insert(
                m.username.clone(),
                MemberView {
                    name: name(&id),
                    discord: m.display_name.clone(),
                    dkp: p.balance,
                    attendance: g.attendance_pct(id, now_ms),
                    raids_attended: attended,
                    last_active_ms: p.log.last().map_or(0, |e| e.ts_ms),
                    history: squish_history(&p.log, 12),
                    characters: characters_of(g, id),
                },
            );
        }

        let mut items: BTreeMap<String, ItemView> = BTreeMap::new();
        for (pid, p) in &g.players {
            for e in &p.log {
                let Some(item) = &e.item else { continue };
                if e.dkp >= 0 {
                    continue;
                }
                let entry = items.entry(item.name.clone()).or_insert_with(|| ItemView {
                    id: item.id.clone(),
                    url: item.url.clone(),
                    image: item.image.clone(),
                    data: item.data.clone(),
                    history: Vec::new(),
                });
                entry.history.push(AwardView {
                    ts_ms: e.ts_ms,
                    raid: e.raid.as_ref().map(|r| r.name.clone()).unwrap_or_default(),
                    winner: name(pid),
                    cost: -e.dkp,
                });
            }
        }
        for it in items.values_mut() {
            it.history.sort_by_key(|a| std::cmp::Reverse(a.ts_ms));
        }

        let mut people = BTreeMap::new();
        let mut ids: Vec<PlayerId> = g.roster.keys().copied().collect();
        ids.extend(g.raiding_players(now_ms).map(|(id, _)| id));
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            people.insert(
                name(&id),
                PersonView {
                    discord: members.get(&id).map(|m| m.display_name.clone()),
                    characters: characters_of(g, id),
                    raiding: g.raiding_players(now_ms).any(|(p, _)| p == id),
                },
            );
        }

        SiteData {
            generated_ms: now_ms,
            avg_attendance: g.average_attendance(now_ms),
            raids,
            upcoming,
            members: members_out,
            people,
            items,
            profiles: BTreeMap::new(),
            gear_items: BTreeMap::new(),
        }
    }
}

/// Two log lines belong to the same raid night: the same ledger raid, or the
/// same name with the lines `gap_ms` apart at most.
fn same_raid_ref(
    a: Option<&nocturnal_core::event::RaidRef>,
    b: Option<&nocturnal_core::event::RaidRef>,
    gap_ms: i64,
) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            a.raid_id == b.raid_id
                || (a.name.trim().eq_ignore_ascii_case(b.name.trim())
                    && gap_ms <= nocturnal_core::state::SAME_RAID_GAP_MS)
        }
        (None, None) => true,
        _ => false,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion
mod tests {
    use super::*;
    use nocturnal_core::{Actor, Command, Ctx, Ledger};

    const GUILD: u64 = 42;

    fn run(l: &mut Ledger, now: i64, cmd: Command) {
        let envs = l
            .propose(
                &Ctx {
                    guild: GUILD,
                    actor: Actor::User(1),
                    now_ms: now,
                },
                &cmd,
            )
            .unwrap();
        l.commit(&envs);
    }

    fn start(l: &mut Ledger, now: i64, id: &str, name: &str, players: Vec<u64>) {
        run(
            l,
            now,
            Command::StartRaid {
                raid_id: id.into(),
                name: name.into(),
                tick_interval_ms: 60_000,
                dkp_per_tick: 1,
                players_present: players,
                event_id: None,
            },
        );
    }

    fn end(l: &mut Ledger, now: i64) {
        run(
            l,
            now,
            Command::EndRaid {
                players_present: vec![],
                reason: "officer".into(),
            },
        );
    }

    /// Aug 31 2026: a 3-minute phantom with a Start tick and one award,
    /// then the real raid nine seconds later.
    fn phantom_then_real() -> Ledger {
        let mut l = Ledger::new();
        start(&mut l, 1_000, "phantom", "Seru & Emp", vec![1, 2]);
        run(
            &mut l,
            1_200,
            Command::AdjustDkp {
                player: 1,
                delta: 100,
                comment: "seed".into(),
                item: None,
            },
        );
        run(
            &mut l,
            1_500,
            Command::AdjustDkp {
                player: 1,
                delta: -42,
                comment: "Sigil Earring".into(),
                item: None,
            },
        );
        end(&mut l, 2_000);
        start(&mut l, 3_000, "seru & emp ", "Seru & Emp", vec![1, 2, 3]);
        run(
            &mut l,
            70_000,
            Command::Tick {
                players_present: vec![1, 2, 3],
            },
        );
        end(&mut l, 80_000);
        l
    }

    #[test]
    fn a_false_start_reads_as_one_raid() {
        let l = phantom_then_real();
        let g = l.state().guild(GUILD).unwrap();
        let site = SiteData::build(g, &HashMap::new(), 100_000, vec![]);
        assert_eq!(
            site.raids.len(),
            1,
            "one raid night, not two: {:?}",
            site.raids
        );
        let r = &site.raids[0];
        assert_eq!(r.id, "seru & emp ", "the real raid is canonical");
        assert_eq!(r.aliases, vec!["phantom".to_owned()]);
        assert_eq!((r.start_ms, r.end_ms, r.exact), (1_000, 80_000, true));
        assert_eq!(r.ticks, 3, "phantom Start + real Start + one Tick");
        assert_eq!(r.attendees.len(), 3);
        assert_eq!(r.loot.len(), 1, "the phantom's award is on the raid");
        assert_eq!(r.loot[0].cost, 42);
    }

    #[test]
    fn different_names_or_a_long_gap_stay_apart() {
        let mut l = Ledger::new();
        start(&mut l, 1_000, "a", "Seru", vec![1]);
        end(&mut l, 2_000);
        start(&mut l, 3_000, "b", "Emp", vec![1]);
        end(&mut l, 4_000);
        start(
            &mut l,
            4_000 + nocturnal_core::state::SAME_RAID_GAP_MS + 1,
            "c",
            "Emp",
            vec![1],
        );
        end(&mut l, 4_000 + nocturnal_core::state::SAME_RAID_GAP_MS + 2);
        let g = l.state().guild(GUILD).unwrap();
        let site = SiteData::build(g, &HashMap::new(), 10_000_000, vec![]);
        let ids: Vec<&str> = site.raids.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["c", "b", "a"], "newest first, nothing folded");
        assert!(site.raids.iter().all(|r| r.aliases.is_empty()));
    }

    #[test]
    fn a_member_history_line_spans_the_false_start() {
        let l = phantom_then_real();
        let g = l.state().guild(GUILD).unwrap();
        // Player 2: phantom Start, real Start, Tick — one line, three ticks.
        let h = squish_history(&g.players[&2].log, 12);
        assert_eq!(h.len(), 1, "{h:?}");
        assert_eq!(h[0]["ticks"], 3);
        assert_eq!(h[0]["dkp"], 3);
    }
}
