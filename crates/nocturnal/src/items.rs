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
    let mut collapsed = String::with_capacity(out.len());
    let mut blank = 0;
    for line in out.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        collapsed.push_str(line.trim_end());
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
        assert!(
            !text.contains("\n\n\n"),
            "blank runs not collapsed: {text:?}"
        );
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
