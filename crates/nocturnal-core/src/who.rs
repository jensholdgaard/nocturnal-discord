//! EQ `/who` log parsing (`/parsedkps`) — port of the legacy `logParser`.
//!
//! Every word following a `]` is a candidate character name; the literals
//! `Players` ("Players on EverQuest:") and `There` ("There are N players…")
//! are filtered out, exactly like the legacy regex `/] (\w+)/g`.

/// Result of parsing a pasted `/who` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhoParse {
    pub characters: Vec<String>,
    /// Timestamp from the first `[…]` header, as unix ms (UTC), if parseable.
    pub ts_ms: Option<i64>,
}

pub fn parse_who(log: &str) -> WhoParse {
    let mut characters = Vec::new();
    let bytes = log.as_bytes();
    let mut i = 0;
    while let Some(off) = log[i..].find("] ") {
        let start = i + off + 2;
        let mut end = start;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
        if end > start {
            let word = &log[start..end];
            if word != "Players" && word != "There" {
                characters.push(word.to_owned());
            }
        }
        i = start;
    }
    WhoParse {
        characters,
        ts_ms: parse_header_ts(log),
    }
}

/// "[Sun Nov 19 09:52:52 2023]" → unix ms, interpreted as UTC.
fn parse_header_ts(log: &str) -> Option<i64> {
    let inner = log.strip_prefix('[')?.split(']').next()?;
    let parts: Vec<&str> = inner.split_whitespace().collect();
    let [_wday, mon, day, time, year] = parts[..] else {
        return None;
    };
    let month = match mon {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let day: i64 = day.parse().ok()?;
    let year: i64 = year.parse().ok()?;
    let [h, m, s] = time.split(':').collect::<Vec<_>>()[..] else {
        return None;
    };
    let (h, m, s): (i64, i64, i64) = (h.parse().ok()?, m.parse().ok()?, s.parse().ok()?);
    Some((days_from_civil(year, month, day) * 86_400 + h * 3600 + m * 60 + s) * 1000)
}

/// Hinnant's days_from_civil.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::parse_who;

    /// The legacy `logParser.spec.js` fixture, verbatim.
    #[test]
    fn parses_the_legacy_fixture() {
        let log = "[Sun Nov 19 09:52:52 2023] Players on EverQuest:\n\
            [Sun Nov 19 09:52:52 2023] ---------------------------\n\
            [Sun Nov 19 09:52:52 2023] [ANONYMOUS] Arbusto\n\
            [Sun Nov 19 09:52:52 2023] [ANONYMOUS] Julia  <Alianza>\n\
            [Sun Nov 19 09:52:52 2023] [44 Monk] Santiwill (Dark Elf) <Alianza>\n\
            [Sun Nov 19 09:52:52 2023] [49 Enchanter] Freddy (High Elf) <Alianza>\n\
            [Sun Nov 19 09:52:52 2023] [37 Paladin] Luthor (Human) <Alianza>\n\
            [Sun Nov 19 09:52:52 2023] [48 Warrior] Tank (Ogre) <Alianza>\n\
            [Sun Nov 19 09:52:52 2023] There are 6 players in Kedge Keep.";
        let parsed = parse_who(log);
        assert_eq!(
            parsed.characters,
            vec!["Arbusto", "Julia", "Santiwill", "Freddy", "Luthor", "Tank"]
        );
        // 2023-11-19 09:52:52 UTC
        assert_eq!(parsed.ts_ms, Some(1_700_387_572_000));
    }

    #[test]
    fn empty_and_garbage_are_harmless() {
        assert!(parse_who("").characters.is_empty());
        let p = parse_who("no brackets at all");
        assert!(p.characters.is_empty() && p.ts_ms.is_none());
    }
}
