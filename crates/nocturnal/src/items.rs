//! EQ item lookup — port of the legacy `search/` (audit #42/E12 fixed):
//! 5-second timeouts, URL-encoded queries, status checks, null-safe parsing,
//! and a permanent in-memory cache (EQ items never change).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context as _;
use nocturnal_core::Item;
use scraper::{Html, Selector};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Database {
    Quarm,
    Takp,
}

impl Database {
    pub fn parse(s: &str) -> Option<Database> {
        match s {
            "quarm" => Some(Database::Quarm),
            "takp" => Some(Database::Takp),
            _ => None,
        }
    }
}

/// A search hit before the detail fetch.
#[derive(Debug, Clone)]
pub struct ItemRef {
    pub id: String,
    pub name: String,
    pub kind: Option<String>,
}

#[derive(Debug)]
pub enum SearchOutcome {
    None,
    Many(Vec<ItemRef>),
    One(Item),
}

pub struct ItemSearch {
    client: reqwest::Client,
    cache: Mutex<HashMap<(Database, String), Item>>,
}

impl ItemSearch {
    pub fn new() -> anyhow::Result<ItemSearch> {
        Ok(ItemSearch {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .user_agent("nocturnal-dkp-bot")
                .build()?,
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub async fn search(&self, query: &str, db: Database) -> anyhow::Result<SearchOutcome> {
        if query.chars().all(|c| c.is_ascii_digit()) && !query.is_empty() {
            return Ok(match self.by_id(query, db).await? {
                Some(item) => SearchOutcome::One(item),
                None => SearchOutcome::None,
            });
        }
        match db {
            Database::Quarm => self.quarm_search(query).await,
            Database::Takp => self.takp_search(query).await,
        }
    }

    pub async fn by_id(&self, id: &str, db: Database) -> anyhow::Result<Option<Item>> {
        let key = (db, id.to_owned());
        if let Ok(cache) = self.cache.lock() {
            if let Some(item) = cache.get(&key) {
                return Ok(Some(item.clone()));
            }
        }
        let item = match db {
            Database::Quarm => self.quarm_by_id(id).await?,
            Database::Takp => self.takp_by_id(id).await?,
        };
        if let (Some(item), Ok(mut cache)) = (&item, self.cache.lock()) {
            cache.insert(key, item.clone());
        }
        Ok(item)
    }

    async fn get_text(&self, url: &str) -> anyhow::Result<String> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .context("item site request")?;
        anyhow::ensure!(
            resp.status().is_success(),
            "item site returned {}",
            resp.status()
        );
        Ok(resp.text().await?)
    }

    // -- Quarm (pqdi.cc) -----------------------------------------------------

    async fn quarm_search(&self, query: &str) -> anyhow::Result<SearchOutcome> {
        let url = format!(
            "https://www.pqdi.cc/api/v1/items?name={}",
            urlencoding::encode(query)
        );
        let resp = self.client.get(&url).send().await.context("pqdi search")?;
        anyhow::ensure!(
            resp.status().is_success(),
            "pqdi returned {}",
            resp.status()
        );
        #[derive(serde::Deserialize)]
        struct SearchResp {
            #[serde(default)]
            items: Vec<SearchItem>,
        }
        #[derive(serde::Deserialize)]
        struct SearchItem {
            id: serde_json::Value,
            name: String,
        }
        let data: SearchResp = resp.json().await.context("pqdi search json")?;
        let refs: Vec<ItemRef> = data
            .items
            .into_iter()
            .map(|i| ItemRef {
                id: match i.id {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                },
                name: i.name,
                kind: None,
            })
            .collect();
        match refs.len() {
            0 => Ok(SearchOutcome::None),
            1 => Ok(match self.by_id(&refs[0].id, Database::Quarm).await? {
                Some(item) => SearchOutcome::One(item),
                None => SearchOutcome::None,
            }),
            _ => Ok(SearchOutcome::Many(refs)),
        }
    }

    async fn quarm_by_id(&self, id: &str) -> anyhow::Result<Option<Item>> {
        let html = self
            .get_text(&format!("https://www.pqdi.cc/get-item-tooltip/{id}"))
            .await?;
        if !html.contains("table") {
            return Ok(None);
        }
        let doc = Html::parse_fragment(&html);
        let name = select_text(&doc, "h4").unwrap_or_else(|| format!("Item {id}"));
        let table_sel = Selector::parse("table").expect("static selector");
        let stats = doc
            .select(&table_sel)
            .next()
            .map(|t| table_to_text(&t.html(), &name))
            .unwrap_or_default();
        let icon = doc
            .select(&Selector::parse(".item-icon").expect("static selector"))
            .next()
            .and_then(|e| e.value().attr("title"))
            .and_then(|t| t.split(' ').nth(1))
            .map(|n| format!("https://www.takproject.net/allaclone/icons/item_{n}.gif"));
        Ok(Some(Item {
            id: id.to_owned(),
            name,
            url: Some(format!("https://www.pqdi.cc/item/{id}")),
            data: Some(stats),
            image: icon,
        }))
    }

    // -- TAKP (takproject.net allaclone) ------------------------------------

    async fn takp_search(&self, query: &str) -> anyhow::Result<SearchOutcome> {
        let url = format!(
            "https://www.takproject.net/allaclone/items.php?iname={}&isearch=Search",
            urlencoding::encode(query)
        );
        let html = self.get_text(&url).await?;
        if html.contains("search-item-list") {
            let doc = Html::parse_document(&html);
            let row_sel = Selector::parse(".search-item-list table tr").expect("static selector");
            let td_sel = Selector::parse("td").expect("static selector");
            let mut refs = Vec::new();
            for row in doc.select(&row_sel) {
                let tds: Vec<String> = row
                    .select(&td_sel)
                    .map(|td| td.text().collect::<String>().trim().to_owned())
                    .collect();
                if tds.len() > 8 {
                    refs.push(ItemRef {
                        id: tds[8].clone(),
                        name: tds[1].clone(),
                        kind: Some(tds[2].clone()).filter(|s| !s.is_empty()),
                    });
                }
            }
            return Ok(match refs.len() {
                0 => SearchOutcome::None,
                _ => SearchOutcome::Many(refs),
            });
        }
        if html.contains("item-info") {
            // Single hit: the site redirected straight to the item page.
            if let Some(item) = self.takp_parse_item(&html, "", None) {
                return Ok(SearchOutcome::One(item));
            }
        }
        Ok(SearchOutcome::None)
    }

    async fn takp_by_id(&self, id: &str) -> anyhow::Result<Option<Item>> {
        let url = format!("https://www.takproject.net/allaclone/item.php?id={id}");
        let html = self.get_text(&url).await?;
        Ok(self.takp_parse_item(&html, id, Some(url)))
    }

    fn takp_parse_item(&self, html: &str, id: &str, url: Option<String>) -> Option<Item> {
        let doc = Html::parse_document(html);
        let name = select_text(&doc, ".item-info > strong")?;
        let stats_sel = Selector::parse(".item-stats").expect("static selector");
        let stats = doc
            .select(&stats_sel)
            .next()
            .map(|t| table_to_text(&t.html(), &name))
            .unwrap_or_default();
        let image = doc
            .select(&Selector::parse(".item-info img").expect("static selector"))
            .next()
            .and_then(|e| e.value().attr("src"))
            .map(|s| {
                if s.starts_with("http") {
                    s.to_owned()
                } else {
                    format!("https://www.takproject.net{s}")
                }
            });
        Some(Item {
            id: id.to_owned(),
            name,
            url,
            data: Some(stats),
            image,
        })
    }
}

fn select_text(doc: &Html, selector: &str) -> Option<String> {
    let sel = Selector::parse(selector).ok()?;
    doc.select(&sel)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// The legacy stats-block flattening: structural tags to newlines, all other
/// markup stripped, the item name removed, blank runs collapsed.
fn table_to_text(html: &str, name: &str) -> String {
    let mut text = html.replace(name, "");
    for tag in ["<br>", "<br/>", "<br />", "<p>", "</p>", "<tr>", "</tr>"] {
        text = text.replace(tag, "\n");
    }
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    // Tighten it. The scraped block arrives padded for a fixed-width web
    // table — leading spaces on some lines, runs of trailing spaces on
    // others, and blank rows between sections. In a Discord embed that is not
    // alignment, it is just a tall ragged block, and raiders reading it on a
    // phone mid-raid asked for it smaller. Every line is trimmed at both
    // ends, runs of spaces inside a line collapse to one, and blank lines go.
    let mut collapsed = String::with_capacity(out.len());
    for line in out.lines() {
        let mut words = line.split_whitespace().peekable();
        if words.peek().is_none() {
            continue;
        }
        let mut first = true;
        for word in words {
            if !first {
                collapsed.push(' ');
            }
            collapsed.push_str(word);
            first = false;
        }
        collapsed.push('\n');
    }
    collapsed.trim().to_owned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion
mod tests {
    use super::{table_to_text, Database, ItemSearch, SearchOutcome};

    #[test]
    fn stats_block_flattening_matches_legacy_shape() {
        let html =
            "<table><tr><td>Symbol of Veeshan</td></tr><tr><td>LORE ITEM<br>NO DROP</td></tr>\
                    <p>Slot: NONE</p><tr><td></td></tr><tr><td>WT: 5</td></tr></table>";
        let text = table_to_text(html, "Symbol of Veeshan");
        assert!(text.contains("LORE ITEM"), "{text:?}");
        assert!(text.contains("NO DROP"), "{text:?}");
        assert!(text.contains("Slot: NONE"), "{text:?}");
        assert!(text.contains("WT: 5"), "{text:?}");
        assert!(!text.contains('<'), "markup survived: {text:?}");
        assert!(
            !text.contains("Symbol of Veeshan"),
            "name not stripped: {text:?}"
        );
        assert!(!text.contains("\n\n"), "blank lines survived: {text:?}");
    }

    /// The scraped table is padded for a fixed-width web layout. None of that
    /// padding survives into the embed: raiders read this on a phone during a
    /// raid and asked for it tighter.
    #[test]
    fn the_web_tables_padding_does_not_reach_the_embed() {
        let html = "<table><tr><td>Cloak</td></tr><tr><td>  LORE ITEM   NO DROP      </td></tr>\
                    <tr><td>   </td></tr><tr><td>  WT: 5    Size: MEDIUM  </td></tr></table>";
        let text = table_to_text(html, "Cloak");
        assert_eq!(text, "LORE ITEM NO DROP\nWT: 5 Size: MEDIUM");
        for line in text.lines() {
            assert_eq!(line, line.trim(), "ragged line: {line:?}");
            assert!(!line.contains("  "), "space run survived: {line:?}");
        }
    }

    #[test]
    fn database_parsing() {
        assert_eq!(Database::parse("quarm"), Some(Database::Quarm));
        assert_eq!(Database::parse("takp"), Some(Database::Takp));
        assert_eq!(Database::parse("nope"), None);
    }

    /// Live check against pqdi.cc — network, so opt-in:
    /// `cargo test -p nocturnal -- --ignored live_quarm`
    #[tokio::test]
    #[ignore = "network"]
    async fn live_quarm_lookup() {
        let search = ItemSearch::new().unwrap();
        match search
            .search("Symbol of Veeshan", Database::Quarm)
            .await
            .unwrap()
        {
            SearchOutcome::One(item) => {
                assert!(item.name.to_lowercase().contains("veeshan"), "{item:?}");
                assert!(item.data.as_deref().is_some_and(|d| !d.is_empty()));
            }
            SearchOutcome::Many(refs) => assert!(!refs.is_empty()),
            SearchOutcome::None => panic!("no results for a known item"),
        }
    }
}

// ---------------------------------------------------------------------------
// The item mirror: pqdi's item rows, cached on disk forever.
// ---------------------------------------------------------------------------

/// The parts of an item row the site renders. pqdi's `/api/v1/item/{id}` is
/// the full EQEmu `items` table row (159 columns); this keeps the ones a gear
/// page shows and stores the whole row beside it, so a later page can show
/// more without refetching.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ItemSummary {
    pub id: i64,
    pub name: String,
    pub icon: i64,
    pub ac: i64,
    pub hp: i64,
    pub mana: i64,
    pub stats: [i64; 7],   // str sta agi dex wis int cha
    pub resists: [i64; 5], // mr fr cr dr pr
    pub slots: i64,
    pub classes: i64,
    pub races: i64,
    pub item_type: i64,
    pub req_level: i64,
    pub weight: i64,
    pub damage: i64,
    pub delay: i64,
    #[serde(default)]
    pub click: Option<String>,
    #[serde(default)]
    pub focus: Option<String>,
    #[serde(default)]
    pub worn: Option<String>,
    #[serde(default)]
    pub proc_effect: Option<String>,
    pub magic: bool,
    pub lore: bool,
    pub no_drop: bool,
}

fn num(v: &serde_json::Value) -> i64 {
    match v {
        serde_json::Value::Number(n) => n.as_i64().unwrap_or(0),
        serde_json::Value::String(s) => s.parse().unwrap_or(0),
        serde_json::Value::Bool(b) => i64::from(*b),
        _ => 0,
    }
}

fn effect(row: &serde_json::Value, name_key: &str, id_key: &str) -> Option<String> {
    let id = num(&row[id_key]);
    if id <= 0 {
        return None;
    }
    match row[name_key].as_str() {
        Some(n) if !n.trim().is_empty() => Some(n.to_owned()),
        _ => Some(format!("#{id}")),
    }
}

impl ItemSummary {
    /// From a pqdi row. Pure, and pinned by a test against a captured row.
    pub fn from_row(row: &serde_json::Value) -> Self {
        let n = |k: &str| num(&row[k]);
        ItemSummary {
            id: n("id"),
            name: row["Name"].as_str().unwrap_or("").to_owned(),
            icon: n("icon"),
            ac: n("ac"),
            hp: n("hp"),
            mana: n("mana"),
            stats: [
                n("astr"),
                n("asta"),
                n("aagi"),
                n("adex"),
                n("awis"),
                n("aint"),
                n("acha"),
            ],
            resists: [n("mr"), n("fr"), n("cr"), n("dr"), n("pr")],
            slots: n("slots"),
            classes: n("classes"),
            races: n("races"),
            item_type: n("itemtype"),
            req_level: n("reqlevel"),
            weight: n("weight"),
            damage: n("damage"),
            delay: n("delay"),
            click: effect(row, "clickname", "clickeffect"),
            focus: effect(row, "focusname", "focuseffect"),
            worn: effect(row, "wornname", "worneffect"),
            proc_effect: effect(row, "procname", "proceffect"),
            magic: n("magic") != 0,
            lore: n("lore") != 0 || row["lore"].as_str().is_some_and(|s| s.starts_with('*')),
            no_drop: n("nodrop") == 0,
        }
    }
}

/// Item rows by id, on disk under `<data>/items/<id>.json`, fetched from pqdi
/// once and never again: items do not change. Reads are synchronous file
/// reads; only a miss touches the network, and only from the bot, never
/// from a page.
pub struct ItemMirror {
    dir: std::path::PathBuf,
    client: reqwest::Client,
}

impl ItemMirror {
    pub fn new(data_dir: &std::path::Path) -> Self {
        ItemMirror {
            dir: data_dir.join("items"),
            client: reqwest::Client::builder()
                .user_agent("nocturnal-dkp-bot")
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }

    fn path(&self, id: i64) -> std::path::PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// The cached row, if any. Never fetches.
    pub fn cached(&self, id: i64) -> Option<serde_json::Value> {
        let text = std::fs::read_to_string(self.path(id)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// The row, fetching and caching on a miss. `None` when pqdi has no such
    /// item or cannot be reached — the caller renders "unknown item", the
    /// next render tries again.
    pub async fn get(&self, id: i64) -> Option<serde_json::Value> {
        if let Some(row) = self.cached(id) {
            return Some(row);
        }
        let url = format!("https://www.pqdi.cc/api/v1/item/{id}");
        let resp = self.client.get(&url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let row: serde_json::Value = resp.json().await.ok()?;
        row.get("id")?;
        if std::fs::create_dir_all(&self.dir).is_ok() {
            let tmp = self.dir.join(format!(".{id}.json.tmp"));
            if std::fs::write(&tmp, serde_json::to_vec(&row).unwrap_or_default()).is_ok() {
                let _ = std::fs::rename(&tmp, self.path(id));
            }
        }
        Some(row)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod mirror_tests {
    use super::*;

    #[test]
    fn a_pqdi_row_becomes_the_summary_the_site_renders() {
        let row: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/pqdi_item_30563.json")).unwrap();
        let s = ItemSummary::from_row(&row);
        assert_eq!(
            (s.id, s.name.as_str()),
            (30563, "Wistful Tunic of the Void")
        );
        assert_eq!(
            (s.ac, s.hp, s.mana, s.req_level, s.icon),
            (32, 100, 75, 55, 632)
        );
        assert_eq!(
            s.stats,
            [8, 25, 25, 8, 15, 0, 0],
            "str sta agi dex wis int cha"
        );
        assert_eq!(s.resists, [15, 15, 15, 0, 0], "mr fr cr dr pr");
        assert!(s.click.is_some(), "the tunic has a click effect");
        assert!(s.no_drop);
    }

    #[test]
    fn the_mirror_reads_what_it_wrote_and_never_fetches_for_a_hit() {
        let dir = tempfile::tempdir().unwrap();
        let m = ItemMirror::new(dir.path());
        assert!(m.cached(30563).is_none());
        std::fs::create_dir_all(dir.path().join("items")).unwrap();
        std::fs::write(
            dir.path().join("items/30563.json"),
            include_str!("../tests/fixtures/pqdi_item_30563.json"),
        )
        .unwrap();
        let row = m.cached(30563).unwrap();
        assert_eq!(
            ItemSummary::from_row(&row).name,
            "Wistful Tunic of the Void"
        );
    }
}
