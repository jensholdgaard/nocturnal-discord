//! `/flags`: Planes of Power flag progression for every member's characters.
//!
//! A member types `#popflags 1` … `#popflags 5` in game (Quarm's own command,
//! see [`super::popflags`]), pastes the output here, and the page shows every
//! step of the progression against that character: done, the next thing to
//! do, or locked behind something else. Each step says where the NPC stands,
//! what to say or hand in, what the game prints when it works, and what to
//! check when it did not.
//!
//! The steps (`flags/guide.json`) were extracted from Quarm's own quest
//! scripts (SecretsOTheP/quests) and server (SecretsOTheP/EQMacEmu), with NPC
//! positions from the Quarm database; every step names the script lines it
//! came from. Saved characters live in `popflags.json` beside the ledger,
//! keyed by the member's Discord login. Nothing here touches the ledger.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use maud::{html, Markup, PreEscaped};
use serde::{Deserialize, Serialize};

use super::popflags::{self, Flags, Known};
use super::upload::Head;
use super::Response;

/// A paste of all five sections is a few KB; this is generous.
pub const MAX_BODY: usize = 64 * 1024;

pub struct FlagsCtx {
    pub rt: tokio::runtime::Handle,
    pub site: crate::site::SiteHandle,
    pub path: PathBuf,
    /// Serialises read-modify-write of the store.
    lock: Mutex<()>,
}

impl FlagsCtx {
    pub fn new(rt: tokio::runtime::Handle, site: crate::site::SiteHandle, data_dir: &Path) -> Self {
        Self {
            rt,
            site,
            path: data_dir.join("popflags.json"),
            lock: Mutex::new(()),
        }
    }
}

// --- the guide ------------------------------------------------------------------------------

/// One condition on the flags: `present`, `absent`, `>=` a stage, or a `bit`
/// of a bit-string global (`hohtrials`, `sol_room`).
#[derive(Debug, Clone, Deserialize)]
pub struct Cond {
    pub flag: String,
    pub op: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub why: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Item {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Action {
    pub kind: String,
    #[serde(default)]
    pub npc: Option<String>,
    #[serde(default)]
    pub zone: Option<String>,
    #[serde(default)]
    pub say: Option<String>,
    #[serde(default)]
    pub items: Vec<Item>,
    pub detail: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Npc {
    pub name: String,
    pub zone: String,
    /// As `/loc` prints it: `Y, X, Z`.
    #[serde(default)]
    pub loc: Option<String>,
    #[serde(default)]
    pub spawn: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Step {
    pub id: String,
    pub tier: u8,
    pub plane: String,
    pub title: String,
    /// A side route or an extra no tier needs: shown, never "do next".
    #[serde(default)]
    pub optional: bool,
    pub done_when: Vec<Cond>,
    /// Also done when any of these holds: a pending memory counts as done
    /// once the Seer has converted it, which deletes the memory flag.
    #[serde(default)]
    pub alt_done: Vec<Cond>,
    #[serde(default)]
    pub requires: Vec<Cond>,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub npcs: Vec<Npc>,
    #[serde(default)]
    pub credit: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub debug: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub source: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Guide {
    /// The quests and server commits the steps were read from.
    pub quests_commit: String,
    pub server_commit: String,
    /// Zone short name → the name players know.
    pub zones: BTreeMap<String, String>,
    pub steps: Vec<Step>,
}

static GUIDE: LazyLock<Guide> = LazyLock::new(|| {
    serde_json::from_str(include_str!("flags/guide.json")).expect("flags/guide.json is valid")
});

/// Where a cited source line lives on GitHub.
fn source_url(src: &str, guide: &Guide) -> String {
    let (file, line) = src.split_once(':').unwrap_or((src, ""));
    let (repo, commit, path) = match file.strip_prefix("EQMacEmu/") {
        Some(p) => ("EQMacEmu", guide.server_commit.as_str(), p),
        None => (
            "quests",
            guide.quests_commit.as_str(),
            file.strip_prefix("quests/").unwrap_or(file),
        ),
    };
    let anchor = if line.is_empty() {
        String::new()
    } else {
        format!("#L{}", line.split('-').next().unwrap_or(line))
    };
    format!("https://github.com/SecretsOTheP/{repo}/blob/{commit}/{path}{anchor}")
}

// --- evaluating a character ----------------------------------------------------------------------

/// The flags with what the server implies filled in: the Seer deletes the
/// cipher halves once the cipher exists, and folds karana and the Marr book
/// into zebuxoruk, so their absence then means done, not missing.
fn effective(flags: &Flags) -> Flags {
    let mut f = flags.clone();
    let present = |f: &Flags, k: &str| f.get(k).is_some_and(|v| *v != Known::Absent);
    if present(&f, "cipher") {
        f.insert("saryrn".into(), Known::Present);
        f.insert("mmarr".into(), Known::Present);
    }
    if present(&f, "zebuxoruk") {
        f.insert("karana".into(), Known::Exact("4".into()));
        f.insert("mmarr_book".into(), Known::Present);
    }
    f
}

/// `Some(true/false)`, or `None` when the paste did not cover this flag.
/// A `note` is a requirement `#popflags` cannot show (a zone flag, an item,
/// a level): it is printed, never evaluated.
fn holds(c: &Cond, f: &Flags) -> Option<bool> {
    if c.op == "note" {
        return Some(true);
    }
    if c.op == "bit" {
        let i = c.value.as_deref()?.parse().ok()?;
        return popflags::bit(f, &c.flag, i);
    }
    let k = f.get(&c.flag)?;
    let n: u32 = c.value.as_deref().and_then(|v| v.parse().ok()).unwrap_or(1);
    match c.op.as_str() {
        "present" => Some(*k != Known::Absent),
        "absent" => Some(*k == Known::Absent),
        ">=" => match k {
            Known::AtLeast(m) if *m < n => None,
            _ => Some(k.stage() >= n),
        },
        "==" => match k {
            Known::Exact(v) => Some(c.value.as_deref() == Some(v.as_str())),
            Known::Absent => Some(false),
            Known::AtLeast(m) if *m > n => Some(false),
            _ => None,
        },
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Done,
    Next,
    Optional,
    Locked,
    Unknown,
}

pub fn status(step: &Step, flags: &Flags) -> Status {
    let f = effective(flags);
    // Plane of Time access means the server counts every tier complete.
    if f.get("time").is_some_and(|k| *k != Known::Absent) {
        return Status::Done;
    }
    let done: Vec<Option<bool>> = step.done_when.iter().map(|c| holds(c, &f)).collect();
    if done.iter().all(|d| *d == Some(true))
        || step.alt_done.iter().any(|c| holds(c, &f) == Some(true))
    {
        return Status::Done;
    }
    let req: Vec<Option<bool>> = step.requires.iter().map(|c| holds(c, &f)).collect();
    if req.contains(&Some(false)) {
        return Status::Locked;
    }
    if done.iter().any(Option::is_none) || req.iter().any(Option::is_none) {
        return Status::Unknown;
    }
    if step.optional {
        Status::Optional
    } else {
        Status::Next
    }
}

/// The server's own tier rules (`PopFlagsTierNComplete` in popflags.cpp).
pub fn tier_complete(tier: u8, flags: &Flags) -> Option<bool> {
    let f = effective(flags);
    let has = |k: &str| f.get(k).map(|v| *v != Known::Absent);
    let stage = |k: &str| f.get(k).map(Known::stage);
    let bit = |k: &str, i: usize| popflags::bit(&f, k, i);
    if has("time") == Some(true) {
        return Some(true);
    }
    let all = |xs: &[Option<bool>]| -> Option<bool> {
        if xs.contains(&Some(false)) {
            Some(false)
        } else if xs.iter().all(|x| *x == Some(true)) {
            Some(true)
        } else {
            None
        }
    };
    let ge = |k: &str, n: u32| stage(k).map(|s| s >= n);
    let either = |a: Option<bool>, b: Option<bool>| match (a, b) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    };
    match tier {
        1 => all(&[
            ge("mavuin", 3),
            ge("fuirstel", 5),
            ge("thelin", 4),
            has("poi_door"),
            ge("zeks", 2),
        ]),
        2 => all(&[
            ge("aerindar", 2),
            either(ge("karana", 2), has("zebuxoruk")),
            has("bertox_key"),
            ge("tylis", 2),
            either(has("saryrn"), has("cipher")),
        ]),
        3 => all(&[
            bit("hohtrials", 0),
            bit("hohtrials", 1),
            bit("hohtrials", 2),
            has("cipher"),
            ge("zebuxoruk", 2),
            ge("zeks", 7),
            bit("sol_room", 0),
            bit("sol_room", 1),
            bit("sol_room", 2),
            bit("sol_room", 3),
            bit("sol_room", 4),
            ge("pofire", 2),
        ]),
        4 | 5 => has("time"),
        _ => None,
    }
}

// --- the store -----------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Character {
    pub flags: Flags,
    #[serde(default)]
    pub sections: Vec<u8>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Member {
    /// The site's name for the member, for the guild table.
    pub name: String,
    pub characters: BTreeMap<String, Character>,
}

/// Login (lowercase) → that member's characters.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Store {
    pub members: BTreeMap<String, Member>,
}

pub fn read_store(path: &Path) -> Result<Store, String> {
    match std::fs::read_to_string(path) {
        Ok(raw) if raw.trim().is_empty() => Ok(Store::default()),
        Ok(raw) => serde_json::from_str(&raw)
            .map_err(|_| "popflags.json exists but is not valid; refusing to touch it.".to_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
        Err(e) => Err(format!("could not read the flag store: {e}")),
    }
}

pub fn write_store(path: &Path, store: &Store) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(store).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, bytes).map_err(|e| format!("could not write the flag store: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("could not write the flag store: {e}"))
}

/// EverQuest names: letters only, capitalised the way the game shows them.
pub fn character_name(raw: &str) -> Option<String> {
    let t = raw.trim();
    if !(3..=20).contains(&t.len()) || !t.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    let mut c = lower.chars();
    c.next()
        .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
}

/// Fold one paste into a member's character.
pub fn apply_paste(member: &mut Member, character: &str, parsed: popflags::Parsed, now: &str) {
    let entry = member.characters.entry(character.to_owned()).or_default();
    popflags::fold(&mut entry.flags, parsed.flags);
    for s in parsed.sections {
        if !entry.sections.contains(&s) {
            entry.sections.push(s);
        }
    }
    entry.sections.sort_unstable();
    entry.updated_at = now.to_owned();
}

// --- HTTP ------------------------------------------------------------------------------------------

fn json(status: &'static str, v: serde_json::Value) -> Response {
    Response {
        status,
        content_type: "application/json; charset=utf-8",
        body: v.to_string().into_bytes(),
        headers: vec!["cache-control: no-store".into()],
    }
}

fn error(status: &'static str, message: &str) -> Response {
    json(
        status,
        serde_json::json!({ "ok": false, "message": message }),
    )
}

/// Who is asking: the Perses login (lowercased key) and the site's name.
fn viewer(ctx: &FlagsCtx, head: &Head) -> Option<(String, String)> {
    let cookie = head.cookie.as_deref()?;
    let login = ctx.rt.block_on(super::upload::whoami(cookie))?;
    let name = ctx
        .site
        .read()
        .ok()
        .and_then(|s| s.clone())
        .and_then(|s| s.members.get(&login).map(|m| m.name.clone()))
        .unwrap_or_else(|| login.clone());
    Some((login.to_lowercase(), name))
}

#[derive(Deserialize)]
struct SaveBody {
    character: String,
    #[serde(default)]
    text: String,
}

pub fn handle(ctx: &FlagsCtx, head: &Head, body: &[u8]) -> Response {
    let (path, query) = head
        .path
        .split_once('?')
        .unwrap_or((head.path.as_str(), ""));
    match (head.method.as_str(), path) {
        ("GET", "/flags") => {
            let viewer = viewer(ctx, head);
            let store = read_store(&ctx.path).unwrap_or_default();
            let wanted = query
                .split('&')
                .find_map(|kv| kv.strip_prefix("c="))
                .and_then(character_name);
            let html = page(
                &store,
                viewer.as_ref().map(|(l, _)| l.as_str()),
                wanted.as_deref(),
            );
            Response {
                status: "200 OK",
                content_type: "text/html; charset=utf-8",
                body: html.into_bytes(),
                headers: vec!["cache-control: no-cache".into()],
            }
        }
        ("POST", "/flags/save" | "/flags/delete") => {
            if !head.xhr {
                return error("403 Forbidden", "Missing the site's request header.");
            }
            let Some((login, name)) = viewer(ctx, head) else {
                return error("401 Unauthorized", "Sign in first.");
            };
            let Ok(req) = serde_json::from_slice::<SaveBody>(body) else {
                return error(
                    "400 Bad Request",
                    "Send the character name and the pasted text.",
                );
            };
            let Some(character) = character_name(&req.character) else {
                return error(
                    "422 Unprocessable Content",
                    "A character name is letters only, like Bubblie.",
                );
            };
            let _guard = ctx.lock.lock();
            let mut store = match read_store(&ctx.path) {
                Ok(s) => s,
                Err(e) => return error("500 Internal Server Error", &e),
            };
            let member = store.members.entry(login).or_default();
            member.name = name;
            if path == "/flags/delete" {
                member.characters.remove(&character);
                return match write_store(&ctx.path, &store) {
                    Ok(()) => json(
                        "200 OK",
                        serde_json::json!({ "ok": true, "character": character }),
                    ),
                    Err(e) => error("500 Internal Server Error", &e),
                };
            }
            let parsed = popflags::parse(&req.text);
            if parsed.flags.is_empty() {
                return error(
                    "422 Unprocessable Content",
                    "No #popflags lines found. In game, type #popflags 1 (then 2, 3, 4 and 5) and paste what it prints.",
                );
            }
            let sections = parsed.sections.clone();
            let unrecognised = parsed.unrecognised.clone();
            let now =
                crate::raid_names::rfc3339(crate::discord::chrono_now_ms()).unwrap_or_default();
            apply_paste(member, &character, parsed, &now);
            match write_store(&ctx.path, &store) {
                Ok(()) => json(
                    "200 OK",
                    serde_json::json!({
                        "ok": true,
                        "character": character,
                        "sections": sections,
                        "unrecognised": unrecognised,
                    }),
                ),
                Err(e) => error("500 Internal Server Error", &e),
            }
        }
        _ => Response::not_found(),
    }
}

// --- the page --------------------------------------------------------------------------------------

const TIERS: &[(u8, &str)] = &[
    (1, "Tier 1"),
    (2, "Tier 2"),
    (3, "Tier 3"),
    (4, "Tier 4: the Elemental Planes"),
    (5, "Plane of Time"),
];

fn pill(s: Status) -> Markup {
    let (class, text) = match s {
        Status::Done => ("good", "Done"),
        Status::Next => ("next", "Do next"),
        Status::Optional => ("muted", "Optional"),
        Status::Locked => ("low", "Locked"),
        Status::Unknown => ("muted", "Not pasted"),
    };
    html! { span class={ "pill " (class) } { (text) } }
}

fn tier_pill(t: Option<bool>, started: bool) -> Markup {
    let (class, text) = match (t, started) {
        (Some(true), _) => ("good", "Complete"),
        (_, true) => ("next", "In progress"),
        (Some(false), false) => ("muted", "Not started"),
        (None, false) => ("muted", "Not pasted"),
    };
    html! { span class={ "pill " (class) } { (text) } }
}

fn zone_name<'a>(g: &'a Guide, short: &'a str) -> &'a str {
    g.zones.get(short).map(String::as_str).unwrap_or(short)
}

fn step_card(g: &Guide, step: &Step, st: Option<Status>) -> Markup {
    let open = matches!(st, Some(Status::Next));
    html! {
        details class={ "step " (match st { Some(Status::Done) => "is-done", Some(Status::Next) => "is-next", Some(Status::Locked) => "is-locked", Some(Status::Optional) => "is-optional", _ => "" }) } open[open] id=(step.id) {
            summary {
                @if let Some(s) = st { (pill(s)) " " }
                span class="title" { (step.title) }
            }
            div class="body" {
                @if !step.actions.is_empty() {
                    ol class="actions" {
                        @for a in &step.actions {
                            li {
                                span class="kind" { (a.kind) }
                                " " (a.detail)
                                @if let Some(npc) = &a.npc {
                                    " " span class="muted" { "(" (npc.replace('_', " ").trim_start_matches('#')) @if let Some(z) = &a.zone { ", " (zone_name(g, z)) } ")" }
                                }
                                @if let Some(say) = &a.say { " " span class="say" { "Say: " code { (say) } } }
                                @if !a.items.is_empty() {
                                    " " span class="items" { "Items: "
                                        @for (i, it) in a.items.iter().enumerate() {
                                            @if i > 0 { ", " }
                                            a href={ "https://www.pqdi.cc/item/" (it.id) } target="_blank" rel="noopener" { (it.name) }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                @if !step.npcs.is_empty() {
                    table class="npcs" {
                        thead { tr { th { "NPC" } th { "Zone" } th { "/loc" } th { "Spawn" } } }
                        tbody {
                            @for n in &step.npcs {
                                tr {
                                    td { (n.name.replace('_', " ").trim_start_matches('#')) }
                                    td { (zone_name(g, &n.zone)) }
                                    td class="num" { (n.loc.as_deref().unwrap_or("-")) }
                                    td { (n.spawn.as_deref().unwrap_or("")) @if let Some(note) = &n.notes { " " span class="muted" { (note) } } }
                                }
                            }
                        }
                    }
                }
                @if !step.requires.is_empty() {
                    p class="req" { b { "Needs first: " }
                        @for (i, c) in step.requires.iter().enumerate() {
                            @if i > 0 { "; " }
                            (c.why.clone().unwrap_or_else(|| format!("{} {} {}", c.flag, c.op, c.value.clone().unwrap_or_default())))
                        }
                    }
                }
                @if let Some(c) = &step.credit { p { b { "Who gets it: " } (c) } }
                @if let Some(m) = &step.message { p { b { "When it works you see: " } q { (m) } } }
                @if !step.debug.is_empty() {
                    div class="debug" {
                        b { "If it didn't work" }
                        ul { @for d in &step.debug { li { (d) } } }
                    }
                }
                @if let Some(n) = &step.notes { p class="muted" { (n) } }
                @if !step.source.is_empty() {
                    p class="src" { "Source: "
                        @for (i, s) in step.source.iter().enumerate() {
                            @if i > 0 { ", " }
                            a href=(source_url(s, g)) target="_blank" rel="noopener" { (s) }
                        }
                    }
                }
            }
        }
    }
}

const FLAGS_CSS: &str = r#"
.flags .intro{max-width:72ch}
.flags form.paste{display:grid;gap:10px;max-width:760px;margin:14px 0 22px}
.flags form.paste textarea{width:100%;min-height:150px;font:13px/1.4 ui-monospace,Consolas,monospace;background:var(--surface);color:var(--text);border:1px solid var(--line-strong);border-radius:6px;padding:8px}
.flags form.paste input{font:inherit;background:var(--surface);color:var(--text);border:1px solid var(--line-strong);border-radius:6px;padding:6px 8px;max-width:240px}
.flags .row{display:flex;gap:10px;align-items:center;flex-wrap:wrap}
.flags button{font:inherit;cursor:pointer;border-radius:6px;padding:7px 14px;border:1px solid var(--accent);background:var(--accent);color:var(--accent-ink)}
.flags button.ghost{background:transparent;color:var(--muted);border-color:var(--line-strong)}
.flags #msg{min-height:1.4em;color:var(--muted)}
.flags .chars{display:flex;gap:6px;flex-wrap:wrap;margin:6px 0 18px}
.flags .chars a{padding:5px 10px;border:1px solid var(--line);border-radius:6px;text-decoration:none;color:var(--text)}
.flags .chars a[aria-current="page"]{border-color:var(--accent);font-weight:700}
.flags .tier{margin:22px 0 8px;display:flex;gap:10px;align-items:baseline}
.flags .tier h2{margin:0;font:600 26px/1.1 "Cormorant Garamond",Georgia,serif}
.flags h3{margin:16px 0 6px;font-size:15px;color:var(--muted);font-weight:700;letter-spacing:.02em}
.flags .pill{display:inline-block;font-size:12px;font-weight:700;padding:2px 8px;border-radius:999px;border:1px solid var(--line-strong);color:var(--muted);white-space:nowrap}
.flags .pill.good{color:var(--good);border-color:var(--good)}
.flags .pill.next{color:var(--accent);border-color:var(--accent)}
.flags .pill.low{color:var(--low);border-color:var(--low)}
.flags details.step{border:1px solid var(--line);border-radius:8px;background:var(--surface);margin:6px 0}
.flags details.step.is-next{border-color:var(--accent)}
.flags details.step.is-done summary .title{color:var(--muted)}
.flags details.step summary{cursor:pointer;padding:9px 12px;list-style-position:inside}
.flags details.step .body{padding:0 14px 12px;display:grid;gap:8px}
.flags details.step .body p{margin:0}
.flags ol.actions{margin:0;padding-left:20px;display:grid;gap:4px}
.flags .kind{font-size:11px;text-transform:uppercase;letter-spacing:.06em;color:var(--brass);font-weight:700}
.flags code{background:var(--surface-2);padding:1px 5px;border-radius:4px}
.flags table.npcs{border-collapse:collapse;font-size:14px;width:auto}
.flags table.npcs th,.flags table.npcs td{padding:4px 10px;border-bottom:1px solid var(--line);text-align:left}
.flags .num{font-variant-numeric:tabular-nums;white-space:nowrap}
.flags .debug{background:var(--surface-2);border-radius:6px;padding:8px 12px}
.flags .debug ul{margin:6px 0 0;padding-left:18px;display:grid;gap:3px}
.flags .muted,.flags .src{color:var(--muted);font-size:13px}
.flags table.guild{border-collapse:collapse;margin-top:8px}
.flags table.guild th,.flags table.guild td{padding:5px 10px;border-bottom:1px solid var(--line);text-align:left;white-space:nowrap}
.flags .wrap{overflow-x:auto}
.flags .todo{border:1px solid var(--accent);border-radius:8px;padding:10px 14px;max-width:760px;background:var(--surface)}
.flags .todo ul{margin:6px 0 0;padding-left:18px;display:grid;gap:3px}
"#;

const FLAGS_JS: &str = r#"
(function(){
  const f=document.getElementById('paste'); if(!f) return;
  const msg=document.getElementById('msg');
  async function send(path, payload){
    const r=await fetch(path,{method:'POST',credentials:'include',headers:{'content-type':'application/json','x-nocturnal':'1'},body:JSON.stringify(payload)});
    let j={}; try{ j=await r.json(); }catch(e){}
    return [r.ok&&j.ok, j];
  }
  f.addEventListener('submit', async e=>{
    e.preventDefault();
    msg.textContent='Saving…';
    const [ok,j]=await send('/flags/save',{character:f.character.value,text:f.text.value});
    if(!ok){ msg.textContent=j.message||'That did not save.'; return; }
    let t='Saved '+j.character+' (sections '+(j.sections||[]).join(', ')+').';
    if((j.unrecognised||[]).length) t+=' '+j.unrecognised.length+' line(s) were not recognised; tell an officer: '+j.unrecognised.slice(0,3).join(' | ');
    msg.textContent=t;
    setTimeout(()=>{ location.href='/flags?c='+encodeURIComponent(j.character); }, j.unrecognised&&j.unrecognised.length?4000:600);
  });
  const del=document.getElementById('delete');
  if(del) del.addEventListener('click', async ()=>{
    if(!confirm('Forget '+del.dataset.c+' on this page?')) return;
    const [ok,j]=await send('/flags/delete',{character:del.dataset.c});
    if(ok) location.href='/flags'; else msg.textContent=j.message||'That did not delete.';
  });
})();
"#;

/// The whole page for `viewer` (their login key), with `wanted` selected.
pub fn page(store: &Store, viewer: Option<&str>, wanted: Option<&str>) -> String {
    let g = &*GUIDE;
    let mine = viewer.and_then(|v| store.members.get(v));
    let selected: Option<(&str, &Character)> = mine.and_then(|m| {
        match wanted {
            Some(w) => m.characters.get_key_value(w),
            None => m.characters.iter().next(),
        }
        .map(|(k, c)| (k.as_str(), c))
    });
    let flags = selected.map(|(_, c)| &c.flags);
    let grouped = group(g);
    let body = html! {
        style { (PreEscaped(FLAGS_CSS)) }
        div class="flags" {
            h1 { "Planes of Power flags" }
            p class="intro" {
                "In game, type " code { "#popflags 1" } ", then " code { "#popflags 2" } ", " code { "3" } ", " code { "4" } " and " code { "5" }
                ". Copy what it prints, from the chat window or your log file, and paste it below. "
                "The page then marks every step for that character and opens the ones to do next. "
                "Paste again after a kill or a hand-in; tiers you leave out are kept."
            }
            form id="paste" class="paste" {
                div class="row" {
                    label for="character" { "Character" }
                    input id="character" name="character" required placeholder="Bubblie" value=(selected.map(|(n, _)| n).unwrap_or("")) list="mychars";
                    @if let Some(m) = mine { datalist id="mychars" { @for n in m.characters.keys() { option value=(n) {} } } }
                }
                textarea name="text" required placeholder="=== Tier 1 Progression ===\n--- Plane of Justice ---\nMavuin's case: Not started\n…" {}
                div class="row" {
                    button type="submit" { "Save" }
                    @if let Some((n, _)) = selected { button type="button" class="ghost" id="delete" data-c=(n) { "Forget " (n) } }
                }
                div id="msg" aria-live="polite" {}
            }
            @if let Some(m) = mine {
                @if m.characters.len() > 1 {
                    div class="chars" {
                        @for n in m.characters.keys() {
                            a href={ "/flags?c=" (n) } aria-current=[(selected.map(|(s, _)| s) == Some(n.as_str())).then_some("page")] { (n) }
                        }
                    }
                }
            }
            @if let Some((n, c)) = selected {
                p class="muted" { (n) ", last pasted " (c.updated_at.get(..10).unwrap_or(&c.updated_at)) ". Sections on file: "
                    (c.sections.iter().map(u8::to_string).collect::<Vec<_>>().join(", ")) "." }
                @let next: Vec<&Step> = GUIDE.steps.iter().filter(|s| status(s, &c.flags) == Status::Next).collect();
                @if !next.is_empty() {
                    div class="todo" {
                        b { "Do next" }
                        ul { @for s in next { li { a href={ "#" (s.id) } { (s.title) } span class="muted" { " · " (s.plane) } } } }
                    }
                }
            } @else {
                p class="muted" { "No character pasted yet, so this is the whole guide without your progress." }
            }
            @for (t, label, planes) in &grouped {
                @let all: Vec<&Step> = planes.iter().flat_map(|(_, v)| v.iter().copied()).collect();
                @let started = flags.is_some_and(|f| all.iter().any(|s| status(s, f) == Status::Done));
                div class="tier" {
                    h2 { (label) }
                    @if let Some(f) = flags { (tier_pill(tier_complete(*t, f), started)) }
                }
                @for (plane, steps) in planes {
                    h3 { (plane) }
                    @for s in steps {
                        (step_card(g, s, flags.map(|f| status(s, f))))
                    }
                }
            }
            (guild_table(store))
            p class="src" {
                "Steps read from Quarm's quest scripts (quests " (g.quests_commit.get(..7).unwrap_or(&g.quests_commit))
                ") and server (EQMacEmu " (g.server_commit.get(..7).unwrap_or(&g.server_commit)) "), NPC positions from the Quarm database. "
                "/loc shows Y, X, Z, the way the game's /loc prints it."
            }
        }
        script { (PreEscaped(FLAGS_JS)) }
    };
    super::pages::layout("Flags", "flags", body, false)
}

/// Tiers in order, each with its planes in first-seen order.
type Grouped<'a> = Vec<(u8, &'static str, Vec<(&'a str, Vec<&'a Step>)>)>;

fn group(g: &Guide) -> Grouped<'_> {
    TIERS
        .iter()
        .filter_map(|(t, label)| {
            let mut planes: Vec<(&str, Vec<&Step>)> = Vec::new();
            for s in g.steps.iter().filter(|s| s.tier == *t) {
                match planes.iter_mut().find(|(p, _)| *p == s.plane) {
                    Some((_, v)) => v.push(s),
                    None => planes.push((s.plane.as_str(), vec![s])),
                }
            }
            (!planes.is_empty()).then_some((*t, *label, planes))
        })
        .collect()
}

fn guild_table(store: &Store) -> Markup {
    let mut rows: Vec<(&str, &str, &Character)> = store
        .members
        .values()
        .flat_map(|m| {
            m.characters
                .iter()
                .map(move |(n, c)| (n.as_str(), m.name.as_str(), c))
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    html! {
        @if !rows.is_empty() {
            div class="tier" { h2 { "The guild" } }
            div class="wrap" {
                table class="guild" {
                    thead { tr { th { "Character" } th { "Member" } th { "Tier 1" } th { "Tier 2" } th { "Tier 3" } th { "Time" } th { "Pasted" } } }
                    tbody {
                        @for (n, m, c) in rows {
                            tr {
                                td { a href={ "/flags?c=" (n) } { (n) } }
                                td { (m) }
                                @for t in [1u8, 2, 3, 5] {
                                    td { (tier_pill(tier_complete(t, &c.flags), c.sections.contains(&t))) }
                                }
                                td class="num" { (c.updated_at.get(..10).unwrap_or(&c.updated_at)) }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn cond(flag: &str, op: &str, value: Option<&str>) -> Cond {
        Cond {
            flag: flag.into(),
            op: op.into(),
            value: value.map(Into::into),
            why: None,
        }
    }

    fn step(done: Vec<Cond>, req: Vec<Cond>) -> Step {
        Step {
            id: "x".into(),
            tier: 1,
            plane: "P".into(),
            title: "t".into(),
            optional: false,
            done_when: done,
            alt_done: vec![],
            requires: req,
            actions: vec![],
            npcs: vec![],
            credit: None,
            message: None,
            debug: vec![],
            notes: None,
            source: vec![],
        }
    }

    #[test]
    fn the_guide_parses_and_every_step_has_a_source_and_a_known_tier() {
        let g = &*GUIDE;
        assert!(!g.steps.is_empty());
        let mut ids = std::collections::HashSet::new();
        for s in &g.steps {
            assert!(ids.insert(&s.id), "duplicate step id {}", s.id);
            assert!((1..=5).contains(&s.tier), "{}", s.id);
            assert!(!s.done_when.is_empty(), "{} has no done_when", s.id);
            assert!(!s.source.is_empty(), "{} cites no source", s.id);
            for c in s.done_when.iter().chain(&s.alt_done).chain(&s.requires) {
                assert!(
                    ["present", "absent", ">=", "==", "bit", "note"].contains(&c.op.as_str()),
                    "{}: op {}",
                    s.id,
                    c.op
                );
            }
        }
    }

    #[test]
    fn a_step_is_next_when_its_prerequisites_hold_and_locked_when_one_fails() {
        let s = step(
            vec![cond("fuirstel", ">=", Some("3"))],
            vec![cond("fuirstel", ">=", Some("2"))],
        );
        let f = popflags::parse("Fuirstel progression: The Ward was recovered").flags;
        assert_eq!(status(&s, &f), Status::Next);
        let f = popflags::parse("Fuirstel progression: Grummus was defeated").flags;
        assert_eq!(status(&s, &f), Status::Done);
        let f = popflags::parse("Fuirstel progression: Not started").flags;
        assert_eq!(status(&s, &f), Status::Locked);
        assert_eq!(status(&s, &Flags::new()), Status::Unknown);
    }

    #[test]
    fn the_cipher_counts_the_deleted_halves_as_done() {
        let s = step(vec![cond("saryrn", "present", None)], vec![]);
        let f = popflags::parse("Cipher information: Received").flags;
        assert_eq!(status(&s, &f), Status::Done);
    }

    #[test]
    fn tier_rules_follow_the_server() {
        let f = popflags::parse(
            "Mavuin's case: Mavuin's case is complete\nFuirstel progression: Fuirstel progression complete\nThelin progression: Thelin was released from Terris Thule\nFactory door access: Unlocked\nGiwin and Zek progression: Meet Giwin in Drunder",
        )
        .flags;
        assert_eq!(tier_complete(1, &f), Some(true));
        assert_eq!(tier_complete(2, &f), None);
    }

    #[test]
    fn a_paste_folds_into_the_named_character_and_names_are_normalised() {
        assert_eq!(character_name(" bUBBLIE "), Some("Bubblie".into()));
        assert_eq!(character_name("Bub bie"), None);
        let mut m = Member::default();
        apply_paste(
            &mut m,
            "Bubblie",
            popflags::parse(
                "=== Tier 2 Progression ===\nTylis progression: Tylis progression complete",
            ),
            "2026-10-05T20:00:00Z",
        );
        apply_paste(
            &mut m,
            "Bubblie",
            popflags::parse("=== Tier 1 Progression ===\nFactory door access: Unlocked"),
            "2026-10-05T21:00:00Z",
        );
        let c = &m.characters["Bubblie"];
        assert_eq!(c.sections, vec![1, 2]);
        assert_eq!(c.flags["tylis"], Known::Exact("2".into()));
        assert_eq!(c.updated_at, "2026-10-05T21:00:00Z");
    }

    #[test]
    fn the_page_renders_with_and_without_a_character() {
        let mut store = Store::default();
        let mut m = Member {
            name: "Bubblie".into(),
            ..Member::default()
        };
        apply_paste(
            &mut m,
            "Bubblie",
            popflags::parse("=== Tier 1 Progression ===\nFactory door access: Unlocked"),
            "2026-10-05T20:00:00Z",
        );
        store.members.insert("bubblie".into(), m);
        let html = page(&store, Some("bubblie"), None);
        assert!(html.contains("Planes of Power flags"));
        assert!(html.contains(r#"href="/flags" aria-current="page""#));
        assert!(html.contains("The guild"));
        let anon = page(&Store::default(), None, None);
        assert!(anon.contains("whole guide"));
    }
}
