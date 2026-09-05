//! Can this character wear this item, and what would it replace?
//!
//! EQEmu's `items` row carries three bitmasks — `classes`, `races`, `slots`
//! — that the site has mirrored for every auctioned item and never read. The
//! class bits follow the client's class ids minus one (bit 0 = Warrior …
//! bit 14 = Beastlord); the race bits are the fourteen Velious-era playable
//! races in client order; the slot bits are the client's 22 equipment slots
//! in the order the inventory export names them, which is also the order
//! the character profile lists them in.
//!
//! Everything here is pure over the mirrored row, the roster record and the
//! character's last profile, so the picker's decisions are testable without
//! Discord or the network.

use crate::items::ItemSummary;
use crate::profiles::Profile;
use nocturnal_core::RosterCharacter;

/// Roster class names in class-id order (bit = id − 1).
const CLASS_BITS: [&str; 15] = [
    "Warrior",
    "Cleric",
    "Paladin",
    "Ranger",
    "Shadow Knight",
    "Druid",
    "Monk",
    "Bard",
    "Rogue",
    "Shaman",
    "Necromancer",
    "Wizard",
    "Magician",
    "Enchanter",
    "Beastlord",
];

/// Three-letter class tags as the item window prints them, same order.
const CLASS_TAGS: [&str; 15] = [
    "WAR", "CLR", "PAL", "RNG", "SHD", "DRU", "MNK", "BRD", "ROG", "SHM", "NEC", "WIZ", "MAG",
    "ENC", "BST",
];

/// Client race ids in `races`-bit order (bit 0 = Human … bit 14 = Froglok).
const RACE_IDS: [i64; 15] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 128, 130, 330];

const RACE_TAGS: [&str; 15] = [
    "HUM", "BAR", "ERU", "ELF", "HIE", "DEF", "HEF", "DWF", "TRL", "OGR", "HFL", "GNM", "IKS",
    "VAH", "FRG",
];

/// Equipment slot names in `slots`-bit order — the client's own order, which
/// is what a profile's `equipment` list uses.
pub const SLOT_NAMES: [&str; 22] = [
    "Charm",
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
    "Ring1",
    "Ring2",
    "Chest",
    "Legs",
    "Feet",
    "Waist",
    "Ammo",
];

/// Every class may use the item (the row says all fifteen bits, or none —
/// pqdi stores a few "ALL" items as 0).
fn all_classes(mask: i64) -> bool {
    mask == 0 || mask & 0x7fff == 0x7fff
}

fn all_races(mask: i64) -> bool {
    // 16383 = the fourteen races before Froglok; a row written after the
    // Froglok bit exists says 32767. Either is "ALL".
    mask == 0 || mask & 0x3fff == 0x3fff
}

/// Whether a class (roster name) may use the item.
pub fn class_can_use(item: &ItemSummary, class: &str) -> bool {
    if all_classes(item.classes) {
        return true;
    }
    CLASS_BITS
        .iter()
        .position(|c| c.eq_ignore_ascii_case(class))
        .is_some_and(|bit| item.classes & (1 << bit) != 0)
}

/// Whether a race (client race id) may use the item. An unknown race id —
/// no profile yet, or a race the table does not know — is not a refusal:
/// the class check is the one the roster can always answer.
pub fn race_can_use(item: &ItemSummary, race_id: Option<i64>) -> bool {
    if all_races(item.races) {
        return true;
    }
    let Some(id) = race_id else { return true };
    RACE_IDS
        .iter()
        .position(|r| *r == id)
        .map_or(true, |bit| item.races & (1 << bit) != 0)
}

/// Whether the item goes in an equipment slot at all. Only equipment is
/// gated by class and race; everything else is loot anyone may bid on.
pub fn is_equipment(item: &ItemSummary) -> bool {
    item.slots & ((1 << SLOT_NAMES.len()) - 1) != 0
}

/// A warning line per winner whose character cannot use the item, for
/// the officer confirming the auction. `classes` maps a lowercase
/// character name to its roster class. A winner without a character, or
/// with one the roster does not know, produces no line: there is nothing
/// to check against.
pub fn winner_warnings(
    item: &ItemSummary,
    winners: &[nocturnal_core::event::Winner],
    classes: &std::collections::HashMap<String, String>,
) -> Vec<String> {
    if !is_equipment(item) {
        return Vec::new();
    }
    winners
        .iter()
        .filter_map(|w| {
            let name = w.character.as_deref()?;
            let class = classes.get(&name.to_lowercase())?;
            (!class_can_use(item, class)).then(|| {
                format!(
                    "**{name}** ({class}) cannot use {} — Class: {}",
                    item.name,
                    class_line(item)
                )
            })
        })
        .collect()
}

/// "WAR CLR PAL" or "ALL", as the item window prints it.
pub fn class_line(item: &ItemSummary) -> String {
    if all_classes(item.classes) {
        return "ALL".to_owned();
    }
    CLASS_TAGS
        .iter()
        .enumerate()
        .filter(|(bit, _)| item.classes & (1 << bit) != 0)
        .map(|(_, t)| *t)
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn race_line(item: &ItemSummary) -> String {
    if all_races(item.races) {
        return "ALL".to_owned();
    }
    RACE_TAGS
        .iter()
        .enumerate()
        .filter(|(bit, _)| item.races & (1 << bit) != 0)
        .map(|(_, t)| *t)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The equipment slots the item can go in, by client name.
pub fn slots(item: &ItemSummary) -> Vec<&'static str> {
    SLOT_NAMES
        .iter()
        .enumerate()
        .filter(|(bit, _)| item.slots & (1 << bit) != 0)
        .map(|(_, s)| *s)
        .collect()
}

/// A character the picker may offer, with what the item would do for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub class: String,
    pub level: u8,
    /// One line, at most 100 characters: what the item replaces and the
    /// stat delta — or why there is no comparison.
    pub upgrade: String,
}

/// Why a roster character was left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excluded {
    pub name: String,
    pub class: String,
    pub reason: &'static str,
}

/// The picker's input: the item as mirrored (`None` when the mirror has no
/// row — TAKP items, or pqdi unreachable), the side's roster characters and
/// whatever profiles and worn-item rows the site snapshot holds.
pub struct Fit<'a> {
    pub item: Option<&'a ItemSummary>,
    pub profiles: &'a std::collections::BTreeMap<String, Profile>,
    pub gear: &'a std::collections::BTreeMap<String, ItemSummary>,
}

impl Fit<'_> {
    /// Split the side's characters into those the item fits and those it
    /// does not. With no item row, every character is a candidate and the
    /// upgrade line says so.
    pub fn split(&self, chars: &[&RosterCharacter]) -> (Vec<Candidate>, Vec<Excluded>) {
        let mut ok = Vec::new();
        let mut out = Vec::new();
        for c in chars {
            let profile = self.profiles.get(&c.name.to_lowercase());
            let Some(item) = self.item else {
                ok.push(Candidate {
                    name: c.name.clone(),
                    class: c.class.clone(),
                    level: c.level,
                    upgrade: "no item data to compare".to_owned(),
                });
                continue;
            };
            // Not equipment — a tradeskill drop, a quest piece, a spell:
            // the class and race masks on such rows mean nothing to a
            // bidder, so nobody is gated (Ziglax, 2026-09-05).
            if !is_equipment(item) {
                ok.push(Candidate {
                    name: c.name.clone(),
                    class: c.class.clone(),
                    level: c.level,
                    upgrade: "not equipment — no class or race limit".to_owned(),
                });
                continue;
            }
            if !class_can_use(item, &c.class) {
                out.push(Excluded {
                    name: c.name.clone(),
                    class: c.class.clone(),
                    reason: "class",
                });
                continue;
            }
            if !race_can_use(item, profile.map(|p| p.race)) {
                out.push(Excluded {
                    name: c.name.clone(),
                    class: c.class.clone(),
                    reason: "race",
                });
                continue;
            }
            ok.push(Candidate {
                name: c.name.clone(),
                class: c.class.clone(),
                level: c.level,
                upgrade: self.upgrade_line(item, profile),
            });
        }
        (ok, out)
    }

    /// What the item replaces for this character and by how much. For a
    /// paired slot (ears, wrists, rings) the comparison is against the
    /// weaker of the two worn items; an empty slot is a straight gain.
    fn upgrade_line(&self, item: &ItemSummary, profile: Option<&Profile>) -> String {
        let Some(profile) = profile else {
            return "no gear on record — run Zeal with the token set up".to_owned();
        };
        let fits = slots(item);
        if fits.is_empty() {
            return "not an equipment slot".to_owned();
        }
        // The weakest worn item among the slots this could go in.
        let mut worst: Option<(&str, Option<&ItemSummary>)> = None;
        for slot in &fits {
            let worn = profile
                .equipment
                .iter()
                .find(|s| s.slot == *slot)
                .and_then(|s| s.id)
                .and_then(|id| self.gear.get(&id.to_string()));
            let score = worn.map_or(i64::MIN, score);
            let replace = worst.map_or(true, |(_, w)| score < w.map_or(i64::MIN, self::score));
            if replace {
                worst = Some((slot, worn));
            }
        }
        let Some((slot, worn)) = worst else {
            return "no slot to compare".to_owned();
        };
        let Some(worn) = worn else {
            return clip(format!("{slot} is empty → {}", delta(item, None)));
        };
        clip(format!(
            "{}: {} → {}",
            slot,
            worn.name,
            delta(item, Some(worn))
        ))
    }
}

/// A single number to rank two worn items by: the sum of what the game
/// shows in the item window. Only used to pick which of a paired slot is
/// weaker; the member sees the full delta, not this.
fn score(i: &ItemSummary) -> i64 {
    i.ac * 2 + i.hp + i.mana + i.stats.iter().sum::<i64>() + i.resists.iter().sum::<i64>()
}

/// "AC +12 · HP +45 · MANA +10 · STA +5" — only what changes, weapons with
/// their ratio. `None` for the worn item means every stat is a gain.
fn delta(item: &ItemSummary, worn: Option<&ItemSummary>) -> String {
    let z = ItemSummary {
        id: 0,
        name: String::new(),
        icon: 0,
        ac: 0,
        hp: 0,
        mana: 0,
        stats: [0; 7],
        resists: [0; 5],
        slots: 0,
        classes: 0,
        races: 0,
        item_type: 0,
        req_level: 0,
        weight: 0,
        damage: 0,
        delay: 0,
        click: None,
        focus: None,
        worn: None,
        proc_effect: None,
        magic: false,
        lore: false,
        no_drop: false,
    };
    let w = worn.unwrap_or(&z);
    let mut parts: Vec<String> = Vec::new();
    let mut push = |label: &str, a: i64, b: i64| {
        if a != b {
            parts.push(format!("{label} {:+}", a - b));
        }
    };
    push("AC", item.ac, w.ac);
    push("HP", item.hp, w.hp);
    push("MANA", item.mana, w.mana);
    for (i, label) in ["STR", "STA", "AGI", "DEX", "WIS", "INT", "CHA"]
        .iter()
        .enumerate()
    {
        push(label, item.stats[i], w.stats[i]);
    }
    for (i, label) in ["MR", "FR", "CR", "DR", "PR"].iter().enumerate() {
        push(label, item.resists[i], w.resists[i]);
    }
    if item.damage > 0 && item.delay > 0 {
        let ratio = |i: &ItemSummary| {
            if i.delay > 0 {
                i.damage as f64 / i.delay as f64
            } else {
                0.0
            }
        };
        parts.insert(
            0,
            format!(
                "{}/{} ({:.2} vs {:.2})",
                item.damage,
                item.delay,
                ratio(item),
                ratio(w)
            ),
        );
    }
    if parts.is_empty() {
        "no stat change".to_owned()
    } else {
        parts.join(" · ")
    }
}

/// Discord's select-option description and modal placeholder both stop at
/// 100 characters; cut on a boundary rather than let the API refuse.
fn clip(s: String) -> String {
    const MAX: usize = 100;
    if s.chars().count() <= MAX {
        return s;
    }
    let mut out: String = s.chars().take(MAX - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::profiles::Slot;

    fn tome() -> ItemSummary {
        let row: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/pqdi_item_26780.json")).unwrap();
        ItemSummary::from_row(&row)
    }

    fn tunic() -> ItemSummary {
        let row: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/pqdi_item_30563.json")).unwrap();
        ItemSummary::from_row(&row)
    }

    fn toon(name: &str, class: &str) -> RosterCharacter {
        RosterCharacter {
            name: name.into(),
            class: class.into(),
            level: 60,
            aa: None,
            profile_url: None,
            access: vec![],
            main: None,
        }
    }

    fn profile(name: &str, race: i64, secondary: Option<i64>) -> Profile {
        Profile {
            name: name.into(),
            level: 60,
            class: 12,
            race,
            deity: 0,
            guild: String::new(),
            base_stats: Default::default(),
            sheet: Default::default(),
            aa: Default::default(),
            equipment: vec![Slot {
                slot: "Secondary".into(),
                id: secondary,
                name: None,
            }],
            aa_abilities: vec![],
            reported_ms: 1,
            reporter: None,
        }
    }

    /// The masks read as the item window prints them — the fixture is a
    /// real auctioned item, a caster secondary.
    #[test]
    fn the_tome_is_a_caster_secondary_for_all_races() {
        let t = tome();
        assert_eq!(class_line(&t), "NEC WIZ MAG ENC");
        assert_eq!(race_line(&t), "ALL");
        assert_eq!(slots(&t), vec!["Secondary"]);
        assert!(class_can_use(&t, "Wizard"));
        assert!(class_can_use(&t, "enchanter"), "case-insensitive");
        assert!(!class_can_use(&t, "Warrior"));
        let u = tunic();
        assert_eq!(class_line(&u), "DRU MNK BST");
        assert_eq!(slots(&u), vec!["Chest"]);
    }

    #[test]
    fn race_gates_only_when_the_row_and_the_profile_both_say() {
        let mut t = tome();
        assert!(race_can_use(&t, Some(9)), "ALL races");
        t.races = 1 << 2; // Erudite only
        assert!(race_can_use(&t, Some(3)));
        assert!(!race_can_use(&t, Some(1)), "a human");
        assert!(
            race_can_use(&t, None),
            "no profile: the class check decides"
        );
    }

    #[test]
    fn the_picker_splits_the_side_by_class_and_says_why() {
        let profiles = Default::default();
        let gear = Default::default();
        let fit = Fit {
            item: Some(&tome()),
            profiles: &profiles,
            gear: &gear,
        };
        let vex = toon("Vexira", "Wizard");
        let thu = toon("Thurgo", "Warrior");
        let (ok, out) = fit.split(&[&vex, &thu]);
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].name, "Vexira");
        assert_eq!(
            ok[0].upgrade,
            "no gear on record — run Zeal with the token set up"
        );
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].name.as_str(), out[0].reason), ("Thurgo", "class"));
    }

    /// A tradeskill or quest drop has no slot; its class bits are noise.
    #[test]
    fn non_equipment_gates_nobody() {
        let mut ore = tome();
        ore.slots = 0;
        ore.classes = 1 << 11; // a "WIZ only" mask on a thing nobody wears
        assert!(!is_equipment(&ore));
        let profiles = Default::default();
        let gear = Default::default();
        let fit = Fit {
            item: Some(&ore),
            profiles: &profiles,
            gear: &gear,
        };
        let thu = toon("Thurgo", "Warrior");
        let (ok, out) = fit.split(&[&thu]);
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].upgrade, "not equipment — no class or race limit");
        assert!(out.is_empty());
        assert!(winner_warnings(&ore, &[], &Default::default()).is_empty());
    }

    /// The officer's safety net at close: a winner whose character cannot
    /// use the item gets a line; no character, or no roster class, none.
    #[test]
    fn a_winner_who_cannot_use_the_item_is_flagged() {
        use nocturnal_core::event::Winner;
        let win = |c: Option<&str>| Winner {
            player: 7,
            amount: 5,
            for_main: true,
            character: c.map(str::to_owned),
        };
        let mut classes = std::collections::HashMap::new();
        classes.insert("thurgo".to_owned(), "Warrior".to_owned());
        classes.insert("vexira".to_owned(), "Wizard".to_owned());
        let lines = winner_warnings(
            &tome(),
            &[
                win(Some("Thurgo")),
                win(Some("Vexira")),
                win(None),
                win(Some("Ghost")),
            ],
            &classes,
        );
        assert_eq!(
            lines,
            vec!["**Thurgo** (Warrior) cannot use Tome of Secrets — Class: NEC WIZ MAG ENC"]
        );
    }

    #[test]
    fn without_an_item_row_everyone_is_offered() {
        let profiles = Default::default();
        let gear = Default::default();
        let fit = Fit {
            item: None,
            profiles: &profiles,
            gear: &gear,
        };
        let thu = toon("Thurgo", "Warrior");
        let (ok, out) = fit.split(&[&thu]);
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].upgrade, "no item data to compare");
        assert!(out.is_empty());
    }

    /// The delta is against what is worn in the slot the item fits — here
    /// a weaker secondary — and an empty slot is a straight gain.
    #[test]
    fn the_upgrade_line_names_the_worn_item_and_the_delta() {
        let mut orb = tome();
        orb.id = 1;
        orb.name = "Orb of Mastery".into();
        orb.ac = 10;
        orb.hp = 0;
        orb.mana = 120;
        orb.stats = [0; 7];
        orb.resists = [0; 5];
        let mut profiles = std::collections::BTreeMap::new();
        profiles.insert("vexira".to_owned(), profile("Vexira", 3, Some(1)));
        profiles.insert("solenne".to_owned(), profile("Solenne", 5, None));
        let mut gear = std::collections::BTreeMap::new();
        gear.insert("1".to_owned(), orb);
        let fit = Fit {
            item: Some(&tome()),
            profiles: &profiles,
            gear: &gear,
        };
        let vex = toon("Vexira", "Wizard");
        let sol = toon("Solenne", "Enchanter");
        let (ok, _) = fit.split(&[&vex, &sol]);
        assert!(
            ok[0]
                .upgrade
                .starts_with("Secondary: Orb of Mastery → AC +50 · HP +120 · STA +10 · AGI +10"),
            "{}",
            ok[0].upgrade
        );
        assert!(
            ok[0].upgrade.ends_with('…') && ok[0].upgrade.chars().count() == 100,
            "clipped to Discord's limit: {}",
            ok[0].upgrade
        );
        assert!(
            ok[1]
                .upgrade
                .starts_with("Secondary is empty → AC +60 · HP +120 · MANA +120"),
            "{}",
            ok[1].upgrade
        );
    }

    #[test]
    fn a_weapon_leads_with_its_ratio() {
        let mut sword = tome();
        sword.damage = 25;
        sword.delay = 30;
        sword.slots = 1 << 13; // Primary
        let s = delta(&sword, None);
        assert!(s.starts_with("25/30 (0.83 vs 0.00)"), "{s}");
    }
}
