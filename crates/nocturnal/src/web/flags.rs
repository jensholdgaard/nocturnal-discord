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
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Step {
    pub id: String,
    pub tier: u8,
    pub plane: String,
    pub title: String,
    /// One or two plain sentences: what the step is for.
    #[serde(default)]
    pub why: Option<String>,
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
    /// The script lines the step was read from: for whoever maintains the
    /// guide (and the tests), never shown to members.
    #[serde(default)]
    #[allow(dead_code)]
    pub source: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Guide {
    /// Zone short name → the name players know.
    pub zones: BTreeMap<String, String>,
    pub steps: Vec<Step>,
}

static GUIDE: LazyLock<Guide> = LazyLock::new(|| {
    serde_json::from_str(include_str!("flags/guide.json")).expect("flags/guide.json is valid")
});

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
//
// Written for a member who has never flagged before: plain words, the next
// thing to do first, and nothing that only makes sense to someone who has
// read the scripts. The script references stay in guide.json for whoever
// maintains it; the page never shows them.

const TIERS: &[(u8, &str, &str)] = &[
    (
        1,
        "Tier 1",
        "The first four planes. Most guilds start here.",
    ),
    (2, "Tier 2", "Opens once tier 1 stories are finished."),
    (
        3,
        "Tier 3",
        "Honor, Thunder, the Zeks and the Tower of Solusek Ro.",
    ),
    (4, "Tier 4", "The four elemental planes."),
    (5, "Plane of Time", "The last door."),
];

/// The words a status is shown with, and its colour class.
fn status_words(s: Status) -> (&'static str, &'static str) {
    match s {
        Status::Done => ("good", "Done"),
        Status::Next => ("next", "Do this next"),
        Status::Optional => ("muted", "Optional"),
        Status::Locked => ("low", "Not yet"),
        Status::Unknown => ("muted", "Unknown"),
    }
}

fn pill(s: Status) -> Markup {
    let (class, text) = status_words(s);
    html! { span class={ "pill " (class) } { (text) } }
}

fn zone_name<'a>(g: &'a Guide, short: &'a str) -> &'a str {
    g.zones.get(short).map(String::as_str).unwrap_or(short)
}

/// The first requirement that fails, as the reason a step is "not yet".
fn blocker<'a>(step: &'a Step, flags: &Flags) -> Option<&'a str> {
    let f = effective(flags);
    step.requires
        .iter()
        .find(|c| holds(c, &f) == Some(false))
        .and_then(|c| c.why.as_deref())
}

fn check_mark(c: &Cond, flags: Option<&Flags>) -> Markup {
    if c.op == "note" {
        return html! { span class="mark info" aria-hidden="true" { "•" } };
    }
    let Some(f) = flags else {
        return html! { span class="mark info" aria-hidden="true" { "•" } };
    };
    match holds(c, &effective(f)) {
        Some(true) => html! { span class="mark yes" title="You have this" { "✓" } },
        Some(false) => html! { span class="mark no" title="You don't have this yet" { "✗" } },
        None => html! { span class="mark info" title="Paste this tier to check" { "?" } },
    }
}

fn step_card(g: &Guide, step: &Step, flags: Option<&Flags>, open: bool) -> Markup {
    let st = flags.map(|f| status(step, f));
    let state_class = match st {
        Some(Status::Done) => "is-done",
        Some(Status::Next) => "is-next",
        Some(Status::Locked) => "is-locked",
        Some(Status::Optional) => "is-optional",
        _ => "",
    };
    html! {
        details class={ "step " (state_class) } open[open] id=(step.id) {
            summary {
                @if let Some(s) = st { (pill(s)) }
                span class="title" { (step.title) }
                @if let (Some(Status::Locked), Some(f)) = (st, flags) {
                    @if let Some(why) = blocker(step, f) {
                        span class="sub" { "First: " (why) }
                    }
                } @else if let Some(why) = &step.why {
                    span class="sub" { (why) }
                }
            }
            div class="body" {
                @if !step.npcs.is_empty() {
                    section {
                        h4 { "Where to go" }
                        ul class="npcs" {
                            @for n in &step.npcs {
                                li {
                                    b { (n.name) }
                                    " in " (zone_name(g, &n.zone))
                                    @if let Some(w) = &n.notes { span class="muted" { " — " (w) } }
                                    @if let Some(l) = &n.loc {
                                        " " span class="loc" title="Type /loc in game and walk until your numbers are close to these" { "/loc " (l) }
                                    }
                                }
                            }
                        }
                    }
                }
                @if !step.actions.is_empty() {
                    section {
                        h4 { "What to do" }
                        ol class="actions" {
                            @for a in &step.actions {
                                li {
                                    (a.detail)
                                    @if let Some(say) = &a.say {
                                        " " span class="say" { "Say " button type="button" class="copy" data-copy=(say) title="Copy" { (say) } }
                                    }
                                    @if !a.items.is_empty() {
                                        " " span class="items" {
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
                }
                @if let Some(m) = &step.message {
                    section { h4 { "You'll know it worked when" } p { (m) } }
                }
                @if !step.requires.is_empty() {
                    section {
                        h4 { "Before you start" }
                        ul class="needs" {
                            @for c in &step.requires {
                                @if let Some(text) = &c.why {
                                    li { (check_mark(c, flags)) " " (text) }
                                }
                            }
                        }
                    }
                }
                @if let Some(c) = &step.credit {
                    section { h4 { "Who gets credit" } p { (c) } }
                }
                @if !step.debug.is_empty() {
                    details class="debug" {
                        summary { "Didn't work? Check these" }
                        ul { @for d in &step.debug { li { (d) } } }
                    }
                }
                @if let Some(n) = &step.notes { p class="tip" { b { "Tip: " } (n) } }
            }
        }
    }
}

const FLAGS_CSS: &str = r#"
.flags{max-width:980px;margin:0 auto}
.flags h1{font:600 40px/1.05 "Cormorant Garamond",Georgia,serif;margin:6px 0 6px;text-wrap:balance}
.flags .lede{font-size:18px;color:var(--muted);margin:0 0 22px;max-width:62ch}
.flags h2{font:600 28px/1.1 "Cormorant Garamond",Georgia,serif;margin:0}
.flags h4{margin:0 0 4px;font-size:12px;text-transform:uppercase;letter-spacing:.08em;color:var(--muted)}
.flags .card{background:var(--surface);border:1px solid var(--line);border-radius:10px;padding:16px 18px}
.flags .how{display:grid;grid-template-columns:repeat(auto-fit,minmax(240px,1fr));gap:12px;margin-bottom:14px}
.flags .how .n{display:inline-grid;place-items:center;width:26px;height:26px;border-radius:50%;background:var(--accent);color:var(--accent-ink);font-weight:700;margin-right:8px}
.flags .how h3{margin:0 0 8px;font-size:16px;display:flex;align-items:center}
.flags .how p{margin:6px 0 0;color:var(--muted);font-size:14px}
.flags .cmds{display:flex;flex-wrap:wrap;gap:6px;margin-top:8px}
.flags button.copy{font:13px/1.2 ui-monospace,Consolas,monospace;background:var(--surface-2);color:var(--text);border:1px solid var(--line-strong);border-radius:6px;padding:4px 8px;cursor:pointer}
.flags button.copy:hover{border-color:var(--accent)}
.flags button.copy.done{border-color:var(--good);color:var(--good)}
.flags form.paste{display:grid;gap:10px}
.flags .drop{border:2px dashed var(--line-strong);border-radius:10px;padding:18px;text-align:center;color:var(--muted);cursor:pointer}
.flags .drop.over{border-color:var(--accent);color:var(--text)}
.flags .drop b{color:var(--text)}
.flags form.paste textarea{width:100%;min-height:110px;font:13px/1.4 ui-monospace,Consolas,monospace;background:var(--surface-2);color:var(--text);border:1px solid var(--line-strong);border-radius:8px;padding:8px}
.flags form.paste input{font:inherit;background:var(--surface-2);color:var(--text);border:1px solid var(--line-strong);border-radius:6px;padding:7px 9px;width:200px}
.flags .row{display:flex;gap:10px;align-items:center;flex-wrap:wrap}
.flags button.primary{font:inherit;font-weight:700;cursor:pointer;border-radius:8px;padding:8px 18px;border:1px solid var(--accent);background:var(--accent);color:var(--accent-ink)}
.flags button.ghost{font:inherit;cursor:pointer;border-radius:8px;padding:8px 14px;background:transparent;color:var(--muted);border:1px solid var(--line-strong)}
.flags #msg{min-height:1.4em;color:var(--muted)}
.flags #msg.err{color:var(--low)}
.flags details.paste-more summary{cursor:pointer;color:var(--muted);font-size:14px}
.flags .chars{display:flex;gap:6px;flex-wrap:wrap;margin:18px 0 6px}
.flags .chars a{padding:6px 12px;border:1px solid var(--line);border-radius:999px;text-decoration:none;color:var(--text);background:var(--surface)}
.flags .chars a[aria-current="page"]{border-color:var(--accent);font-weight:700}
.flags .nextup{border:2px solid var(--accent);border-radius:12px;padding:16px 18px;margin:14px 0 8px;background:var(--surface)}
.flags .nextup .eyebrow{font-size:12px;text-transform:uppercase;letter-spacing:.08em;color:var(--accent);font-weight:700}
.flags .nextup h2{margin:4px 0 6px}
.flags .nextup p{margin:0 0 10px;color:var(--muted)}
.flags .nextup ul{margin:8px 0 0;padding-left:18px;display:grid;gap:4px}
.flags .journey{display:grid;gap:10px;margin:18px 0 28px}
.flags .journey .tierrow{display:grid;grid-template-columns:120px 1fr;gap:10px;align-items:start}
.flags .journey .tl{font-weight:700;padding-top:6px}
.flags .journey .zones{display:flex;flex-wrap:wrap;gap:6px}
.flags .zchip{display:inline-flex;gap:6px;align-items:center;padding:6px 10px;border-radius:8px;border:1px solid var(--line);background:var(--surface);text-decoration:none;color:var(--text);font-size:14px}
.flags .zchip .bar{width:46px;height:6px;border-radius:3px;background:var(--surface-2);overflow:hidden}
.flags .zchip .bar i{display:block;height:100%;background:var(--good)}
.flags .zchip.done{border-color:var(--good)}
.flags .zchip.now{border-color:var(--accent)}
.flags .zchip.later{color:var(--muted)}
.flags .tier{margin:34px 0 4px;display:flex;gap:12px;align-items:baseline;flex-wrap:wrap}
.flags .tier .blurb{color:var(--muted)}
.flags h3.plane{margin:22px 0 8px;font-size:17px}
.flags .pill{display:inline-block;font-size:12px;font-weight:700;padding:2px 9px;border-radius:999px;border:1px solid var(--line-strong);color:var(--muted);white-space:nowrap}
.flags .pill.good{color:var(--good);border-color:var(--good)}
.flags .pill.next{color:var(--accent-ink);background:var(--accent);border-color:var(--accent)}
.flags .pill.low{color:var(--low);border-color:var(--low)}
.flags details.step{border:1px solid var(--line);border-radius:10px;background:var(--surface);margin:8px 0}
.flags details.step.is-next{border:2px solid var(--accent)}
.flags details.step.is-done{opacity:.8}
.flags details.step > summary{cursor:pointer;padding:12px 14px;display:grid;grid-template-columns:auto 1fr;column-gap:10px;row-gap:2px;align-items:center;list-style:none}
.flags details.step > summary::-webkit-details-marker{display:none}
.flags details.step > summary .title{font-weight:700}
.flags details.step.is-done > summary .title{font-weight:400;color:var(--muted)}
.flags details.step > summary .sub{grid-column:2;color:var(--muted);font-size:14px}
.flags details.step .body{padding:2px 16px 16px;display:grid;gap:14px;border-top:1px solid var(--line)}
.flags details.step .body section:first-child{margin-top:12px}
.flags details.step .body p{margin:0}
.flags ol.actions{margin:0;padding-left:22px;display:grid;gap:6px}
.flags ul.npcs,.flags ul.needs{margin:0;padding:0;list-style:none;display:grid;gap:5px}
.flags .loc{font:12px/1.2 ui-monospace,Consolas,monospace;color:var(--muted);white-space:nowrap;border:1px dotted var(--line-strong);border-radius:4px;padding:1px 5px}
.flags .say{white-space:nowrap}
.flags .mark{display:inline-block;width:1.2em;text-align:center;font-weight:700}
.flags .mark.yes{color:var(--good)}
.flags .mark.no{color:var(--low)}
.flags .mark.info{color:var(--muted)}
.flags details.debug{background:var(--surface-2);border-radius:8px;padding:8px 12px}
.flags details.debug summary{cursor:pointer;font-weight:700}
.flags details.debug ul{margin:8px 0 2px;padding-left:20px;display:grid;gap:4px}
.flags .tip{color:var(--muted)}
.flags .muted{color:var(--muted)}
.flags .glossary{margin:10px 0 0}
.flags .glossary summary{cursor:pointer;color:var(--muted)}
.flags .glossary dl{display:grid;grid-template-columns:max-content 1fr;gap:6px 14px;margin:10px 0 0}
.flags .glossary dt{font-weight:700}
.flags .glossary dd{margin:0;color:var(--muted)}
.flags table.guild{border-collapse:collapse;margin-top:8px}
.flags table.guild th,.flags table.guild td{padding:6px 12px;border-bottom:1px solid var(--line);text-align:left;white-space:nowrap}
.flags .wrap{overflow-x:auto}
.flags .foot{color:var(--muted);font-size:13px;margin-top:30px}
@media (max-width:600px){.flags .journey .tierrow{grid-template-columns:1fr}.flags h1{font-size:32px}}
"#;

const FLAGS_JS: &str = r#"
(function(){
  // Copy buttons: commands and the exact words to say.
  document.addEventListener('click', async e=>{
    const b=e.target.closest('button.copy'); if(!b) return;
    e.preventDefault();
    try{ await navigator.clipboard.writeText(b.dataset.copy); b.classList.add('done'); setTimeout(()=>b.classList.remove('done'),1200); }catch(_){}
  });
  const f=document.getElementById('paste'); if(!f) return;
  const msg=document.getElementById('msg'), text=f.text, drop=document.getElementById('drop'), file=document.getElementById('file');
  const say=(t,err)=>{ msg.textContent=t; msg.classList.toggle('err',!!err); };
  // Only the #popflags lines leave the browser: never chat, tells or anything else.
  const keep=/^(=== .+ ===|--- .+ ---|Pending memory: .+|[A-Z][A-Za-z'`, ]{2,60}: .+)$/;
  const chat=/ (tells|says|shouts|auctions|told)\b|, '|^You /;
  function flagLines(raw){
    const out=[];
    for(const line of raw.split(/\r?\n/)){
      const t=line.replace(/^\[[^\]]*\]\s*/,'').trim();
      if(keep.test(t) && !chat.test(t)) out.push(t);
    }
    return out.join('\n');
  }
  async function readLog(fl){
    const m=fl.name.match(/^eqlog_([A-Za-z]+)_/);
    if(m && !f.character.value) f.character.value=m[1];
    const tail=fl.size>8e6 ? fl.slice(fl.size-8e6) : fl;  // the last part of a long log is enough
    const lines=flagLines(await tail.text());
    if(!lines){ say('No #popflags lines in that file. Type #popflags 1 to 5 in game first, then drop the log again.',true); return; }
    text.value=lines;
    say('Found your flag lines'+(m?' for '+m[1]:'')+'. Press Save.');
  }
  drop.addEventListener('click',()=>file.click());
  drop.addEventListener('keydown',e=>{ if(e.key==='Enter'||e.key===' '){ e.preventDefault(); file.click(); } });
  file.addEventListener('change',()=>{ if(file.files[0]) readLog(file.files[0]); });
  ['dragenter','dragover'].forEach(ev=>drop.addEventListener(ev,e=>{e.preventDefault();drop.classList.add('over');}));
  ['dragleave','drop'].forEach(ev=>drop.addEventListener(ev,e=>{e.preventDefault();drop.classList.remove('over');}));
  drop.addEventListener('drop',e=>{ const fl=e.dataTransfer.files[0]; if(fl) readLog(fl); });
  async function send(path,payload){
    const r=await fetch(path,{method:'POST',credentials:'include',headers:{'content-type':'application/json','x-nocturnal':'1'},body:JSON.stringify(payload)});
    let j={}; try{ j=await r.json(); }catch(_){}
    return [r.ok&&j.ok,j];
  }
  f.addEventListener('submit',async e=>{
    e.preventDefault();
    if(!f.character.value.trim()){ say('Type your character name first.',true); f.character.focus(); return; }
    const body=flagLines(text.value) || text.value;
    say('Saving…');
    const [ok,j]=await send('/flags/save',{character:f.character.value,text:body});
    if(!ok){ say(j.message||'That did not save. Try again in a moment.',true); return; }
    const n=(j.unrecognised||[]).length;
    say('Saved '+j.character+'.'+(n?' A few lines looked new to us; an officer will check them.':''));
    setTimeout(()=>{ location.href='/flags?c='+encodeURIComponent(j.character)+'#next'; }, 700);
  });
  const del=document.getElementById('delete');
  if(del) del.addEventListener('click',async ()=>{
    if(!confirm('Remove '+del.dataset.c+' from this page? You can paste again any time.')) return;
    const [ok,j]=await send('/flags/delete',{character:del.dataset.c});
    if(ok) location.href='/flags'; else say(j.message||'That did not work.',true);
  });
})();
"#;

/// Steps grouped by tier, then plane, in the guide's order.
type Grouped<'a> = Vec<(
    u8,
    &'static str,
    &'static str,
    Vec<(&'a str, Vec<&'a Step>)>,
)>;

fn group(g: &Guide) -> Grouped<'_> {
    TIERS
        .iter()
        .filter_map(|(t, label, blurb)| {
            let mut planes: Vec<(&str, Vec<&Step>)> = Vec::new();
            for s in g.steps.iter().filter(|s| s.tier == *t) {
                match planes.iter_mut().find(|(p, _)| *p == s.plane) {
                    Some((_, v)) => v.push(s),
                    None => planes.push((s.plane.as_str(), vec![s])),
                }
            }
            (!planes.is_empty()).then_some((*t, *label, *blurb, planes))
        })
        .collect()
}

fn anchor(plane: &str) -> String {
    plane
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

/// One chip per plane: how many of its required steps are done.
fn journey(grouped: &Grouped<'_>, flags: &Flags) -> Markup {
    html! {
        div class="journey" {
            @for (_, label, _, planes) in grouped {
                div class="tierrow" {
                    div class="tl" { (label) }
                    div class="zones" {
                        @for (plane, steps) in planes.iter().filter(|(_, v)| v.iter().any(|s| !s.optional)) {
                            @let required: Vec<&&Step> = steps.iter().filter(|s| !s.optional).collect();
                            @let done = required.iter().filter(|s| status(s, flags) == Status::Done).count();
                            @let next = required.iter().any(|s| status(s, flags) == Status::Next);
                            @let total = required.len().max(1);
                            @let class = if done == required.len() && !required.is_empty() { "done" } else if next { "now" } else { "later" };
                            a class={ "zchip " (class) } href={ "#" (anchor(plane)) } {
                                span { (plane) }
                                span class="bar" aria-hidden="true" { i style={ "width:" (done * 100 / total) "%" } {} }
                                span class="muted" { (done) "/" (required.len()) }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn glossary() -> Markup {
    html! {
        details class="glossary" {
            summary { "New to flagging? What the words mean" }
            dl {
                dt { "Flag" } dd { "The game's record that you did a step. Flags open the doors and portals to the next planes. They belong to one character." }
                dt { "Tier" } dd { "A group of planes that open together. You finish tier 1 to reach tier 2, and so on." }
                dt { "Hail" } dd { "Target the NPC and press H, or type /say Hail." }
                dt { "Say" } dd { "Type the words in /say while the NPC is targeted. Use the copy buttons so the spelling is exact." }
                dt { "Hand in" } dd { "Open a trade with the NPC by dragging the item onto them, then press Give." }
                dt { "/loc" } dd { "Type /loc in game to see where you stand. Walk until your numbers are close to the ones shown here." }
                dt { "The Seer" } dd { "Seer Mal Nae`Shi in the Plane of Knowledge. Sit near her and say what the step tells you." }
                dt { "Saved memory" } dd { "If you kill a boss before you had the earlier step, the game remembers it. Visit the Seer later and say \"unlock memories\" to turn it into the flag." }
            }
        }
    }
}

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
    let next: Vec<&Step> = flags
        .map(|f| {
            g.steps
                .iter()
                .filter(|s| status(s, f) == Status::Next)
                .collect()
        })
        .unwrap_or_default();
    let missing_tiers: Vec<u8> = selected
        .map(|(_, c)| (1..=5).filter(|t| !c.sections.contains(t)).collect())
        .unwrap_or_default();
    let body = html! {
        style { (PreEscaped(FLAGS_CSS)) }
        div class="flags" {
            h1 { "Planes of Power flags" }
            p class="lede" { "See what your character has done, and exactly what to do next: who to talk to, where they stand and what to say." }

            div class="how" {
                div class="card" {
                    h3 { span class="n" { "1" } "Ask the game" }
                    p { "Log in to the character and type each of these. Click one to copy it." }
                    div class="cmds" {
                        @for t in 1..=5 { button type="button" class="copy" data-copy={ "#popflags " (t) } { "#popflags " (t) } }
                    }
                }
                div class="card" {
                    h3 { span class="n" { "2" } "Bring the answer here" }
                    p { "The game writes everything to your log file. Turn logging on once with " button type="button" class="copy" data-copy="/log on" { "/log on" } " before step 1." }
                    p { "The log is in your EverQuest folder, in Logs, named like eqlog_YourName_….txt." }
                }
                div class="card" {
                    h3 { span class="n" { "3" } "Follow the next step" }
                    p { "The page marks what you've done and opens the next thing to do. Come back and update after every kill or hand-in." }
                }
            }

            form id="paste" class="paste card" {
                div id="drop" class="drop" tabindex="0" role="button" aria-label="Choose your log file" {
                    b { "Drop your log file here" } " or click to choose it"
                    br;
                    span class="muted" { "Only the flag lines are sent. Your chat stays on your computer." }
                }
                input type="file" id="file" accept=".txt,text/plain" hidden;
                details class="paste-more" {
                    summary { "Or paste the lines yourself" }
                    textarea name="text" placeholder="=== Tier 1 Progression ===\nMavuin's case: Not started\n…" {}
                }
                div class="row" {
                    label for="character" { "Character" }
                    input id="character" name="character" placeholder="Your character's name" autocomplete="off"
                        value=(selected.map(|(n, _)| n).unwrap_or("")) list="mychars";
                    @if let Some(m) = mine { datalist id="mychars" { @for n in m.characters.keys() { option value=(n) {} } } }
                    button type="submit" class="primary" { "Save" }
                    @if let Some((n, _)) = selected { button type="button" class="ghost" id="delete" data-c=(n) { "Remove " (n) } }
                }
                div id="msg" aria-live="polite" {}
            }
            (glossary())

            @if let Some(m) = mine {
                @if m.characters.len() > 1 {
                    div class="chars" role="navigation" aria-label="Your characters" {
                        @for n in m.characters.keys() {
                            a href={ "/flags?c=" (n) } aria-current=[(selected.map(|(s, _)| s) == Some(n.as_str())).then_some("page")] { (n) }
                        }
                    }
                }
            }

            @if let (Some((name, c)), Some(f)) = (selected, flags) {
                div class="nextup" id="next" {
                    @if let Some(first) = next.first() {
                        div class="eyebrow" { "Next for " (name) }
                        h2 { a href={ "#" (first.id) } { (first.title) } }
                        @if let Some(w) = &first.why { p { (w) } }
                        @if next.len() > 1 {
                            b { "You can also work on" }
                            ul { @for s in next.iter().skip(1) { li { a href={ "#" (s.id) } { (s.title) } span class="muted" { " · " (s.plane) } } } }
                        }
                    } @else if tier_complete(5, f) == Some(true) {
                        div class="eyebrow" { (name) }
                        h2 { "Every flag is done. The Plane of Time is open." }
                    } @else {
                        div class="eyebrow" { (name) }
                        h2 { "Paste the rest of your tiers" }
                        p { "Nothing is open yet in the tiers you pasted. Type the other #popflags numbers and add them." }
                    }
                    @if !missing_tiers.is_empty() {
                        p class="muted" {
                            "Not pasted yet: " (missing_tiers.iter().map(|t| format!("#popflags {t}")).collect::<Vec<_>>().join(", "))
                            ". Steps there show as Unknown."
                        }
                    }
                    p class="muted" { "Updated " (c.updated_at.get(..10).unwrap_or(&c.updated_at)) "." }
                }
                (journey(&grouped, f))
            } @else {
                p class="muted" { "No character saved yet, so below is the whole guide without your progress. Save a character to see your next step." }
            }

            @for (t, label, blurb, planes) in &grouped {
                @let all: Vec<&Step> = planes.iter().flat_map(|(_, v)| v.iter().copied()).collect();
                @let started = flags.is_some_and(|f| all.iter().any(|s| status(s, f) == Status::Done));
                div class="tier" {
                    h2 { (label) }
                    @if let Some(f) = flags {
                        @match (tier_complete(*t, f), started) {
                            (Some(true), _) => { span class="pill good" { "Complete" } }
                            (_, true) => { span class="pill" { "In progress" } }
                            _ => { span class="pill" { "Not started" } }
                        }
                    }
                    span class="blurb" { (blurb) }
                }
                @for (plane, steps) in planes {
                    h3 class="plane" id=(anchor(plane)) { (plane) }
                    @for s in steps {
                        @let st = flags.map(|f| status(s, f));
                        (step_card(g, s, flags, st == Some(Status::Next) && next.first().is_some_and(|n| n.id == s.id)))
                    }
                }
            }

            (guild_table(store))
            p class="foot" {
                "Built from Project Quarm's own quests, checked October 2026. Positions are where the NPC stands when the zone starts; some walk around or only appear during an event."
            }
        }
        script { (PreEscaped(FLAGS_JS)) }
    };
    super::pages::layout("Flags", "flags", body, false)
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
    let cell = |t: u8, c: &Character| -> Markup {
        match (tier_complete(t, &c.flags), c.sections.contains(&t)) {
            (Some(true), _) => html! { span class="pill good" { "Done" } },
            (_, true) => html! { span class="pill" { "Working" } },
            _ => html! { span class="muted" { "–" } },
        }
    };
    html! {
        @if !rows.is_empty() {
            div class="tier" { h2 { "The guild" } span class="blurb" { "Everyone who has saved a character here." } }
            div class="wrap" {
                table class="guild" {
                    thead { tr { th { "Character" } th { "Member" } th { "Tier 1" } th { "Tier 2" } th { "Tier 3" } th { "Time" } th { "Updated" } } }
                    tbody {
                        @for (n, m, c) in rows {
                            tr {
                                td { (n) }
                                td { (m) }
                                @for t in [1u8, 2, 3, 5] { td { (cell(t, c)) } }
                                td class="muted" { (c.updated_at.get(..10).unwrap_or(&c.updated_at)) }
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
            why: None,
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
        assert!(
            !html.contains("github.com"),
            "no script references on the page"
        );
        assert!(html.contains(r#"href="/flags" aria-current="page""#));
        assert!(html.contains("The guild"));
        let anon = page(&Store::default(), None, None);
        assert!(anon.contains("whole guide"));
    }
}
