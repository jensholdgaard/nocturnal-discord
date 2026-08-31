//! The guild site, rendered by the bot.
//!
//! Every page is a Maud template over [`crate::site::SiteData`] — typed
//! against the ledger, so a wrong field is a compile error — served from the
//! live snapshot the render loop publishes. The one thing that is not Rust
//! is the Perses island: pages leave `<div data-panel="…">` placeholders and
//! `/assets/island.js` mounts real Perses charts into them, querying
//! Prometheus through Perses on this origin with the viewer's login.
//!
//! Caddy keeps the whole site behind the Discord login; this server trusts
//! nothing about the viewer except what the page asks Perses for itself.

pub mod pages;

use std::path::{Path, PathBuf};

use crate::site::SiteHandle;

/// Where the island lives, recorded once at startup so templates can find
/// its checksum for cache-busting without threading a path through every
/// page function.
pub static ASSETS_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// What a request gets back.
pub struct Response {
    pub status: &'static str,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    /// Extra headers, each a full `name: value` line.
    pub headers: Vec<String>,
}

impl Response {
    fn html(body: String) -> Self {
        Response {
            status: "200 OK",
            content_type: "text/html; charset=utf-8",
            body: body.into_bytes(),
            headers: vec!["cache-control: no-cache, must-revalidate".into()],
        }
    }
    fn not_found() -> Self {
        Response {
            status: "404 Not Found",
            content_type: "text/plain; charset=utf-8",
            body: b"not found\n".to_vec(),
            headers: Vec::new(),
        }
    }
}

fn decode(seg: &str) -> String {
    urlencoding::decode(seg)
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| seg.to_owned())
}

/// Route one request. `path` is the request target without a query string.
pub fn respond(path: &str, site: &SiteHandle, assets_dir: Option<&Path>) -> Response {
    if let Some(d) = assets_dir {
        let _ = ASSETS_DIR.set(d.to_path_buf());
    }
    let path = path.split('?').next().unwrap_or("/");
    if let Some(rest) = path.strip_prefix("/assets/") {
        return serve_asset(rest, assets_dir);
    }
    let segs: Vec<String> = path.trim_matches('/').split('/').map(decode).collect();
    let known = matches!(
        (segs[0].as_str(), segs.len()),
        ("", 1)
            | ("me", 1)
            | ("roster", 1)
            | ("loot", 1)
            | ("raid", 2)
            | ("member", 2)
            | ("who", 2)
            | ("char", 2)
            | ("item", 2)
    );
    if !known {
        return Response::not_found();
    }
    let snapshot = site.read().ok().and_then(|s| s.clone());
    let Some(data) = snapshot else {
        return Response::html(pages::not_ready());
    };
    let page = match (segs[0].as_str(), segs.get(1).map(String::as_str)) {
        ("", None) => pages::raid(&data, None, assets_dir.is_some()),
        ("raid", Some(id)) => pages::raid(&data, Some(id), assets_dir.is_some()),
        ("me", None) => pages::me_redirect(),
        ("member", Some(login)) => pages::member(&data, login, assets_dir.is_some()),
        ("who", Some(name)) => pages::person(&data, name),
        ("char", Some(name)) => pages::character(&data, name),
        ("roster", None) => pages::roster(&data),
        ("loot", None) => pages::loot(&data),
        ("item", Some(name)) => pages::item(&data, name),
        _ => return Response::not_found(),
    };
    Response::html(page)
}

fn serve_asset(rest: &str, assets_dir: Option<&Path>) -> Response {
    let Some(dir) = assets_dir else {
        return Response::not_found();
    };
    // Only plain file names from the island build: no separators, no dots at
    // the front, so the directory is the whole reachable surface.
    if rest.is_empty() || rest.contains('/') || rest.starts_with('.') || rest.contains("..") {
        return Response::not_found();
    }
    let file: PathBuf = dir.join(rest);
    let Ok(body) = std::fs::read(&file) else {
        return Response::not_found();
    };
    let content_type = match file.extension().and_then(|e| e.to_str()) {
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("map") => "application/json",
        _ => "application/octet-stream",
    };
    Response {
        status: "200 OK",
        content_type,
        body,
        headers: vec!["cache-control: public, max-age=86400".into()],
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::site::*;

    fn fixture() -> SiteHandle {
        let mut data = SiteData {
            generated_ms: 1_787_853_613_551,
            ..Default::default()
        };
        data.raids.push(RaidView {
            id: "r1".into(),
            name: "Vulak & Ring War".into(),
            date_ms: 1_787_853_613_551,
            start_ms: 1_787_853_613_551,
            end_ms: 1_787_861_213_551,
            exact: true,
            ticks: 33,
            dkp_per_tick: 1,
            attendees: vec!["Galeedor".into(), "Shaku".into()],
            attendee_characters: vec!["galeedor".into(), "shaku".into()],
            loot: vec![LootView {
                ts_ms: 1_787_855_000_000,
                item: "Wistful Tunic of the Void".into(),
                winner: "Shaku".into(),
                cost: 18,
            }],
        });
        data.members.insert(
            "bisben_".into(),
            MemberView {
                name: "Controels".into(),
                discord: "Controels".into(),
                dkp: 201,
                attendance: 61.35,
                raids_attended: 6,
                last_active_ms: 1_787_853_613_551,
                history: vec![],
                characters: vec![CharacterView {
                    name: "Controels".into(),
                    class: "Bard".into(),
                    level: 60,
                    aa: None,
                    main: Some(nocturnal_core::MainRank::Main),
                }],
            },
        );
        data.people.insert(
            "Shaku".into(),
            PersonView {
                discord: Some("Asberdies".into()),
                characters: vec![],
                raiding: true,
            },
        );
        data.items.insert(
            "Wistful Tunic of the Void".into(),
            ItemView {
                id: "30563".into(),
                url: None,
                image: None,
                data: Some("AC: 32".into()),
                history: vec![],
            },
        );
        std::sync::Arc::new(std::sync::RwLock::new(Some(std::sync::Arc::new(data))))
    }

    #[test]
    fn every_route_renders_from_the_snapshot() {
        let site = fixture();
        for (path, expect) in [
            ("/", "Vulak &amp; Ring War"),
            ("/raid/r1", "Galeedor"),
            ("/member/bisben_", "Controels"),
            ("/who/Shaku", "Asberdies"),
            ("/roster", "Roster"),
            ("/loot", "Wistful Tunic"),
            ("/item/Wistful%20Tunic%20of%20the%20Void", "AC: 32"),
            ("/me", "whoami"),
        ] {
            let r = respond(path, &site, None);
            assert_eq!(r.status, "200 OK", "{path}");
            let body = String::from_utf8(r.body).unwrap();
            assert!(body.contains(expect), "{path} lacks {expect:?}");
        }
        assert_eq!(respond("/nope", &site, None).status, "404 Not Found");
        assert_eq!(
            respond("/member/nobody", &site, None).status,
            "200 OK",
            "an unknown member is a page, not an error"
        );
    }

    #[test]
    fn assets_never_escape_their_directory() {
        let site = fixture();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("island.js"), "ok").unwrap();
        assert_eq!(
            respond("/assets/island.js", &site, Some(dir.path())).status,
            "200 OK"
        );
        for bad in [
            "/assets/../Cargo.toml",
            "/assets/.hidden",
            "/assets/sub/x.js",
            "/assets/",
        ] {
            assert_eq!(
                respond(bad, &site, Some(dir.path())).status,
                "404 Not Found",
                "{bad}"
            );
        }
    }

    #[test]
    fn before_the_first_render_the_site_says_so() {
        let site: SiteHandle = Default::default();
        let r = respond("/", &site, None);
        assert!(String::from_utf8(r.body).unwrap().contains("Warming up"));
        assert_eq!(
            respond("/nope", &site, None).status,
            "404 Not Found",
            "unknown paths are 404 even while warming"
        );
    }
}
