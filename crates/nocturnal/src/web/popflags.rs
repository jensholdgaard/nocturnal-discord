//! Reading a member's `#popflags` output back into flag values.
//!
//! Quarm keeps Planes of Power progression on the server as per-character
//! quest globals (`mavuin`, `fuirstel`, `zeks`, …); the client never holds
//! them. Since EQMacEmu #382 (2026-09-06) any player can print them with
//! `#popflags 1` … `#popflags 5`, one fixed line per flag
//! (`zone/gm_commands/popflags.cpp`). Every label and stage text below is
//! copied from that file; a reworded line there shows up here as an
//! unrecognised line rather than a wrong value.
//!
//! Lines are taken as pasted from chat or straight from an EverQuest log
//! (`[Mon Oct 05 20:00:00 2026] Fuirstel progression: …`). What a line cannot
//! pin down exactly is kept as a lower bound (`AtLeast`), so a later, more
//! precise line wins.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What the output says about one quest global.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", content = "v", rename_all = "snake_case")]
pub enum Known {
    /// The global is not set.
    Absent,
    /// Set, value not shown (an access line: "Unlocked").
    Present,
    /// The exact value ("3", or a bit string like "101").
    Exact(String),
    /// Set to at least this stage.
    AtLeast(u32),
}

impl Known {
    /// The lowest stage this is consistent with: 0 when absent.
    pub fn stage(&self) -> u32 {
        match self {
            Known::Absent => 0,
            Known::Present => 1,
            Known::AtLeast(n) => *n,
            Known::Exact(v) => v.parse().unwrap_or(1),
        }
    }

    fn merge(self, newer: Known) -> Known {
        match (self, newer) {
            (Known::Exact(v), Known::AtLeast(_) | Known::Present) => Known::Exact(v),
            (Known::AtLeast(a), Known::AtLeast(b)) => Known::AtLeast(a.max(b)),
            (Known::AtLeast(a), Known::Present) => Known::AtLeast(a),
            (_, newer) => newer,
        }
    }
}

/// The flags one paste established, keyed by quest global.
pub type Flags = BTreeMap<String, Known>;

/// What a paste yielded.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub flags: Flags,
    /// Tier sections seen (1–4, and 5 for the Plane of Time section).
    pub sections: Vec<u8>,
    /// Lines that looked like `#popflags` output but matched nothing here.
    pub unrecognised: Vec<String>,
}

const MAVUIN: &[&str] = &[
    "The evidence needed to save Mavuin has been requested",
    "The Tribunal has agreed to hear Mavuin's case",
    "Mavuin's case is complete",
];
const FUIRSTEL: &[&str] = &[
    "Obtain the Ward for Milyk",
    "The Ward was recovered",
    "Grummus was defeated",
    "Crypt of Decay access was granted",
    "Fuirstel progression complete",
];
const THELIN: &[&str] = &[
    "Help Thelin escape the hedge maze",
    "Defeat Terris Thule",
    "Terris Thule was defeated",
    "Thelin was released from Terris Thule",
    "Nightmare progression complete",
];
const AERINDAR: &[&str] = &[
    "Aerin`Dar defeated; the meaning of Justice remains",
    "Halls of Honor access unlocked",
];
const KARANA_T2: &[&str] = &[
    "Prove yourself to Askr",
    "Complete Mavuin's case, then use the Storms shrine to enter Bastion of Thunder",
    "Bastion of Thunder progression recorded",
    "Karana's information obtained",
];
const KARANA_T3: &[&str] = &[
    "Return to Askr",
    "Complete Mavuin's case, then use the Storms shrine to enter Bastion of Thunder",
    "Karana progression continues",
    "Karana's information obtained",
];
const TYLIS: &[&str] = &[
    "Rescue Tylis from the Plane of Torment",
    "Tylis progression complete",
];
const ZEKS: &[&str] = &[
    "Initial Zek progression recorded",
    "Meet Giwin in Drunder",
    "Vallon Zek's information obtained",
    "Tallon Zek's information obtained",
    "Both Zek information sets obtained",
    "Defeat Rallos Zek",
    "Zek progression complete",
];
const KARANA_DONE: &str = "Complete; combined into Zebuxoruk lore";

/// `Pending memory: <name>` → the checklist global behind it.
const PENDING: &[(&str, &str)] = &[
    ("Grummus", "cl_grummus"),
    ("Thelin's hedge maze", "cl_maze"),
    ("Terris Thule", "cl_terris"),
    ("Manaetic Behemoth", "cl_behemoth"),
    ("Aerin`Dar", "cl_aerindar"),
    ("Karana", "cl_karana"),
    ("Bertoxxulous", "cl_bertox"),
    ("Keeper of Sorrows", "cl_keeper"),
    ("Saryrn", "cl_saryrn"),
    ("Vallon Zek", "cl_vallon"),
    ("Tallon Zek", "cl_tallon"),
    ("Rallos Zek", "cl_rallos"),
    ("Solusek Ro", "cl_solusek"),
];

/// `<label>: Unlocked|Locked` lines that map to one global's presence.
const ACCESS: &[(&str, &str)] = &[
    ("Seventh Hammer access", "seventh"),
    ("Crypt of Decay access", "grummus"),
    ("Factory door access", "poi_door"),
    ("Lower Crypt access", "bertox_key"),
    ("Plane of Earth B access", "earthb_key"),
    ("Plane of Time access", "time"),
];

const HOH_BITS: &[&str] = &["Rydda`Dar trial", "Village trial", "Nomad trial"];
const SOL_BITS: &[&str] = &["Xuzl", "Arlyxir", "Dresolik", "Rizlona", "Jiva"];

/// Drop an EverQuest log timestamp (`[Mon Oct 05 20:00:00 2026] `).
fn strip_stamp(line: &str) -> &str {
    let t = line.trim();
    match (t.starts_with('['), t.find("] ")) {
        (true, Some(i)) => t[i + 2..].trim(),
        _ => t,
    }
}

fn stage_of(stages: &[&str], text: &str) -> Option<Known> {
    match text {
        "Not started" => Some(Known::Absent),
        "Progress recorded" => Some(Known::AtLeast(1)),
        _ => stages
            .iter()
            .position(|s| *s == text)
            .map(|i| Known::Exact((i + 1).to_string())),
    }
}

/// Read pasted `#popflags` output.
pub fn parse(text: &str) -> Parsed {
    let mut out = Parsed::default();
    let put = |flags: &mut Flags, name: &str, k: Known| {
        let merged = match flags.remove(name) {
            Some(old) => old.merge(k),
            None => k,
        };
        flags.insert(name.to_owned(), merged);
        Some(())
    };
    let mut hoh: Option<[bool; 3]> = None;
    let mut sol: Option<[bool; 5]> = None;
    let mut tier: u8 = 0;

    for raw in text.lines() {
        let line = strip_stamp(raw);
        if line.is_empty() {
            continue;
        }
        if let Some(n) = line
            .strip_prefix("=== Tier ")
            .and_then(|r| r.strip_suffix(" Progression ==="))
            .and_then(|n| n.parse::<u8>().ok())
        {
            tier = n;
            if !out.sections.contains(&n) {
                out.sections.push(n);
            }
            continue;
        }
        if line == "=== Plane of Time ===" {
            tier = 5;
            if !out.sections.contains(&5) {
                out.sections.push(5);
            }
            continue;
        }
        // Headings, notices and the overview carry no flag values.
        if line.starts_with("===")
            || line.starts_with("---")
            || line.starts_with("Tier ")
            || line.starts_with("Details: ")
            || line.starts_with("If one of these is missing")
            || line.starts_with("A checklist memory")
            || line.starts_with("Sit near Seer")
            || line.starts_with("Pending checklist memories")
            || line.starts_with("Complete the ")
        {
            continue;
        }
        if let Some(name) = line.strip_prefix("Pending memory: ") {
            match PENDING.iter().find(|(n, _)| *n == name) {
                Some((_, g)) => {
                    put(&mut out.flags, g, Known::Present);
                }
                None => out.unrecognised.push(line.to_owned()),
            }
            continue;
        }
        let Some((label, value)) = line.split_once(": ") else {
            continue; // ordinary chat in a log paste
        };
        let ok = match label {
            "Mavuin's case" => {
                stage_of(MAVUIN, value).and_then(|k| put(&mut out.flags, "mavuin", k))
            }
            "Fuirstel progression" => {
                stage_of(FUIRSTEL, value).and_then(|k| put(&mut out.flags, "fuirstel", k))
            }
            "Thelin progression" => {
                stage_of(THELIN, value).and_then(|k| put(&mut out.flags, "thelin", k))
            }
            "Giwin and Manaetic Behemoth progression" => {
                stage_of(&[], value).and_then(|k| put(&mut out.flags, "zeks", k))
            }
            "Aerin`Dar progression" => {
                stage_of(AERINDAR, value).and_then(|k| put(&mut out.flags, "aerindar", k))
            }
            "Askr and Karana progression" | "Agnarr and Karana progression" => {
                if value == KARANA_DONE {
                    put(&mut out.flags, "zebuxoruk", Known::AtLeast(1))
                } else {
                    let stages = if label.starts_with("Askr") {
                        KARANA_T2
                    } else {
                        KARANA_T3
                    };
                    stage_of(stages, value).and_then(|k| put(&mut out.flags, "karana", k))
                }
            }
            "Tylis progression" => {
                stage_of(TYLIS, value).and_then(|k| put(&mut out.flags, "tylis", k))
            }
            "Giwin and Zek progression" => {
                stage_of(ZEKS, value).and_then(|k| put(&mut out.flags, "zeks", k))
            }
            "Saryrn cipher half" | "Mithaniel Marr cipher half" => {
                let half = if label.starts_with("Saryrn") {
                    "saryrn"
                } else {
                    "mmarr"
                };
                match value {
                    "Combined into Cipher" => put(&mut out.flags, "cipher", Known::Present),
                    "Complete" => put(&mut out.flags, half, Known::Present),
                    "Incomplete" => {
                        put(&mut out.flags, half, Known::Absent);
                        put(&mut out.flags, "cipher", Known::Absent)
                    }
                    _ => None,
                }
            }
            "Halls of Honor trials" if value == "None completed" => {
                put(&mut out.flags, "hohtrials", Known::Absent)
            }
            "Tower wing flags" if value == "None completed" => {
                put(&mut out.flags, "sol_room", Known::Absent)
            }
            "Cipher information" => match value {
                "Received" => put(&mut out.flags, "cipher", Known::Present),
                "Missing" => put(&mut out.flags, "cipher", Known::Absent),
                _ => None,
            },
            "Zebuxoruk lore" => match value {
                "Received" => put(&mut out.flags, "zebuxoruk", Known::AtLeast(1)),
                "Missing" => put(&mut out.flags, "zebuxoruk", Known::Absent),
                _ => None,
            },
            "Combined Zek information" => match value {
                "Received" => put(&mut out.flags, "zeks", Known::AtLeast(5)),
                "Missing" => Some(()),
                _ => None,
            },
            "Final elemental information" | "Air, Earth, and Water access" => match value {
                "Received" | "Unlocked" => put(&mut out.flags, "zebuxoruk", Known::AtLeast(2)),
                "Missing" | "Locked" => Some(()),
                _ => None,
            },
            "Plane of Fire progression" => match value {
                "Unlocked" => put(&mut out.flags, "pofire", Known::AtLeast(2)),
                "In progress" => put(&mut out.flags, "pofire", Known::Exact("1".into())),
                "Not started" => put(&mut out.flags, "pofire", Known::Absent),
                _ => None,
            },
            "Plane of Fire access" => match value {
                "Unlocked" => put(&mut out.flags, "pofire", Known::AtLeast(2)),
                "Locked" => Some(()),
                _ => None,
            },
            _ => {
                if let Some((_, g)) = ACCESS.iter().find(|(l, _)| *l == label) {
                    match value {
                        "Unlocked" => put(&mut out.flags, g, Known::Present),
                        "Locked" => put(&mut out.flags, g, Known::Absent),
                        _ => None,
                    }
                } else if let Some(i) = HOH_BITS.iter().position(|l| *l == label) {
                    let bits = hoh.get_or_insert([false; 3]);
                    bits[i] = value == "Complete";
                    (value == "Complete" || value == "Incomplete").then_some(())
                } else if let Some(i) = SOL_BITS.iter().position(|l| *l == label) {
                    let bits = sol.get_or_insert([false; 5]);
                    bits[i] = value == "Complete";
                    (value == "Complete" || value == "Incomplete").then_some(())
                } else if tier > 0 && label.len() < 60 {
                    // Inside a #popflags section but not a line we know: the
                    // server's wording changed. Surface it.
                    None
                } else {
                    Some(()) // ordinary chat in a log paste
                }
            }
        };
        if ok.is_none() {
            out.unrecognised.push(line.to_owned());
        }
    }
    let bitstring = |bits: &[bool]| {
        bits.iter()
            .map(|b| if *b { '1' } else { '0' })
            .collect::<String>()
    };
    if let Some(b) = hoh {
        put(&mut out.flags, "hohtrials", Known::Exact(bitstring(&b)));
    }
    if let Some(b) = sol {
        put(&mut out.flags, "sol_room", Known::Exact(bitstring(&b)));
    }
    out.sections.sort_unstable();
    // A pending memory is printed only while it exists (and never once the
    // Plane of Time flag is set), so a pasted tier without the line means
    // the memory is not pending.
    let timeless = out.flags.get("time").is_some_and(|k| *k != Known::Absent);
    if !timeless {
        for (tier, globals) in PENDING_BY_TIER {
            if out.sections.contains(tier) {
                for g in *globals {
                    out.flags.entry((*g).to_owned()).or_insert(Known::Absent);
                }
            }
        }
    }
    out
}

/// Which `Pending memory:` lines each `#popflags` tier prints.
const PENDING_BY_TIER: &[(u8, &[&str])] = &[
    (1, &["cl_grummus", "cl_maze", "cl_terris", "cl_behemoth"]),
    (
        2,
        &[
            "cl_aerindar",
            "cl_karana",
            "cl_bertox",
            "cl_keeper",
            "cl_saryrn",
        ],
    ),
    (3, &["cl_vallon", "cl_tallon", "cl_rallos", "cl_solusek"]),
];

/// Is bit `i` of a bit-string global (`hohtrials`, `sol_room`) set?
pub fn bit(flags: &Flags, name: &str, i: usize) -> Option<bool> {
    match flags.get(name)? {
        Known::Absent => Some(false),
        Known::Exact(v) => Some(v.as_bytes().get(i) == Some(&b'1')),
        _ => None,
    }
}

/// Fold a newer paste into what was stored: a later paste of one tier must
/// not forget the tiers it did not include.
pub fn fold(stored: &mut Flags, newer: Flags) {
    for (k, v) in newer {
        let merged = match stored.remove(&k) {
            // A newer exact reading replaces an old one outright: flags can
            // be deleted (the cipher halves, karana) and must not stick.
            Some(old) if matches!(v, Known::AtLeast(_) | Known::Present) => old.merge(v),
            _ => v,
        };
        stored.insert(k, merged);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_tier_one_paste_with_log_stamps_reads_back_to_globals() {
        let paste = "\
[Mon Oct 05 20:00:00 2026] === Tier 1 Progression ===
[Mon Oct 05 20:00:00 2026] --- Plane of Justice ---
[Mon Oct 05 20:00:00 2026] Mavuin's case: The Tribunal has agreed to hear Mavuin's case
[Mon Oct 05 20:00:00 2026] Seventh Hammer access: Locked
[Mon Oct 05 20:00:00 2026] --- Plane of Disease ---
[Mon Oct 05 20:00:00 2026] Fuirstel progression: Grummus was defeated
[Mon Oct 05 20:00:00 2026] Crypt of Decay access: Unlocked
[Mon Oct 05 20:00:00 2026] Pending memory: Grummus
[Mon Oct 05 20:00:00 2026] --- Plane of Nightmare ---
[Mon Oct 05 20:00:00 2026] Thelin progression: Not started
[Mon Oct 05 20:00:00 2026] --- Plane of Innovation ---
[Mon Oct 05 20:00:00 2026] Factory door access: Unlocked
[Mon Oct 05 20:00:00 2026] Giwin and Manaetic Behemoth progression: Progress recorded
[Mon Oct 05 20:00:01 2026] Bubblie tells the guild, 'pulling'";
        let p = parse(paste);
        assert_eq!(p.sections, vec![1]);
        assert!(p.unrecognised.is_empty(), "{:?}", p.unrecognised);
        assert_eq!(p.flags["mavuin"], Known::Exact("2".into()));
        assert_eq!(p.flags["seventh"], Known::Absent);
        assert_eq!(p.flags["fuirstel"], Known::Exact("3".into()));
        assert_eq!(p.flags["grummus"], Known::Present);
        assert_eq!(p.flags["cl_grummus"], Known::Present);
        assert_eq!(p.flags["thelin"], Known::Absent);
        assert_eq!(p.flags["poi_door"], Known::Present);
        assert_eq!(p.flags["zeks"], Known::AtLeast(1));
        // Tier 1 was pasted without these memories, so they are not pending.
        assert_eq!(p.flags["cl_maze"], Known::Absent);
        assert!(!p.flags.contains_key("cl_bertox"));
    }

    #[test]
    fn tier_three_bits_and_the_exact_zek_stage_win_over_the_lower_bound() {
        let paste = "\
=== Tier 1 Progression ===
Giwin and Manaetic Behemoth progression: Progress recorded
=== Tier 3 Progression ===
Rydda`Dar trial: Complete
Village trial: Incomplete
Nomad trial: Complete
Mithaniel Marr cipher half: Complete
Agnarr and Karana progression: Karana progression continues
Giwin and Zek progression: Defeat Rallos Zek
Combined Zek information: Received
Xuzl: Complete
Arlyxir: Complete
Dresolik: Incomplete
Rizlona: Incomplete
Jiva: Complete
Plane of Fire progression: In progress";
        let p = parse(paste);
        assert!(p.unrecognised.is_empty(), "{:?}", p.unrecognised);
        assert_eq!(p.sections, vec![1, 3]);
        assert_eq!(p.flags["hohtrials"], Known::Exact("101".into()));
        assert_eq!(bit(&p.flags, "hohtrials", 1), Some(false));
        assert_eq!(p.flags["sol_room"], Known::Exact("11001".into()));
        assert_eq!(p.flags["zeks"], Known::Exact("6".into()));
        assert_eq!(p.flags["karana"], Known::Exact("3".into()));
        assert_eq!(p.flags["mmarr"], Known::Present);
        assert_eq!(p.flags["pofire"], Known::Exact("1".into()));
    }

    #[test]
    fn karana_folded_into_zebuxoruk_and_cipher_halves_read_as_combined() {
        let p = parse(
            "=== Tier 2 Progression ===\nAskr and Karana progression: Complete; combined into Zebuxoruk lore\nSaryrn cipher half: Combined into Cipher\n=== Tier 4 Progression ===\nAir, Earth, and Water access: Unlocked\nPlane of Time access: Locked",
        );
        assert!(p.unrecognised.is_empty(), "{:?}", p.unrecognised);
        assert_eq!(p.flags["zebuxoruk"], Known::AtLeast(2));
        assert_eq!(p.flags["cipher"], Known::Present);
        assert_eq!(p.flags["time"], Known::Absent);
    }

    #[test]
    fn a_reworded_server_line_is_reported_not_guessed() {
        let p = parse("=== Tier 1 Progression ===\nFuirstel progression: The Ward was polished");
        assert!(!p.flags.contains_key("fuirstel"));
        assert_eq!(
            p.unrecognised,
            vec!["Fuirstel progression: The Ward was polished".to_owned()]
        );
    }

    #[test]
    fn a_later_paste_keeps_other_tiers_and_replaces_deleted_flags() {
        let mut stored = parse("=== Tier 2 Progression ===\nSaryrn cipher half: Complete\nTylis progression: Tylis progression complete").flags;
        fold(
            &mut stored,
            parse("=== Tier 3 Progression ===\nCipher information: Received").flags,
        );
        assert_eq!(stored["tylis"], Known::Exact("2".into()));
        assert_eq!(stored["cipher"], Known::Present);
        assert_eq!(stored["saryrn"], Known::Present);
    }
}
