//! `/roster` — the guild roster, absorbed from `nocturnal-roster-bot`.
//!
//! The roster bot kept its data in a Google Sheet and its identity in a cell
//! note. Here the ledger owns it: `roster.character.*` events, a projection,
//! and a page that is a pure function of the projection. The command surface
//! is the roster bot's — add / edit / remove / export — with the same
//! options, ranges and refusals, so nothing members already know changes.
//!
//! One deliberate difference, recorded in commands.md: raid-access flags are
//! an option on the command (`access: VP, ST`) rather than a second
//! interactive menu. A menu with a 60-second collector is the same shape of
//! thing that produced the auction bugs, and a typed list is checked against
//! the configured labels before the ledger ever sees it.

use nocturnal_core::{Command, MainRank, RosterCharacter};
use nocturnal_telemetry::attr;
use poise::serenity_prelude as serenity;

use crate::discord::{execute, rejection_text, require_guild, Context, Data, Error};

/// The class option. One variant per entry in `nocturnal_core::CLASSES`,
/// pinned by a test so the picker and the ledger cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, poise::ChoiceParameter)]
pub enum Class {
    Bard,
    Cleric,
    Druid,
    Enchanter,
    Magician,
    Monk,
    Necromancer,
    Paladin,
    Ranger,
    Rogue,
    #[name = "Shadow Knight"]
    ShadowKnight,
    Shaman,
    Warrior,
    Wizard,
    Beastlord,
}

impl Class {
    fn as_str(self) -> &'static str {
        match self {
            Class::Bard => "Bard",
            Class::Cleric => "Cleric",
            Class::Druid => "Druid",
            Class::Enchanter => "Enchanter",
            Class::Magician => "Magician",
            Class::Monk => "Monk",
            Class::Necromancer => "Necromancer",
            Class::Paladin => "Paladin",
            Class::Ranger => "Ranger",
            Class::Rogue => "Rogue",
            Class::ShadowKnight => "Shadow Knight",
            Class::Shaman => "Shaman",
            Class::Warrior => "Warrior",
            Class::Wizard => "Wizard",
            Class::Beastlord => "Beastlord",
        }
    }
    #[cfg(test)]
    const ALL: [Class; 15] = [
        Class::Bard,
        Class::Cleric,
        Class::Druid,
        Class::Enchanter,
        Class::Magician,
        Class::Monk,
        Class::Necromancer,
        Class::Paladin,
        Class::Ranger,
        Class::Rogue,
        Class::ShadowKnight,
        Class::Shaman,
        Class::Warrior,
        Class::Wizard,
        Class::Beastlord,
    ];
}

/// Parse `access: VP, ST` against the configured labels. Case-insensitive on
/// the way in, canonical spelling on the way out, order as configured.
fn parse_access(raw: Option<&str>, allowed: &[String]) -> Result<Option<Vec<String>>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let raw = raw.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("none") {
        return Ok(Some(Vec::new()));
    }
    let mut picked = Vec::new();
    for token in raw.split(',') {
        let t = token.trim();
        if t.is_empty() {
            continue;
        }
        match allowed.iter().find(|a| a.eq_ignore_ascii_case(t)) {
            Some(a) => {
                if !picked.contains(a) {
                    picked.push(a.clone());
                }
            }
            None => {
                return Err(format!(
                    "`{t}` is not an access label — the choices are {}",
                    allowed.join(", ")
                ))
            }
        }
    }
    picked.sort_by_key(|p| allowed.iter().position(|a| a == p));
    Ok(Some(picked))
}

fn main_rank(raw: Option<&str>) -> Result<Option<Option<MainRank>>, String> {
    Ok(
        match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            None => None,
            Some("main") => Some(Some(MainRank::Main)),
            Some("second") | Some("m2") => Some(Some(MainRank::Second)),
            Some("alt") | Some("none") | Some("") => Some(None),
            Some(other) => return Err(format!("`{other}` — use main, second or alt")),
        },
    )
}

fn describe(c: &RosterCharacter) -> String {
    let rank = match c.main {
        Some(MainRank::Main) => "M-",
        Some(MainRank::Second) => "M2-",
        None => "",
    };
    let mut s = format!("{}: `{} ({rank}{})`", c.class, c.name, c.level);
    if let Some(url) = &c.profile_url {
        s.push_str(&format!(" • <{url}>"));
    }
    if let Some(aa) = c.aa {
        s.push_str(&format!(" • AA={aa}"));
    }
    if !c.access.is_empty() {
        s.push_str(&format!(" • Access=[{}]", c.access.join(", ")));
    }
    s
}

/// Shared body of add and edit: build the record (merging with the existing
/// one on edit, exactly as the legacy bot preserved link and access when the
/// option was left out), then let the ledger decide.
#[allow(clippy::too_many_arguments)]
async fn upsert(
    ctx: Context<'_>,
    replace: bool,
    name: String,
    class: Class,
    level: i64,
    aa: Option<i64>,
    quarmy_link: Option<String>,
    access: Option<String>,
    main: Option<String>,
) -> Result<(), Error> {
    let ledger_guild = require_guild(&ctx)?;
    crate::discord::ack_ephemeral(&ctx).await?;
    let player = ctx.author().id.get();
    let allowed = ctx.data().roster_access_labels.clone();

    let access = match parse_access(access.as_deref(), &allowed) {
        Ok(a) => a,
        Err(e) => {
            ctx.say(format!(":no_entry: {e}")).await?;
            return Ok(());
        }
    };
    let main = match main_rank(main.as_deref()) {
        Ok(m) => m,
        Err(e) => {
            ctx.say(format!(":no_entry: {e}")).await?;
            return Ok(());
        }
    };

    // On edit, fields left out mean "as before". On add there is no before.
    let key = name.to_lowercase();
    let existing = ctx
        .data()
        .driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .and_then(|g| g.roster.get(&player))
                .and_then(|chars| chars.get(&key))
                .cloned()
        })
        .await;
    let character = RosterCharacter {
        name: name.trim().to_owned(),
        class: class.as_str().to_owned(),
        level: level.clamp(0, 255) as u8,
        aa: aa
            .map(|a| a.clamp(0, u16::MAX as i64) as u16)
            .or(existing.as_ref().and_then(|e| e.aa)),
        profile_url: quarmy_link
            .map(|u| u.trim().to_owned())
            .filter(|u| !u.is_empty())
            .or(existing.as_ref().and_then(|e| e.profile_url.clone())),
        access: access.unwrap_or_else(|| {
            existing
                .as_ref()
                .map(|e| e.access.clone())
                .unwrap_or_default()
        }),
        main: main.unwrap_or(existing.as_ref().and_then(|e| e.main)),
    };

    match execute(
        &ctx,
        Command::SetRosterCharacter {
            player,
            character: character.clone(),
            replace,
        },
    )
    .await?
    {
        Ok(_) => {
            tracing::info!(
                { attr::NOCTURNAL_PLAYER_ID } = player,
                { attr::NOCTURNAL_COMMAND } = if replace { "roster.edit" } else { "roster.add" },
                "roster character set"
            );
            ctx.say(format!(
                "{} • {}",
                if replace { "Updated" } else { "Saved" },
                describe(&character)
            ))
            .await?;
            if let Some(out) = &ctx.data().roster_output {
                crate::roster_page::rematerialize(
                    ctx.serenity_context().http.as_ref(),
                    ctx.guild_id().map_or(0, |g| g.get()),
                    &ctx.data().driver,
                    out,
                    &ctx.data().members,
                    ledger_guild,
                    ctx.data().ourios.as_ref(),
                    &ctx.data().item_mirror,
                    &ctx.data().site,
                )
                .await;
            }
        }
        Err(e) => {
            ctx.say(rejection_text(&e)).await?;
        }
    }
    Ok(())
}

/// Manage your characters on the guild roster.
#[poise::command(
    slash_command,
    rename = "roster",
    subcommands("add", "edit", "remove", "rank", "export")
)]
pub async fn roster(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Add a character to your roster row.
#[allow(clippy::too_many_arguments)] // one slash option per parameter, as the roster bot had
#[tracing::instrument(name = "command.roster.add", skip_all, err, fields(otel.kind = "server"))]
#[poise::command(slash_command, ephemeral)]
pub async fn add(
    ctx: Context<'_>,
    #[description = "Character name"] name: String,
    #[description = "Class"] class: Class,
    #[description = "Level 1–65"]
    #[min = 1]
    #[max = 65]
    level: i64,
    #[description = "Alternate Abilities 1–1000"]
    #[min = 1]
    #[max = 1000]
    aa: Option<i64>,
    #[description = "Quarmy character page (https://quarmy.com/...)"] quarmy_link: Option<String>,
    #[description = "Raid access, comma-separated: VP, ST, Emp, VT"] access: Option<String>,
    #[description = "main, second or alt"] main: Option<String>,
) -> Result<(), Error> {
    upsert(
        ctx,
        false,
        name,
        class,
        level,
        aa,
        quarmy_link,
        access,
        main,
    )
    .await
}

/// Edit a character already on your roster row. Fields left out stay as they were.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(name = "command.roster.edit", skip_all, err, fields(otel.kind = "server"))]
#[poise::command(slash_command, ephemeral)]
pub async fn edit(
    ctx: Context<'_>,
    #[description = "Character name"] name: String,
    #[description = "Class"] class: Class,
    #[description = "Level 1–65"]
    #[min = 1]
    #[max = 65]
    level: i64,
    #[description = "Alternate Abilities 1–1000"]
    #[min = 1]
    #[max = 1000]
    aa: Option<i64>,
    #[description = "Quarmy character page; leave empty to keep the existing one"]
    quarmy_link: Option<String>,
    #[description = "Raid access, comma-separated; `none` clears"] access: Option<String>,
    #[description = "main, second or alt"] main: Option<String>,
) -> Result<(), Error> {
    upsert(ctx, true, name, class, level, aa, quarmy_link, access, main).await
}

/// Remove a character from your roster row.
#[tracing::instrument(name = "command.roster.remove", skip_all, err, fields(otel.kind = "server"))]
#[poise::command(slash_command, ephemeral)]
pub async fn remove(
    ctx: Context<'_>,
    #[description = "Character name"] name: String,
) -> Result<(), Error> {
    let ledger_guild = require_guild(&ctx)?;
    crate::discord::ack_ephemeral(&ctx).await?;
    let player = ctx.author().id.get();
    match execute(
        &ctx,
        Command::RemoveRosterCharacter {
            player,
            name: name.clone(),
        },
    )
    .await?
    {
        Ok(_) => {
            ctx.say(format!("Removed **{name}** from your row."))
                .await?;
            if let Some(out) = &ctx.data().roster_output {
                crate::roster_page::rematerialize(
                    ctx.serenity_context().http.as_ref(),
                    ctx.guild_id().map_or(0, |g| g.get()),
                    &ctx.data().driver,
                    out,
                    &ctx.data().members,
                    ledger_guild,
                    ctx.data().ourios.as_ref(),
                    &ctx.data().item_mirror,
                    &ctx.data().site,
                )
                .await;
            }
        }
        Err(e) => {
            ctx.say(rejection_text(&e)).await?;
        }
    };
    Ok(())
}

/// The rank an officer gives a character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, poise::ChoiceParameter)]
pub enum Rank {
    #[name = "main"]
    Main,
    #[name = "second"]
    Second,
    #[name = "alt"]
    Alt,
}

/// Officers: rank a member's character as main, second or alt.
//
// The Main bid button offers a member's main; the rest bid as ALT. A ledger
// event with the officer as actor, so the ranking has a history.
#[tracing::instrument(name = "command.roster.rank", skip_all, err, fields(otel.kind = "server"))]
#[poise::command(slash_command, ephemeral, check = "crate::discord::officer_check")]
pub async fn rank(
    ctx: Context<'_>,
    #[description = "The member"] member: serenity::User,
    #[description = "Character name (on that member's row)"] name: String,
    #[description = "main, second or alt"] rank: Rank,
) -> Result<(), Error> {
    let ledger_guild = require_guild(&ctx)?;
    crate::discord::ack_ephemeral(&ctx).await?;
    let player = member.id.get();
    let key = name.trim().to_lowercase();
    let existing = ctx
        .data()
        .driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .and_then(|g| g.roster.get(&player))
                .and_then(|chars| chars.get(&key))
                .cloned()
        })
        .await;
    let Some(mut character) = existing else {
        ctx.say(format!(
            ":no_entry: **{}** is not on <@{player}>'s row — they add it with `/roster add`, or it appears once their Zeal reports it.",
            name.trim()
        ))
        .await?;
        return Ok(());
    };
    let main = match rank {
        Rank::Main => Some(MainRank::Main),
        Rank::Second => Some(MainRank::Second),
        Rank::Alt => None,
    };
    // One main per member: ranking a new main demotes the old one to alt,
    // so the Main bid button never has two answers.
    let mut demote: Option<RosterCharacter> = None;
    if main == Some(MainRank::Main) {
        let key = character.name.to_lowercase();
        demote = ctx
            .data()
            .driver
            .query(move |l| {
                l.state()
                    .guild(ledger_guild)
                    .and_then(|g| g.roster.get(&player))
                    .and_then(|chars| {
                        chars
                            .values()
                            .find(|c| {
                                c.main == Some(MainRank::Main) && c.name.to_lowercase() != key
                            })
                            .cloned()
                    })
            })
            .await;
    }
    character.main = main;
    let mut lines = Vec::new();
    if let Some(mut old) = demote {
        old.main = None;
        let old_name = old.name.clone();
        match execute(
            &ctx,
            Command::SetRosterCharacter {
                player,
                character: old,
                replace: true,
            },
        )
        .await?
        {
            Ok(_) => lines.push(format!("**{old_name}** is no longer the main.")),
            Err(e) => {
                ctx.say(rejection_text(&e)).await?;
                return Ok(());
            }
        }
    }
    match execute(
        &ctx,
        Command::SetRosterCharacter {
            player,
            character: character.clone(),
            replace: true,
        },
    )
    .await?
    {
        Ok(_) => {
            tracing::info!(
                { attr::NOCTURNAL_PLAYER_ID } = player,
                { attr::NOCTURNAL_COMMAND } = "roster.rank",
                "roster character ranked"
            );
            lines.push(format!("<@{player}> • {}", describe(&character)));
            ctx.say(lines.join("\n")).await?;
            if let Some(out) = &ctx.data().roster_output {
                crate::roster_page::rematerialize(
                    ctx.serenity_context().http.as_ref(),
                    ctx.guild_id().map_or(0, |g| g.get()),
                    &ctx.data().driver,
                    out,
                    &ctx.data().members,
                    ledger_guild,
                    ctx.data().ourios.as_ref(),
                    &ctx.data().item_mirror,
                    &ctx.data().site,
                )
                .await;
            }
        }
        Err(e) => {
            ctx.say(rejection_text(&e)).await?;
        }
    }
    Ok(())
}

/// Officers: every guild member as a CSV — ID, username, display name, roles, bot/human, joined.
#[tracing::instrument(name = "command.roster.export", skip_all, err, fields(otel.kind = "server"))]
#[poise::command(slash_command, ephemeral, check = "crate::discord::officer_check")]
pub async fn export(ctx: Context<'_>) -> Result<(), Error> {
    crate::discord::ack_ephemeral(&ctx).await?;
    let Some(guild_id) = ctx.guild_id() else {
        ctx.say(":no_entry: Only in a server.").await?;
        return Ok(());
    };
    // Listing members needs the Server Members privileged intent on the
    // application; without it Discord answers 403. Say exactly that rather
    // than "export failed".
    let mut after: Option<serenity::UserId> = None;
    let mut rows: Vec<(u64, String, String, String, &str, String)> = Vec::new();
    loop {
        let page = match guild_id
            .members(ctx.serenity_context(), Some(1000), after)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                let msg = e.to_string();
                ctx.say(if msg.contains("Missing Access") || msg.contains("50001") {
                    ":no_entry: Discord refused to list members. Enable **Server Members Intent** for this bot in the Developer Portal (Bot → Privileged Gateway Intents), then try again.".to_owned()
                } else {
                    format!(":no_entry: Could not list members: {msg}")
                })
                .await?;
                return Ok(());
            }
        };
        if page.is_empty() {
            break;
        }
        for m in &page {
            let mut roles: Vec<String> = Vec::new();
            if let Some(g) = ctx.guild() {
                let mut rs: Vec<_> = m.roles.iter().filter_map(|r| g.roles.get(r)).collect();
                rs.sort_by_key(|r| std::cmp::Reverse(r.position));
                roles = rs.iter().map(|r| r.name.clone()).collect();
            }
            rows.push((
                m.user.id.get(),
                m.user.name.clone(),
                m.display_name().to_string(),
                format!("[{}]", roles.join(",")),
                if m.user.bot { "Bot" } else { "Human" },
                m.joined_at.and_then(|t| t.to_rfc3339()).unwrap_or_default(),
            ));
        }
        after = page.last().map(|m| m.user.id);
        if page.len() < 1000 {
            break;
        }
    }
    rows.sort_by(|a, b| a.5.cmp(&b.5));
    let mut csv = String::from("ID,Username,Display Name,Roles,User Type,Join Date\n");
    for (id, u, d, r, t, j) in &rows {
        let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
        csv.push_str(&format!("{id},{},{},{},{t},{}\n", q(u), q(d), q(r), q(j)));
    }
    ctx.send(
        poise::CreateReply::default()
            .content(format!("Exported {} members.", rows.len()))
            .attachment(serenity::CreateAttachment::bytes(
                csv.into_bytes(),
                "raw-discord-data.csv",
            ))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![roster()]
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use nocturnal_core::CLASSES;

    fn labels() -> Vec<String> {
        ["VP", "ST", "Emp", "VT"].map(String::from).to_vec()
    }

    #[test]
    fn access_is_canonicalised_and_ordered_as_configured() {
        assert_eq!(
            parse_access(Some("st, vp"), &labels()).unwrap(),
            Some(vec!["VP".into(), "ST".into()])
        );
        assert_eq!(parse_access(Some("none"), &labels()).unwrap(), Some(vec![]));
        assert_eq!(
            parse_access(None, &labels()).unwrap(),
            None,
            "left out means unchanged"
        );
        assert!(parse_access(Some("VP, Kael"), &labels()).is_err());
    }

    #[test]
    fn the_picker_and_the_ledger_agree_on_the_classes() {
        let picked: Vec<&str> = Class::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(picked, CLASSES.to_vec());
    }

    #[test]
    fn main_rank_reads_the_three_words() {
        assert_eq!(main_rank(Some("Main")).unwrap(), Some(Some(MainRank::Main)));
        assert_eq!(main_rank(Some("m2")).unwrap(), Some(Some(MainRank::Second)));
        assert_eq!(main_rank(Some("alt")).unwrap(), Some(None));
        assert_eq!(main_rank(None).unwrap(), None);
        assert!(main_rank(Some("boss")).is_err());
    }
}

// ---------------------------------------------------------------------------
// One-time import of the Google Sheet (M9)
// ---------------------------------------------------------------------------

/// The page payload the Apps Script served: what `--import-roster` reads.
#[derive(Debug, serde::Deserialize)]
pub struct SheetPayload {
    pub values: Vec<Vec<serde_json::Value>>,
    #[serde(default)]
    pub notes: Vec<Vec<String>>,
    #[serde(default)]
    pub links: Vec<Vec<Option<String>>>,
}

/// A row of the sheet, parsed but not yet joined to a Discord member.
#[derive(Debug, PartialEq)]
pub struct SheetRow {
    pub display_name: String,
    pub characters: Vec<RosterCharacter>,
}

/// `Shaku (M-60)` → name, main rank, level. The sheet also held plain
/// `Shaku (60)` and `Shaku (M2-60)`.
fn parse_cell(text: &str) -> Option<(String, Option<MainRank>, u8)> {
    let text = text.trim();
    let open = text.rfind('(')?;
    let close = text.rfind(')')?;
    if close < open {
        return None;
    }
    let name = text[..open].trim();
    let inner = text[open + 1..close].trim();
    let (main, lvl) = if let Some(rest) = inner.strip_prefix("M2-") {
        (Some(MainRank::Second), rest)
    } else if let Some(rest) = inner.strip_prefix("M-") {
        (Some(MainRank::Main), rest)
    } else {
        (None, inner)
    };
    let level: u8 = lvl.trim().parse().ok()?;
    (!name.is_empty()).then(|| (name.to_owned(), main, level))
}

/// `AA: 355\nAccess: ST, VP` → (aa, access), either absent.
fn parse_note(note: &str) -> (Option<u16>, Vec<String>) {
    let mut aa = None;
    let mut access = Vec::new();
    for line in note.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        match k.trim().to_ascii_lowercase().as_str() {
            "aa" => aa = v.trim().parse().ok(),
            "access" => {
                access = v
                    .split(',')
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(String::from)
                    .collect()
            }
            _ => {}
        }
    }
    (aa, access)
}

/// Every member row of the sheet, with its characters. Pure, so the parse is
/// tested on a synthetic payload rather than on somebody's real roster.
pub fn parse_sheet(p: &SheetPayload) -> Vec<SheetRow> {
    const HEADER: usize = 4;
    const CLASS0: usize = 3;
    let cell = |r: usize, c: usize| -> String {
        p.values
            .get(r)
            .and_then(|row| row.get(c))
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default()
    };
    let mut rows = Vec::new();
    for r in HEADER + 1..p.values.len() {
        let display_name = cell(r, 0).trim().to_owned();
        if display_name.is_empty() {
            continue;
        }
        let mut characters = Vec::new();
        for (i, class) in nocturnal_core::CLASSES.iter().enumerate() {
            let c = CLASS0 + i;
            let text = cell(r, c);
            let Some((name, main, level)) = parse_cell(&text) else {
                continue;
            };
            let note = p
                .notes
                .get(r)
                .and_then(|n| n.get(c))
                .map(String::as_str)
                .unwrap_or("");
            let (aa, access) = parse_note(note);
            let profile_url = p.links.get(r).and_then(|l| l.get(c)).cloned().flatten();
            characters.push(RosterCharacter {
                name,
                class: (*class).to_owned(),
                level,
                aa,
                profile_url,
                access,
                main,
            });
        }
        rows.push(SheetRow {
            display_name,
            characters,
        });
    }
    rows
}

/// What an import did, for the operator.
#[derive(Debug, Default)]
pub struct ImportReport {
    pub rows: usize,
    pub matched: usize,
    pub characters_imported: usize,
    pub characters_skipped: usize,
    pub refused: Vec<String>,
    pub unmatched: Vec<String>,
}

/// Import the sheet into the ledger, joining rows to members by display name.
///
/// The public payload carries no Discord IDs (the Apps Script strips the
/// column-A notes that held them), so the join is by name: every player the
/// ledger knows is looked up once, and any sheet name still unmatched is
/// tried against Discord's member search. Rows that match nobody are
/// reported, not guessed — a member can re-add their characters in a minute,
/// whereas a wrong join puts one person's characters on another's row.
///
/// Idempotent: a character already on a row is skipped, so re-running after
/// a fix imports only what is new.
pub async fn import_sheet(
    driver: &crate::driver::DriverHandle,
    http: &serenity::Http,
    discord_guild: serenity::GuildId,
    ledger_guild: u64,
    payload: &SheetPayload,
) -> anyhow::Result<ImportReport> {
    use nocturnal_core::Actor;
    let rows = parse_sheet(payload);
    let mut report = ImportReport {
        rows: rows.len(),
        ..Default::default()
    };

    // Display name → player id, from the members the ledger already knows.
    let known: Vec<u64> = driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .map_or(Vec::new(), |g| g.players.keys().copied().collect())
        })
        .await;
    let mut by_name: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for id in known {
        if let Ok(m) = discord_guild.member(http, serenity::UserId::new(id)).await {
            by_name.insert(m.display_name().to_lowercase(), id);
        }
    }

    for row in rows {
        let key = row.display_name.to_lowercase();
        let id = match by_name.get(&key).copied() {
            Some(id) => Some(id),
            None => discord_guild
                .search_members(http, &row.display_name, Some(5))
                .await
                .ok()
                .and_then(|ms| {
                    ms.into_iter()
                        .find(|m| m.display_name().eq_ignore_ascii_case(&row.display_name))
                        .map(|m| m.user.id.get())
                }),
        };
        let Some(player) = id else {
            report.unmatched.push(row.display_name);
            continue;
        };
        report.matched += 1;
        for character in row.characters {
            let name = character.name.clone();
            match driver
                .execute(
                    ledger_guild,
                    Actor::System,
                    Command::SetRosterCharacter {
                        player,
                        character,
                        replace: false,
                    },
                )
                .await
            {
                Ok(_) => report.characters_imported += 1,
                Err(crate::driver::ExecError::Rejected(
                    nocturnal_core::Rejection::RosterCharacterExists { .. },
                )) => report.characters_skipped += 1,
                Err(e) => report
                    .refused
                    .push(format!("{}: {name}: {e}", row.display_name)),
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod import_tests {
    use super::*;

    #[test]
    fn cells_and_notes_parse_the_way_the_sheet_wrote_them() {
        assert_eq!(
            parse_cell("Shaku (M-60)"),
            Some(("Shaku".into(), Some(MainRank::Main), 60))
        );
        assert_eq!(
            parse_cell("Eklavdra (25)"),
            Some(("Eklavdra".into(), None, 25))
        );
        assert_eq!(
            parse_cell("Asberdies (M2-60)"),
            Some(("Asberdies".into(), Some(MainRank::Second), 60))
        );
        assert_eq!(parse_cell("nonsense"), None);
        assert_eq!(
            parse_note("AA: 355\nAccess: ST, VP"),
            (Some(355), vec!["ST".into(), "VP".into()])
        );
        assert_eq!(parse_note(""), (None, vec![]));
    }

    #[test]
    fn a_sheet_payload_becomes_rows_with_characters() {
        let mut values: Vec<Vec<serde_json::Value>> = vec![vec![serde_json::Value::Null; 20]; 5];
        values[4][0] = "Discord profile".into();
        let mut row = vec![serde_json::Value::String(String::new()); 20];
        row[0] = "Asberdies / Shaku".into();
        row[3 + 11] = "Shaku (M-60)".into(); // Shaman
        row[3 + 3] = "Eklavdra (25)".into(); // Enchanter
        values.push(row);
        let mut notes = vec![vec![String::new(); 20]; 6];
        notes[5][3 + 11] = "AA: 355\nAccess: ST, VP".into();
        let mut links = vec![vec![None; 20]; 6];
        links[5][3 + 11] = Some("https://quarmy.com/c/shaku".into());
        let rows = parse_sheet(&SheetPayload {
            values,
            notes,
            links,
        });
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].display_name, "Asberdies / Shaku");
        let chars = &rows[0].characters;
        assert_eq!(chars.len(), 2);
        let shaku = chars.iter().find(|c| c.name == "Shaku").unwrap();
        assert_eq!(
            (shaku.class.as_str(), shaku.level, shaku.aa, shaku.main),
            ("Shaman", 60, Some(355), Some(MainRank::Main))
        );
        assert_eq!(shaku.access, vec!["ST", "VP"]);
        assert_eq!(
            shaku.profile_url.as_deref(),
            Some("https://quarmy.com/c/shaku")
        );
        assert_eq!(
            chars.iter().find(|c| c.name == "Eklavdra").unwrap().class,
            "Enchanter"
        );
    }
}
