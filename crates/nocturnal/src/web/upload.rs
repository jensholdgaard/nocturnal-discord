//! The site's drop zone: `POST /upload` with a Zeal export as the body.
//!
//! The site server trusts nothing about the viewer, so an upload starts by
//! asking Perses who holds the cookies the browser sent - the same whoami
//! the pages call - and maps that login to a ledger player through the
//! snapshot's member list. From there it is the one upload flow the Discord
//! command uses (`profiles::upload_export`). Caddy's login wall already
//! stands in front of `/upload`; this is the second lock, the one that
//! names the member.
//!
//! The request is raw: the file's bytes as the body and its name in an
//! `x-filename` header, which is what one `fetch` call sends and spares a
//! multipart parser. Size is capped before the body is read.

use std::io::Read;

use crate::site::SiteHandle;

/// Where Perses answers on the box. Caddy proxies the same origin for the
/// browser; the bot asks it directly with the browser's cookies.
const PERSES_WHOAMI: &str = "http://127.0.0.1:8080/api/v1/user/whoami";

/// The body is a text export of a few KB; anything larger is not one.
pub const MAX_BODY: usize = 512 * 1024;

/// What the server thread needs to run an upload: the ledger, the guild it
/// writes for, the snapshot with the member list, and a runtime to await on.
#[derive(Clone)]
pub struct UploadCtx {
    pub driver: crate::driver::DriverHandle,
    pub ledger_guild: u64,
    pub site: SiteHandle,
    pub rt: tokio::runtime::Handle,
}

/// The request line and headers of one HTTP/1.1 request, as far as this
/// server needs them.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Head {
    pub method: String,
    pub path: String,
    pub content_length: usize,
    pub cookie: Option<String>,
    pub filename: Option<String>,
}

/// Parse the head of a request from the bytes read so far. `None` until the
/// blank line that ends the headers has arrived. Returns the head and the
/// index where the body starts.
pub fn parse_head(buf: &[u8]) -> Option<(Head, usize)> {
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let text = String::from_utf8_lossy(&buf[..end]);
    let mut lines = text.split("\r\n");
    let mut parts = lines.next().unwrap_or("").split_whitespace();
    let mut head = Head {
        method: parts.next().unwrap_or("GET").to_owned(),
        path: parts.next().unwrap_or("/").to_owned(),
        ..Head::default()
    };
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => head.content_length = value.parse().unwrap_or(0),
            "cookie" => head.cookie = Some(value.to_owned()),
            "x-filename" => head.filename = Some(value.to_owned()),
            _ => {}
        }
    }
    Some((head, end + 4))
}

/// Read the rest of the body after `already` bytes of it arrived with the
/// head. The caller has checked the length against [`MAX_BODY`].
pub fn read_body(stream: &mut impl Read, already: &[u8], content_length: usize) -> Vec<u8> {
    let mut body = already.to_vec();
    while body.len() < content_length {
        let mut chunk = vec![0u8; (content_length - body.len()).min(64 * 1024)];
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    body.truncate(content_length);
    body
}

/// A JSON answer for the page's script.
pub fn reply(status: &'static str, ok: bool, message: &str) -> super::Response {
    super::Response {
        status,
        content_type: "application/json; charset=utf-8",
        body: serde_json::json!({ "ok": ok, "message": message })
            .to_string()
            .into_bytes(),
        headers: vec!["cache-control: no-store".into()],
    }
}

/// Who Perses says holds these cookies: the login (Discord username), or
/// nothing when not signed in or Perses is unreachable.
async fn whoami(cookie: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;
    let r = client
        .get(PERSES_WHOAMI)
        .header("cookie", cookie)
        .send()
        .await
        .ok()?;
    if !r.status().is_success() {
        return None;
    }
    let v: serde_json::Value = r.json().await.ok()?;
    v["metadata"]["name"].as_str().map(str::to_owned)
}

/// Run one upload for the viewer behind `head`'s cookies.
pub fn handle(ctx: &UploadCtx, head: &Head, body: &[u8]) -> super::Response {
    let Some(cookie) = head.cookie.as_deref() else {
        return reply("401 Unauthorized", false, "Sign in first.");
    };
    let filename = head
        .filename
        .as_deref()
        .map(|f| f.rsplit(['/', '\\']).next().unwrap_or(f).to_owned())
        .unwrap_or_else(|| "export.txt".to_owned());
    let Some(login) = ctx.rt.block_on(whoami(cookie)) else {
        return reply(
            "401 Unauthorized",
            false,
            "Perses does not know who you are - sign in again.",
        );
    };
    let player = ctx
        .site
        .read()
        .ok()
        .and_then(|s| s.clone())
        .and_then(|s| s.logins.get(&login.to_lowercase()).copied());
    let Some(player) = player else {
        return reply(
            "403 Forbidden",
            false,
            &format!("{login} is not on the ledger as a member, so there is no roster row to put this on."),
        );
    };
    let outcome = ctx.rt.block_on(crate::profiles::upload_export(
        &ctx.driver,
        ctx.ledger_guild,
        player,
        &filename,
        body,
        crate::discord::chrono_now_ms(),
    ));
    match outcome {
        Ok(o) => reply("200 OK", true, &o.line()),
        Err(e) => reply("422 Unprocessable Content", false, &e),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_head_is_parsed_case_insensitively_and_the_body_offset_is_right() {
        let raw = b"POST /upload HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\nCookie: a=b; c=d\r\nX-Filename: ZiglaxQuarmy.txt\r\n\r\nhello";
        let (h, at) = parse_head(raw).unwrap();
        assert_eq!(h.method, "POST");
        assert_eq!(h.path, "/upload");
        assert_eq!(h.content_length, 5);
        assert_eq!(h.cookie.as_deref(), Some("a=b; c=d"));
        assert_eq!(h.filename.as_deref(), Some("ZiglaxQuarmy.txt"));
        assert_eq!(&raw[at..], b"hello");
    }

    #[test]
    fn an_incomplete_head_waits() {
        assert!(parse_head(b"POST /upload HTTP/1.1\r\nContent-Length: 5\r\n").is_none());
    }

    #[test]
    fn the_body_is_read_to_its_length_and_no_further() {
        let mut rest = std::io::Cursor::new(b"world!!!".to_vec());
        let body = read_body(&mut rest, b"hello ", 11);
        assert_eq!(body, b"hello world");
    }

    #[test]
    fn a_reply_is_json_the_page_can_show() {
        let r = reply("200 OK", true, "done");
        assert_eq!(r.content_type, "application/json; charset=utf-8");
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["message"], "done");
    }
}
