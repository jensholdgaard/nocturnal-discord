//! RaidHelper integration — port of the legacy `raidHelperUtils.js`.
//!
//! Two touch points, both optional and both keyed on the guild's API key:
//! `/startraid` names and links a raid from an event starting within ±10
//! minutes, and ending a linked raid awards DKP to members who both signed up
//! and actually attended. Every outbound call has a timeout (audit #42's
//! lesson applied to a second scraper-ish dependency).

use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize;

use nocturnal_core::state::Raid;
use nocturnal_core::PlayerId;

const API: &str = "https://raid-helper.dev/api/v4";

#[derive(Debug, Deserialize)]
pub struct Event {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(rename = "startTime", default)]
    pub start_time: i64,
    #[serde(rename = "signUps", default)]
    pub signups: Vec<SignUp>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SignUp {
    #[serde(rename = "userId", default)]
    pub user_id: String,
    #[serde(rename = "className", default)]
    pub class_name: String,
}

/// Statuses that mean "not raiding", exactly as the legacy list.
const IGNORED_STATUSES: [&str; 2] = ["Absence", "Bench"];

fn client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .user_agent("nocturnal-dkp-bot")
        .build()?)
}

/// An event starting within ±10 minutes of now, used to auto-name a raid.
pub async fn event_starting_now(
    api_key: &str,
    guild: u64,
    now_ms: i64,
) -> anyhow::Result<Option<Event>> {
    #[derive(Deserialize)]
    struct Listing {
        #[serde(rename = "postedEvents", default)]
        posted_events: Vec<Event>,
    }
    let window_ms = 10 * 60 * 1000;
    let resp = client()?
        .get(format!("{API}/servers/{guild}/events"))
        .header("Authorization", api_key)
        .header("StartTimeFilter", ((now_ms - window_ms) / 1000).to_string())
        .header("EndTimeFilter", ((now_ms + window_ms) / 1000).to_string())
        .send()
        .await
        .context("raid-helper event listing")?;
    if !resp.status().is_success() {
        anyhow::bail!("raid-helper returned {}", resp.status());
    }
    let listing: Listing = resp.json().await.context("raid-helper listing json")?;
    Ok(listing.posted_events.into_iter().find(|e| {
        let start = e.start_time * 1000;
        start > now_ms - window_ms && start < now_ms + window_ms
    }))
}

pub async fn fetch_event(event_id: &str) -> anyhow::Result<Event> {
    let resp = client()?
        .get(format!("{API}/events/{event_id}"))
        .send()
        .await
        .context("raid-helper event")?;
    if !resp.status().is_success() {
        anyhow::bail!("Failed to fetch RaidHelper event: {}", resp.status());
    }
    resp.json().await.context("raid-helper event json")
}

/// Who gets the event DKP, and who narrowly misses — the legacy rule: a
/// signup that isn't Absence/Bench and attended at least
/// `min(10, max(1, distinct_attendees / 2))` ticks.
#[derive(Debug, Default, PartialEq)]
pub struct Award {
    pub rewarded: Vec<PlayerId>,
    pub not_enough_attendance: Vec<(PlayerId, u32)>,
    pub signed_up_absent: Vec<PlayerId>,
    pub attended_unsigned: Vec<PlayerId>,
    pub required: u32,
}

pub fn decide_award(raid: &Raid, signups: &[SignUp]) -> Award {
    let mut attended: std::collections::BTreeMap<PlayerId, u32> = Default::default();
    for entry in &raid.entries {
        for player in &entry.players {
            *attended.entry(*player).or_default() += 1;
        }
    }
    let half = (attended.len() / 2).max(1) as u32;
    let required = half.min(10);

    let eligible: Vec<&SignUp> = signups
        .iter()
        .filter(|s| !IGNORED_STATUSES.contains(&s.class_name.as_str()))
        .collect();
    let signed: std::collections::BTreeSet<PlayerId> = eligible
        .iter()
        .filter_map(|s| s.user_id.parse().ok())
        .collect();

    let mut award = Award {
        required,
        ..Award::default()
    };
    for player in &signed {
        match attended.get(player) {
            Some(&count) if count >= required => award.rewarded.push(*player),
            Some(&count) => award.not_enough_attendance.push((*player, count)),
            None => award.signed_up_absent.push(*player),
        }
    }
    for (player, count) in &attended {
        if !signed.contains(player) && *count >= required {
            award.attended_unsigned.push(*player);
        }
    }
    award
}

#[cfg(test)]
mod tests {
    use super::{decide_award, SignUp};
    use nocturnal_core::state::{AttendanceEntry, Raid};

    fn raid(entries: Vec<Vec<u64>>) -> Raid {
        Raid {
            name: "R".into(),
            date_ms: 0,
            tick_interval_ms: 1,
            dkp_per_tick: 1,
            active: false,
            tick_no: entries.len() as u32,
            event_id: None,
            entries: entries
                .into_iter()
                .map(|players| AttendanceEntry {
                    players,
                    comment: "Tick".into(),
                    ts_ms: 0,
                    amount: 1,
                })
                .collect(),
        }
    }

    fn signup(id: u64, class_name: &str) -> SignUp {
        SignUp {
            user_id: id.to_string(),
            class_name: class_name.to_owned(),
        }
    }

    /// The legacy award rule, case by case.
    #[test]
    fn award_follows_the_legacy_rule() {
        // Four distinct attendees → required = min(10, max(1, 4/2)) = 2.
        let raid = raid(vec![vec![1, 2, 3, 4], vec![1, 2, 3], vec![1, 2], vec![1]]);
        let signups = vec![
            signup(1, "Warrior"), // signed, 4 ticks → rewarded
            signup(2, "Cleric"),  // signed, 3 ticks → rewarded
            signup(4, "Rogue"),   // signed, 1 tick  → short of 2
            signup(5, "Mage"),    // signed, absent
            signup(6, "Absence"), // ignored status
            signup(7, "Bench"),   // ignored status
        ];
        let award = decide_award(&raid, &signups);
        assert_eq!(award.required, 2);
        assert_eq!(award.rewarded, vec![1, 2]);
        assert_eq!(award.not_enough_attendance, vec![(4, 1)]);
        assert_eq!(award.signed_up_absent, vec![5]);
        // 3 attended twice but never signed up.
        assert_eq!(award.attended_unsigned, vec![3]);
    }

    /// A tiny raid still requires at least one tick, and the cap is 10.
    #[test]
    fn required_attendance_is_clamped() {
        let one = raid(vec![vec![1]]);
        assert_eq!(decide_award(&one, &[signup(1, "Warrior")]).required, 1);

        let big: Vec<Vec<u64>> = (0..30).map(|_| (1..=40).collect()).collect();
        assert_eq!(
            decide_award(&raid(big), &[signup(1, "Warrior")]).required,
            10
        );
    }
}
