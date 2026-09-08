//! Zeal's `/outputfile` exports, read the way `/magelo` would have said it.
//!
//! A member on any Zeal build can type `/outputfile quarmy` and get
//! `<Name>Quarmy.txt`: a character line (name, level, class, race, deity,
//! guild, base stats), the inventory in `Location / Name / ID / Count /
//! Slots` columns, trained AA as index and rank, and skills. `/outputfile
//! inventory` writes the inventory section alone, and Zeal writes that one
//! on every camp when its export-on-camp setting is on. Both formats are
//! read from the client source (`Zeal/outputfile.cpp`, `IDToEquipSlot`) and
//! pinned by fixtures cut from real exports.
//!
//! What comes out is the JSON body a client's `everquest.character.profile`
//! event carries, so the site, the upgrade line and everything else read
//! one shape. Bags, bank and coin are in the file and never leave this
//! module: nobody needs the guild to hold their bank.
//!
//! Slots are read **by position**, not by label. The old label format
//! repeats `Ear`, `Wrist` and `Fingers` for the paired slots; the new one
//! numbers them. The 21 equipment rows always come out in the client's slot
//! order, so the label is a check and the position is the key, and the
//! profile names slots the way the client's own event does (`Ear1`,
//! `Wrist2`, `Finger1`).

use nocturnal_core::ProfileSource;

use crate::profiles::Slot;

/// The 21 equipment slots in the client's order, named as the profile
/// event names them (Zeal's new label format).
pub const SLOTS: [&str; 21] = [
    "Ear1",
    "Head",
    "Face",
    "Ear2",
    "Neck",
    "Shoulders",
    "Arms",
    "Back",
    "Wrist1",
    "Wrist2",
    "Range",
    "Hands",
    "Primary",
    "Secondary",
    "Finger1",
    "Finger2",
    "Chest",
    "Legs",
    "Feet",
    "Waist",
    "Ammo",
];

/// The old label for each position, where it differs from [`SLOTS`].
fn old_label(i: usize) -> &'static str {
    match SLOTS[i] {
        "Ear1" | "Ear2" => "Ear",
        "Wrist1" | "Wrist2" => "Wrist",
        "Finger1" | "Finger2" => "Fingers",
        s => s,
    }
}

/// What a file said. `name` and the character fields are `None` for an
/// inventory export, which has no character line.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub source: ProfileSource,
    pub name: Option<String>,
    pub level: Option<i64>,
    pub class: Option<i64>,
    pub race: Option<i64>,
    pub gender: Option<i64>,
    pub deity: Option<i64>,
    pub guild: Option<String>,
    /// str, sta, cha, dex, int, agi, wis - the client's own keys.
    pub base_stats: Vec<(&'static str, i64)>,
    pub equipment: Vec<Slot>,
    /// Item stack counts by position, for the body's `count`.
    counts: Vec<Option<i64>>,
    /// Trained abilities as `[client_index, rank]`, the numbers the file has.
    pub aa_abilities: Vec<(u16, u8)>,
}

/// Why a file could not be read as a profile, in words for the member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn err<T>(msg: impl Into<String>) -> Result<T, ParseError> {
    Err(ParseError(msg.into()))
}

/// Read a `/outputfile quarmy` or `/outputfile inventory` export.
pub fn parse(text: &str) -> Result<Parsed, ParseError> {
    let lines: Vec<Vec<&str>> = text
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split('\t').collect())
        .collect();
    if lines.is_empty() {
        return err("the file is empty");
    }

    let mut p = Parsed {
        source: ProfileSource::InventoryFile,
        name: None,
        level: None,
        class: None,
        race: None,
        gender: None,
        deity: None,
        guild: None,
        base_stats: Vec::new(),
        equipment: Vec::new(),
        counts: Vec::new(),
        aa_abilities: Vec::new(),
    };

    let mut i = 0;
    // Section 1 (quarmy only): the character line under its header.
    if lines[0].first() == Some(&"Character") && lines[0].get(1) == Some(&"Name") {
        let Some(row) = lines.get(1).filter(|r| r.first() == Some(&"Character")) else {
            return err("the character line is missing under its header");
        };
        // Character, Name, LastName, Level, Class, Race, Gender, Deity, Guild, GuildRank,
        // BaseSTR, BaseSTA, BaseCHA, BaseDEX, BaseINT, BaseAGI, BaseWIS
        if row.len() < 17 {
            return err(format!(
                "the character line has {} columns, not 17",
                row.len()
            ));
        }
        let num = |col: usize, what: &str| -> Result<i64, ParseError> {
            row[col]
                .trim()
                .parse::<i64>()
                .map_err(|_| ParseError(format!("{what} is not a number: {:?}", row[col])))
        };
        let name = row[1].trim();
        if name.is_empty() {
            return err("the character line has no name");
        }
        p.source = ProfileSource::QuarmyFile;
        p.name = Some(name.to_owned());
        p.level = Some(num(3, "level")?);
        p.class = Some(num(4, "class")?);
        p.race = Some(num(5, "race")?);
        p.gender = Some(num(6, "gender")?);
        p.deity = Some(num(7, "deity")?);
        p.guild = Some(row[8].trim().to_owned()).filter(|g| g != "None");
        for (key, col) in [
            ("str", 10),
            ("sta", 11),
            ("cha", 12),
            ("dex", 13),
            ("int", 14),
            ("agi", 15),
            ("wis", 16),
        ] {
            p.base_stats.push((key, num(col, key)?));
        }
        i = 2;
    }

    // Section 2: the inventory header, then 21 equipment rows in slot order.
    match lines.get(i) {
        Some(h) if h.first() == Some(&"Location") && h.get(1) == Some(&"Name") => i += 1,
        _ => return err("no inventory section (a `Location  Name  ID  Count  Slots` line)"),
    }
    for (pos, want) in SLOTS.iter().enumerate() {
        let Some(row) = lines.get(i) else {
            return err(format!("the file ends before slot {want}"));
        };
        let label = row.first().copied().unwrap_or("");
        if label != *want && label != old_label(pos) {
            return err(format!(
                "expected the {want} slot at row {} and found {label:?}",
                i + 1
            ));
        }
        if row.len() < 3 {
            return err(format!("the {want} row has no item id"));
        }
        let name = row[1].trim();
        let id = row[2].trim().parse::<i64>().unwrap_or(0);
        let count = row.get(3).and_then(|c| c.trim().parse::<i64>().ok());
        if name == "Empty" || id <= 0 {
            p.equipment.push(Slot {
                slot: (*want).to_owned(),
                id: None,
                name: None,
            });
            p.counts.push(None);
        } else {
            p.equipment.push(Slot {
                slot: (*want).to_owned(),
                id: Some(id),
                name: Some(name.to_owned()),
            });
            p.counts.push(count);
        }
        i += 1;
    }

    // Bags, bank and coin: skipped. Section 3 (quarmy only): AA purchases.
    while let Some(row) = lines.get(i) {
        if row.first() == Some(&"AAIndex") {
            i += 1;
            while let Some(row) = lines.get(i) {
                if row.first() == Some(&"SkillID") {
                    break;
                }
                if let (Some(idx), Some(rank)) = (
                    row.first().and_then(|v| v.trim().parse::<u16>().ok()),
                    row.get(1).and_then(|v| v.trim().parse::<u8>().ok()),
                ) {
                    if rank > 0 {
                        p.aa_abilities.push((idx, rank));
                    }
                }
                i += 1;
            }
            break;
        }
        i += 1;
    }
    Ok(p)
}

/// The character the file is for, from the file or from the file's name
/// (`Ziglax-Inventory.txt`, `ZiglaxQuarmy.txt`).
pub fn character_name(parsed: &Parsed, filename: &str) -> Option<String> {
    if let Some(n) = &parsed.name {
        return Some(n.clone());
    }
    let stem = filename.rsplit('/').next().unwrap_or(filename);
    let stem = stem.strip_suffix(".txt").unwrap_or(stem);
    let name: String = stem
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    let name = name
        .strip_suffix("Quarmy")
        .or_else(|| name.strip_suffix("Inventory"))
        .unwrap_or(&name);
    (name.len() >= 3).then(|| name.to_owned())
}

/// Fields an inventory export does not carry, taken from the roster row.
#[derive(Debug, Clone, Copy, Default)]
pub struct Fallback<'a> {
    pub level: i64,
    pub class: i64,
    pub race: Option<i64>,
    pub guild: &'a str,
}

/// The profile event body, as `Zeal/otlp_exporter.cpp` builds it, plus
/// `source` and `reason: "upload"` so a reader can tell a file from a
/// client. Numbers the file does not have stay absent, never zero.
pub fn body(parsed: &Parsed, name: &str, fallback: Fallback<'_>) -> serde_json::Value {
    let mut j = serde_json::json!({
        "name": name,
        "level": parsed.level.unwrap_or(fallback.level),
        "class": parsed.class.unwrap_or(fallback.class),
        "race": parsed.race.or(fallback.race).unwrap_or(0),
        "guild": parsed.guild.clone().unwrap_or_else(|| fallback.guild.to_owned()),
        "source": parsed.source.as_str(),
        "reason": "upload",
    });
    if let Some(g) = parsed.gender {
        j["gender"] = g.into();
    }
    if let Some(d) = parsed.deity {
        j["deity"] = d.into();
    }
    if !parsed.base_stats.is_empty() {
        j["base_stats"] = parsed
            .base_stats
            .iter()
            .map(|(k, v)| ((*k).to_owned(), serde_json::Value::from(*v)))
            .collect::<serde_json::Map<_, _>>()
            .into();
    }
    if parsed.source == ProfileSource::QuarmyFile {
        let spent: i64 = parsed.aa_abilities.iter().map(|(_, r)| i64::from(*r)).sum();
        j["aa"] = serde_json::json!({ "spent": spent });
        j["aa_abilities"] = parsed
            .aa_abilities
            .iter()
            .map(|(i, r)| serde_json::json!([i, r]))
            .collect::<Vec<_>>()
            .into();
    }
    j["equipment"] = parsed
        .equipment
        .iter()
        .zip(parsed.counts.iter())
        .map(|(s, count)| {
            let mut slot = serde_json::json!({ "slot": s.slot });
            if let (Some(id), Some(name)) = (s.id, &s.name) {
                slot["id"] = id.into();
                slot["name"] = name.clone().into();
                if let Some(c) = count {
                    slot["count"] = (*c).into();
                }
            }
            slot
        })
        .collect::<Vec<_>>()
        .into();
    j
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const QUARMY_OLD: &str = include_str!("../tests/fixtures/quarmy_old_labels.txt");
    const QUARMY_NEW: &str = include_str!("../tests/fixtures/quarmy_new_labels.txt");
    const INVENTORY: &str = include_str!("../tests/fixtures/inventory_old_labels.txt");

    #[test]
    fn a_quarmy_export_is_a_whole_character() {
        let p = parse(QUARMY_OLD).unwrap();
        assert_eq!(p.source, ProfileSource::QuarmyFile);
        assert_eq!(p.name.as_deref(), Some("Ziglax"));
        assert_eq!(
            (p.level, p.class, p.race, p.deity),
            (Some(60), Some(14), Some(12), Some(396))
        );
        assert_eq!(p.guild.as_deref(), Some("Nocturnal"));
        assert_eq!(p.base_stats[0], ("str", 60));
        assert_eq!(p.base_stats[6], ("wis", 67));
        assert_eq!(p.equipment.len(), 21);
        assert_eq!(p.aa_abilities, vec![(211, 3), (37, 2)]);
    }

    #[test]
    fn paired_slots_are_read_by_position_whatever_the_label() {
        let old = parse(QUARMY_OLD).unwrap();
        let new = parse(QUARMY_NEW).unwrap();
        assert_eq!(old.equipment, new.equipment, "same gear, two label formats");
        let by_slot: std::collections::HashMap<_, _> = old
            .equipment
            .iter()
            .map(|s| (s.slot.as_str(), s.id))
            .collect();
        assert_eq!(by_slot["Ear1"], Some(27939), "the first Ear line");
        assert_eq!(by_slot["Ear2"], Some(10912), "the second Ear line");
        assert_eq!(by_slot["Wrist1"], Some(12806));
        assert_eq!(by_slot["Wrist2"], Some(5723));
        assert_eq!(by_slot["Finger1"], Some(10366));
        assert_eq!(by_slot["Finger2"], Some(25198));
        assert_eq!(by_slot["Ammo"], None, "Empty is an empty slot");
    }

    #[test]
    fn bags_bank_and_coin_never_leave_the_parser() {
        let p = parse(QUARMY_OLD).unwrap();
        let b = body(&p, "Ziglax", Fallback::default()).to_string();
        assert!(!b.contains("Backpack"), "a bag");
        assert!(!b.contains("Ant's Potion"), "a bag's contents");
        assert!(!b.contains("Currency"), "coin");
        assert!(!b.contains("Bank"), "the bank");
        assert!(!b.contains("SkillID"), "skills");
    }

    #[test]
    fn the_body_is_what_a_client_would_have_sent() {
        let p = parse(QUARMY_OLD).unwrap();
        let b = body(&p, "Ziglax", Fallback::default());
        assert_eq!(b["name"], "Ziglax");
        assert_eq!(b["level"], 60);
        assert_eq!(b["class"], 14);
        assert_eq!(b["base_stats"]["int"], 113);
        assert_eq!(b["aa"]["spent"], 5);
        assert_eq!(b["aa_abilities"][0], serde_json::json!([211, 3]));
        assert_eq!(b["source"], "quarmy_file");
        assert_eq!(b["reason"], "upload");
        assert_eq!(b["equipment"][1]["slot"], "Head");
        assert_eq!(b["equipment"][1]["id"], 1867);
        assert_eq!(b["equipment"][20]["slot"], "Ammo");
        assert!(
            b["equipment"][20].get("id").is_none(),
            "an empty slot has no id"
        );
        assert!(
            b.get("sheet").is_none(),
            "no AC/HP/attack: absent, not zero"
        );
        // And the site's own parser takes it as a profile.
        let profile = crate::profiles::from_body_text(&b.to_string(), 1, None).unwrap();
        assert_eq!(profile.equipment.len(), 21);
        assert_eq!(profile.source.as_deref(), Some("quarmy_file"));
        assert_eq!(profile.aa.get("spent"), Some(&5));
    }

    #[test]
    fn an_inventory_export_is_gear_only_and_borrows_the_rest() {
        let p = parse(INVENTORY).unwrap();
        assert_eq!(p.source, ProfileSource::InventoryFile);
        assert_eq!(p.name, None);
        assert_eq!(p.equipment.len(), 21);
        assert!(p.aa_abilities.is_empty());
        assert_eq!(
            character_name(&p, "Ziglax-Inventory.txt").as_deref(),
            Some("Ziglax")
        );
        assert_eq!(
            character_name(&p, "ZiglaxQuarmy.txt").as_deref(),
            Some("Ziglax")
        );
        assert_eq!(character_name(&p, "x.txt"), None);
        let b = body(
            &p,
            "Ziglax",
            Fallback {
                level: 60,
                class: 14,
                race: None,
                guild: "Nocturnal",
            },
        );
        assert_eq!(b["level"], 60);
        assert_eq!(b["race"], 0, "unknown race: the item check treats 0 as any");
        assert!(b.get("aa").is_none());
        assert_eq!(b["source"], "inventory_file");
    }

    #[test]
    fn a_file_that_is_not_an_export_says_why() {
        assert_eq!(parse("").unwrap_err().0, "the file is empty");
        assert!(parse("hello\tworld\n")
            .unwrap_err()
            .0
            .contains("no inventory section"));
        let truncated: String = QUARMY_OLD.lines().take(10).collect::<Vec<_>>().join("\n");
        assert!(parse(&truncated)
            .unwrap_err()
            .0
            .contains("ends before slot"));
        let wrong = QUARMY_OLD.replacen("Head\tCirclet", "Hat\tCirclet", 1);
        assert!(parse(&wrong)
            .unwrap_err()
            .0
            .contains("expected the Head slot"));
    }
}
