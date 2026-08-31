//! The pages. Maud templates over `SiteData`: what the JavaScript site drew
//! from `site.json`, now rendered by the bot from the typed snapshot. The
//! visual design is the same stylesheet; charts are placeholders the Perses
//! island fills.

use maud::{html, Markup, PreEscaped, DOCTYPE};

use crate::profiles::Profile;
use crate::site::{ItemView, RaidView, SiteData};

const CSS: &str = include_str!("site.css");

/// Tooltips, the item dialog, the roster filter and sign-out: the little
/// behaviour every page shares, in one place, with no data of its own.
const PAGE_JS: &str = r#"
const tip=document.createElement('div');tip.id='tip';tip.hidden=true;document.body.appendChild(tip);
function placeTip(x,y){const w=tip.offsetWidth,h=tip.offsetHeight;tip.style.left=Math.min(x+14,innerWidth-w-8)+'px';tip.style.top=(y+18+h>innerHeight?y-h-8:y+18)+'px';}
document.addEventListener('mouseover',e=>{const el=e.target.closest('[data-tip]');if(!el)return;tip.textContent=el.dataset.tip;tip.hidden=false;placeTip(e.clientX,e.clientY);});
document.addEventListener('mousemove',e=>{if(!tip.hidden)placeTip(e.clientX,e.clientY);});
document.addEventListener('mouseout',e=>{if(e.target.closest&&e.target.closest('[data-tip]'))tip.hidden=true;});
const rq=document.getElementById('rq');if(rq){rq.addEventListener('input',()=>{const q=rq.value.trim().toLowerCase();document.querySelectorAll('#rt tbody tr:not(.counts)').forEach(tr=>{tr.style.display=!q||tr.textContent.toLowerCase().includes(q)?'':'none';});});}
const so=document.getElementById('signout');if(so)so.addEventListener('click',async()=>{try{await fetch('/perses/api/auth/logout',{credentials:'include'});}catch(e){}location.href='/';});
fetch('/perses/api/v1/user/whoami').then(r=>r.ok?r.json():null).then(u=>{const w=document.getElementById('who');if(!w)return;const n=u&&u.metadata&&u.metadata.name;w.innerHTML=n?('signed in as <b>'+n.replace(/[&<>]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;'}[c]))+'</b> · <button class="namelink" id="signout">sign out</button>'):'not signed in';const so=document.getElementById('signout');if(so)so.addEventListener('click',async()=>{try{await fetch('/perses/api/auth/logout',{credentials:'include'});}catch(e){}location.href='/';});}).catch(()=>{});
"#;

/// The island's version, for cache-busting its URLs: the checksum the puller
/// wrote beside the assets directory, else the bot's own version. Assets are
/// cached for a day by URL, so a new island must be a new URL.
fn island_version() -> String {
    static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        crate::web::ASSETS_DIR
            .get()
            .and_then(|d| d.parent().map(|p| p.join(".sha256")))
            .and_then(|f| std::fs::read_to_string(f).ok())
            .map(|s| s.trim().chars().take(12).collect())
            .filter(|s: &String| !s.is_empty())
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned())
    })
    .clone()
}

fn layout_full(title: &str, current: &str, body: Markup, island: bool, wide: bool) -> String {
    let v = island_version();
    let doc = html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " · Nocturnal" }
                link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Cormorant+Garamond:wght@500;600&family=Atkinson+Hyperlegible:ital,wght@0,400;0,700;1,400&display=swap";
                style { (PreEscaped(CSS)) }
                @if island { link rel="stylesheet" href={ "/assets/island.css?v=" (v) }; }
            }
            body {
                nav { div class="in" {
                    a class="brand" href="/" { "Nocturnal" }
                    a class="tab" href="/" aria-current=[(current == "raid").then_some("page")] { "Raid night" }
                    a class="tab" href="/me" aria-current=[(current == "me").then_some("page")] { "Me" }
                    a class="tab" href="/roster" aria-current=[(current == "roster").then_some("page")] { "Roster" }
                    a class="tab" href="/loot" aria-current=[(current == "loot").then_some("page")] { "Loot" }
                    a class="tab" href="/perses/" title="The full Perses dashboards" { "Dashboards ↗" }
                    span class="who" id="who" { "…" }
                } }
                main id="main" class=[wide.then_some("wide")] { (body) }
                script { (PreEscaped(PAGE_JS)) }
                @if island { script type="module" src={ "/assets/island.js?v=" (v) } {} }
            }
        }
    };
    doc.into_string()
}

/// Every page is wide now; prose constrains itself with .read.
fn layout(title: &str, current: &str, body: Markup, island: bool) -> String {
    layout_full(title, current, body, island, true)
}

// --- small helpers -------------------------------------------------------------------------

fn day(ms: i64) -> String {
    let (y, m, d) = civil(ms);
    const M: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let dow = [
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
    ][(ms.div_euclid(86_400_000)).rem_euclid(7) as usize];
    format!(
        "{dow} {d} {} {y}",
        M[(m as usize).saturating_sub(1).min(11)]
    )
}

fn hm(ms: i64) -> String {
    let s = ms.div_euclid(1000).rem_euclid(86_400);
    format!("{:02}:{:02}", s / 3600, (s % 3600) / 60)
}

fn civil(ms: i64) -> (i64, u32, u32) {
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

fn ago(now: i64, ms: i64) -> String {
    if ms <= 0 {
        return "never".into();
    }
    let d = (now - ms).div_euclid(86_400_000);
    match d {
        i64::MIN..=0 => "today".into(),
        1 => "yesterday".into(),
        2..=13 => format!("{d} days ago"),
        14..=59 => format!("{} weeks ago", d / 7),
        60..=364 => format!("{} months ago", d / 30),
        _ => format!("{} years ago", d / 365),
    }
}

fn fmt(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

fn enc(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

fn name_link(name: &str) -> Markup {
    html! { a class="namelink" href={ "/who/" (enc(name)) } { (name) } }
}

fn item_link(data: &SiteData, name: &str) -> Markup {
    let tip = data
        .items
        .get(name)
        .and_then(|i| i.data.clone())
        .unwrap_or_default();
    html! { a class="item" href={ "/item/" (enc(name)) } data-tip=[(!tip.is_empty()).then_some(tip)] { (name) } }
}

fn discord_box(what: &str) -> Markup {
    html! { div class="discord" { (what) " — that happens in Discord, not here. This site only looks." } }
}

/// A Perses panel placeholder for the island, which supplies the datasource
/// and the time range.
#[allow(clippy::too_many_arguments)]
fn panel(
    title: &str,
    kind: &str,
    query: &str,
    series_name: &str,
    start_ms: i64,
    end_ms: i64,
    height: u32,
    island: bool,
) -> Markup {
    // Each panel plugin validates its spec; an empty one is not "defaults".
    let spec = match kind {
        "BarChart" => serde_json::json!({
            "calculation": "last-number",
            "format": { "unit": "decimal", "shortValues": true },
            "sort": "desc",
            "mode": "value"
        }),
        "StatChart" => serde_json::json!({
            "calculation": "last-number",
            "format": { "unit": "decimal", "shortValues": true }
        }),
        _ => serde_json::json!({
            "legend": { "position": "right", "mode": "list" },
            "visual": { "lineWidth": 1.5, "areaOpacity": 0.05 }
        }),
    };
    let desc = serde_json::json!({
        "kind": kind, "title": title, "query": query, "seriesNameFormat": series_name,
        "start": start_ms, "end": end_ms, "height": height, "spec": spec,
    })
    .to_string();
    html! {
        section class="panel" {
            div class="ph" { b { (title) } span { "Perses · everquest" } }
            @if island {
                div class="pb" data-panel=(desc) style={ "min-height:" (height) "px" } { p class="empty" { "Loading from Perses…" } }
            } @else {
                div class="pb" { p class="empty" { "The Perses island is not installed on this server, so this chart cannot render." } }
            }
        }
    }
}

// --- pages ----------------------------------------------------------------------------------

pub fn not_ready() -> String {
    layout(
        "Warming up",
        "raid",
        html! {
            div class="read" { h1 { "Warming up" } p class="lede" { "The bot is replaying the ledger; the site renders the moment it is done. Reload in a few seconds." } }
        },
        false,
    )
}

pub fn me_redirect() -> String {
    layout(
        "Me",
        "me",
        html! {
            div class="read" { p class="lede" id="me-note" { "Finding you…" } }
            script { (PreEscaped(r#"fetch('/perses/api/v1/user/whoami').then(r=>r.ok?r.json():null).then(u=>{const n=u&&u.metadata&&u.metadata.name;if(n){location.replace('/member/'+encodeURIComponent(n));}else{document.getElementById('me-note').innerHTML='You are not signed in. <a href="/perses/api/auth/providers/oauth/discord/login?rd=%2Fme">Sign in with Discord</a>.';}}).catch(()=>{document.getElementById('me-note').textContent='Could not ask Perses who you are.';});"#)) }
        },
        false,
    )
}

pub fn raid(data: &SiteData, id: Option<&str>, island: bool) -> String {
    let Some(r) = (match id {
        Some(id) => data.raids.iter().find(|r| r.id == id),
        None => data.raids.first(),
    }) else {
        return layout(
            "Raid night",
            "raid",
            html! { div class="read" { h1 { "No raids yet" } p class="lede" { "The ledger holds no raid to show." } } },
            false,
        );
    };
    let is_last = data.raids.first().is_some_and(|f| f.id == r.id);
    let end = if r.exact {
        r.end_ms
    } else {
        r.end_ms + 900_000
    };
    let dps_total = r.loot.iter().map(|l| l.cost).sum::<i64>();
    let body = html! {
        div class="read" {
            div class="eyebrow" { (if is_last { "Last raid" } else { "Raid night" }) " · " (day(r.date_ms)) }
            h1 { (r.name) }
            p class="lede" {
                (hm(r.start_ms)) " to " (hm(r.end_ms))
                @if !r.exact { " " span class="mut" { "(last tick — this raid predates exact end times)" } }
                ". " b { (r.attendees.len()) " came" } ", " (r.ticks) " ticks at " (r.dkp_per_tick) " DKP, " b { (r.loot.len()) " drops" } " for " (fmt(dps_total)) " DKP."
            }
            div class="raidpick" {
                @for x in &data.raids {
                    a href={ "/raid/" (enc(&x.id)) } { button type="button" aria-pressed=(x.id == r.id) { (x.name) span class="mut" { " · " (day(x.date_ms).split(' ').skip(1).take(2).collect::<Vec<_>>().join(" ")) } } }
                }
            }
        }
        div class="panels" {
            (panel("Damage, by character", "BarChart",
                &format!("sort_desc(sum by (everquest_combat_source) (increase(everquest_combat_damage_total{{everquest_combat_direction=\"outgoing\",everquest_combat_source_type=\"player\"}}[{}s])))", ((end - r.start_ms) / 1000).max(60)),
                "{{everquest_combat_source}}", r.start_ms, end, 320, island))
            (panel("DPS through the night", "TimeSeriesChart",
                "sum by (everquest_combat_source) (rate(everquest_combat_damage_total{everquest_combat_direction=\"outgoing\",everquest_combat_source_type=\"player\"}[2m]))",
                "{{everquest_combat_source}}", r.start_ms, end, 320, island))
        }
        div class="read" {
            p class="mut" style="font-size:13px" { "Only characters running the DPS meter show up — set yours up with " code { "/dpstoken" } " in Discord." }
        }
        div class="columns" {
            div class="col" {
                h2 { "What dropped" }
                @if r.loot.is_empty() { p class="empty" { "Nothing dropped — or nothing was bid on." } } @else {
                    div class="tablewrap" { table {
                        thead { tr { th { "Item" } th { "Went to" } th class="num" { "DKP" } th { "When" } } }
                        tbody { @for l in &r.loot { tr { td { (item_link(data, &l.item)) } td { (name_link(&l.winner)) } td class="num brassx" { (l.cost) } td class="mut" { (hm(l.ts_ms)) } } } }
                    } }
                }
            }
            div class="col" {
                @if is_last {
                    h2 { "Next raid" }
                    @if data.upcoming.is_empty() { p class="empty" { "Nothing scheduled in RaidHelper for the next two weeks." } } @else {
                        div class="next" { @for e in &data.upcoming { div class="ev" { span { b { (e.title) } small { (day(e.start_ms)) " · " (hm(e.start_ms)) } } span class="mut" { (e.signups) " signed up" } } } }
                    }
                }
                h2 { "Who came" }
                div class="chips" { @for a in &r.attendees { span class="chip" { (name_link(a)) } } }
            }
        }
        div class="read" {
            (discord_box("Want to bid, or start a raid?"))
        }
    };
    layout(&r.name, "raid", body, island)
}

fn characters_chips(data: &SiteData, chars: &[crate::site::CharacterView]) -> Markup {
    html! {
        div class="chips" { @for c in chars {
            @let key = c.name.to_lowercase();
            @let rank = match c.main { Some(nocturnal_core::MainRank::Main) => "M-", Some(nocturnal_core::MainRank::Second) => "M2-", None => "" };
            span class="chip" data-tip=[c.aa.map(|aa| format!("{aa} AA"))] {
                @if data.profiles.contains_key(&key) { a class="namelink" href={ "/char/" (enc(&c.name)) } { (c.name) " (" (rank) (c.level) ")" } }
                @else { (c.name) " (" (rank) (c.level) ")" }
                " " span class="mut" { (c.class) @if let Some(aa) = c.aa { " · " (aa) " AA" } }
            }
        } }
    }
}

pub fn member(data: &SiteData, login: &str, island: bool) -> String {
    let Some(m) = data.members.get(login) else {
        return layout(
            "Me",
            "me",
            html! {
                div class="read" { h1 { "Not on the ledger" } p class="lede" { "Perses knows you as " b { (login) } ", but that name is not on the DKP ledger as someone raiding in the last 90 days." } (discord_box("Think that is wrong?")) }
            },
            false,
        );
    };
    let now = data.generated_ms;
    let body = html! {
            div class="read" {
                div class="eyebrow" { "You" }
                h1 { (m.name) @if m.discord != m.name { " " span class="mut" style="font:400 15px/1 'Atkinson Hyperlegible',system-ui,sans-serif" { "· " (m.discord) " on Discord" } } }
                div class="nums" {
                    div class="brass" { b { (fmt(m.dkp)) } span { "DKP" } }
                    div { b { (format!("{:.2}", m.attendance)) "%" } span { "attendance, 90 days" } }
                    div { b { (m.raids_attended) span style="font-size:16px;font-weight:400;color:var(--muted)" { " / " (data.raids.len()) } } span { "of the last " (data.raids.len()) " raids" } }
                }
    }
        div class="columns" {
            div class="col" {
                h2 { "Characters" }
                @if m.characters.is_empty() { p class="empty" { "Nothing on the roster yet — " code { "/roster add" } " in Discord, or run the meter and zone." } }
                @else { (characters_chips(data, &m.characters)) }
                @if m.characters.iter().any(|c| data.profiles.contains_key(&c.name.to_lowercase())) { p class="mut" style="font-size:13px" { "Underlined characters have a gear profile from the meter — click one." } }
                @else { p class="mut" style="font-size:13px" { "No gear profile yet: with the meter running, " code { "/otlp profile" } " in game sends one, and every zone-in after that keeps it current." } }
            }
            div class="col" {
            div class="panels" {
                            (panel("Your damage, last 7 days", "TimeSeriesChart",
                    &format!("sum by (everquest_combat_source) (rate(everquest_combat_damage_total{{everquest_combat_direction=\"outgoing\",everquest_reporter=\"{}\"}}[5m]))", login.replace('"', "")),
                    "{{everquest_combat_source}}", now - 7 * 86_400_000, now, 300, island))
            }
                h2 { "Recent ledger" }
                @if m.history.is_empty() { p class="empty" { "Nothing yet." } } @else {
                    ul class="hist" { @for h in &m.history {
                        @let dkp = h["dkp"].as_i64().unwrap_or(0);
                        li {
                            span class={ "d " (if dkp < 0 { "neg" } else { "pos" }) } { (if dkp > 0 { "+" } else { "" }) (dkp) }
                            span {
                                @if h["kind"] == "raid" { (h["raid"].as_str().unwrap_or("Raid")) " " span class="mut" { "· " (h["ticks"].as_i64().unwrap_or(0)) " ticks" } }
                                @else if h["kind"] == "loot" { (item_link(data, h["item"].as_str().unwrap_or(""))) }
                                @else { (h["comment"].as_str().unwrap_or("")) }
                                small { @if h["kind"] != "raid" { @if let Some(rn) = h["raid"].as_str() { (rn) " · " } } (ago(now, h["ts_ms"].as_i64().unwrap_or(0))) }
                            }
                        }
                    } }
                }
            }
        }
        div class="read" {
            (discord_box("Need a meter token, or want to register a character?"))
        }
        };
    layout(&m.name, "me", body, island)
}

pub fn person(data: &SiteData, name: &str) -> String {
    let Some(p) = data.people.get(name) else {
        return layout(
            name,
            "roster",
            html! { div class="read" { h1 { (name) } p class="lede" { "Not on the roster." } } },
            false,
        );
    };
    let body = html! {
        div class="read" {
            div class="eyebrow" { "Member" }
            h1 { (name) @if let Some(d) = &p.discord { @if d != name { " " span class="mut" style="font:400 15px/1 'Atkinson Hyperlegible',system-ui,sans-serif" { "· " (d) " on Discord" } } } }
            h2 { "Characters" }
            @if p.characters.is_empty() { p class="empty" { "Nothing on the roster." } } @else { (characters_chips(data, &p.characters)) }
            (discord_box("Want to award or take DKP?"))
        }
    };
    layout(name, "roster", body, false)
}

const SLOT_ORDER: [&str; 22] = [
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
const CLASSES: [&str; 16] = [
    "",
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

fn gear_tip(it: &crate::items::ItemSummary) -> String {
    let mut parts: Vec<String> = Vec::new();
    if it.req_level > 0 {
        parts.push(format!("Req level {}", it.req_level));
    }
    let mut st: Vec<String> = Vec::new();
    for (n, v) in [("AC", it.ac), ("HP", it.hp), ("Mana", it.mana)] {
        if v != 0 {
            st.push(format!("{n}: {v}"));
        }
    }
    for (n, v) in ["STR", "STA", "AGI", "DEX", "WIS", "INT", "CHA"]
        .iter()
        .zip(it.stats.iter())
    {
        if *v != 0 {
            st.push(format!("{n}: {v}"));
        }
    }
    for (n, v) in ["MR", "FR", "CR", "DR", "PR"].iter().zip(it.resists.iter()) {
        if *v != 0 {
            st.push(format!("{n}: {v}"));
        }
    }
    if !st.is_empty() {
        parts.push(st.join("  "));
    }
    for (n, v) in [
        ("Click", &it.click),
        ("Focus", &it.focus),
        ("Worn", &it.worn),
        ("Proc", &it.proc_effect),
    ] {
        if let Some(v) = v {
            parts.push(format!("{n}: {v}"));
        }
    }
    parts.join("\n")
}

pub fn character(data: &SiteData, name: &str) -> String {
    let key = name.to_lowercase();
    let Some(p): Option<&Profile> = data.profiles.get(&key) else {
        return layout(
            name,
            "me",
            html! { div class="read" { h1 { "No profile" } p class="lede" { "Nothing has been reported for " b { (name) } ". With the meter running, " code { "/otlp profile" } " in game sends one." } } },
            false,
        );
    };
    let mut slots: Vec<&crate::profiles::Slot> = p.equipment.iter().collect();
    slots.sort_by_key(|s| SLOT_ORDER.iter().position(|x| *x == s.slot).unwrap_or(99));
    let gear_of = |id: i64| data.gear_items.get(&id.to_string());
    let (mut ac, mut hp, mut mana) = (0i64, 0i64, 0i64);
    for s in &slots {
        if let Some(it) = s.id.and_then(gear_of) {
            ac += it.ac;
            hp += it.hp;
            mana += it.mana;
        }
    }
    let class = CLASSES.get(p.class as usize).copied().unwrap_or("");
    let body = html! {
        div class="read" {
            div class="eyebrow" { "Character · reported " (ago(data.generated_ms, p.reported_ms)) }
            h1 { (p.name) " " span class="mut" style="font:400 16px/1 'Atkinson Hyperlegible',system-ui,sans-serif" { "· " (p.level) " " (class) @if !p.guild.is_empty() { " · " (p.guild) } } }
            div class="stats" {
                div { b { (fmt(ac)) } span { "AC from gear" } }
                div { b { (fmt(hp)) } span { "HP from gear" } }
                div { b { (fmt(mana)) } span { "Mana from gear" } }
                div { b { (p.aa.get("spent").copied().unwrap_or(0)) } span { "AA spent" } }
                div { b { (p.aa.get("unspent").copied().unwrap_or(0)) } span { "AA unspent" } }
            }
            div class="stats" { @for k in ["str", "sta", "agi", "dex", "wis", "int", "cha"] { @if let Some(v) = p.base_stats.get(k) { div { b { (v) } span { "base " (k) } } } } }
        }
        div class="wide-block" {
            h2 { "Gear" }
            div class="gear" { @for s in &slots {
                @match s.id.and_then(gear_of) {
                    Some(it) => div class="g" { div class="s" { (s.slot) } div class="n" { span class="item" data-tip=(gear_tip(it)) { (it.name) } } div class="st" { @if it.ac != 0 { "AC " (it.ac) " " } @if it.hp != 0 { "HP " (it.hp) " " } @if it.mana != 0 { "Mana " (it.mana) } } },
                    None => @if let Some(n) = &s.name { div class="g" { div class="s" { (s.slot) } div class="n" { (n) } } } @else { div class="g empty" { div class="s" { (s.slot) } div class="n mut" { "—" } } },
                }
            } }
        }
        div class="read" {
            p class="mut" style="font-size:13px" { "Item numbers are the item's own; totals are gear only, not the character's computed stats — those come later." }
            (discord_box("Want this updated? Zone, or type /otlp profile in game."))
        }
    };
    layout(&p.name, "me", body, false)
}

pub fn roster(data: &SiteData) -> String {
    // The matrix, from the same people the roster payload lists.
    let classes = nocturnal_core::CLASSES;
    let body = html! {
        div class="eyebrow" { "Who can we field" }
        h1 { "Roster" }
        p class="lede" { "The matrix the guild already uses, from the ledger." }
        input type="search" id="rq" placeholder="Filter by member or character" aria-label="Filter roster";
        div class="tablewrap matrix" { table id="rt" {
            thead { tr { th { "Member" } @for c in classes { th class="cls" { (c) } } th { "Discord" } } }
            tbody { @for (name, p) in &data.people { tr {
                td { (name_link(name)) }
                @for c in classes {
                    @let cs: Vec<&crate::site::CharacterView> = p.characters.iter().filter(|x| x.class == *c).collect();
                    @let cls = cs.first().map(|x| match (x.main, x.level) { (Some(_), _) => "cls m", (None, 60) => "cls l60", _ => "cls low" }).unwrap_or("");
                    td class=(cls) { @for (i, x) in cs.iter().enumerate() { @if i > 0 { ", " } (x.name) " (" (match x.main { Some(nocturnal_core::MainRank::Main) => "M-", Some(nocturnal_core::MainRank::Second) => "M2-", None => "" }) (x.level) ")" } }
                }
                td class="mut" { (p.discord.clone().unwrap_or_default()) }
            } } }
        } }
        (discord_box("Add or change a character?"))
    };
    layout_full("Roster", "roster", body, false, true)
}

pub fn loot(data: &SiteData) -> String {
    let mut all: Vec<(&RaidView, &crate::site::LootView)> = data
        .raids
        .iter()
        .flat_map(|r| r.loot.iter().map(move |l| (r, l)))
        .collect();
    all.sort_by_key(|(_, l)| std::cmp::Reverse(l.ts_ms));
    let body = html! {
        div class="eyebrow" { "What dropped lately" }
        h1 { "Loot" }
        p class="lede" { "Every drop from the last " (data.raids.len()) " raids and what it went for. It is a record, not a price guide — there is deliberately no \"what things usually cost\" here." }
        div class="tablewrap" { table {
            thead { tr { th { "Item" } th { "Went to" } th class="num" { "DKP" } th { "Raid" } } }
            tbody { @for (r, l) in &all { tr { td { (item_link(data, &l.item)) } td { (name_link(&l.winner)) } td class="num brassx" { (l.cost) } td class="mut" { (r.name) " · " (ago(data.generated_ms, r.date_ms)) } } } }
        } }
        (discord_box("Disputing a line? Officers use /searchlogs"))
    };
    layout_full("Loot", "loot", body, false, true)
}

pub fn item(data: &SiteData, name: &str) -> String {
    let it: Option<&ItemView> = data.items.get(name);
    let body = html! {
        div class="read" {
            div class="eyebrow" { "Item" }
            h1 { (name) @if let Some(u) = it.and_then(|i| i.url.as_ref()) { " " a href=(u) target="_blank" rel="noopener" style="font-size:13px;font-weight:400" { "pqdi ↗" } } }
            @if let Some(d) = it.and_then(|i| i.data.as_ref()) { pre style="white-space:pre-wrap;font:13px/1.4 inherit;color:var(--muted);margin:0 0 14px" { (d) } }
            h2 { "Every time it dropped" }
            @match it.map(|i| &i.history) {
                Some(h) if !h.is_empty() => div class="tablewrap" { table {
                    thead { tr { th { "Went to" } th class="num" { "DKP" } th { "Raid" } } }
                    tbody { @for a in h { tr { td { (name_link(&a.winner)) } td class="num brassx" { (a.cost) } td class="mut" { (a.raid) " · " (ago(data.generated_ms, a.ts_ms)) } } } }
                } },
                _ => p class="empty" { "Never charged in the ledger." },
            }
            p class="mut" style="font-size:13px;margin-top:12px" { "This is the ledger's own history for the item — what " code { "/searchlogs" } " would show an officer." }
        }
    };
    layout(name, "loot", body, false)
}
