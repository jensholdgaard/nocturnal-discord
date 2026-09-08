//! Health endpoints and the guild site on one plain std thread: `/healthz`
//! (process live), `/readyz` (replay done + gateway connected), and every
//! other path handed to `crate::web`, which renders a page from the live
//! snapshot. No async, no framework: a page is a string, a request is a
//! line, and Caddy in front does TLS, the login wall and keep-alive.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

/// How long the writer may go without a gauge-sampling cycle before
/// /readyz stops vouching for it. The scheduler drives a ledger query every
/// ten seconds, so a healthy writer beats at least that often.
const WRITER_STALE_MS: i64 = 60_000;

/// Ready = replay done and gateway connected, *and* the ledger writer thread
/// still beating. On 2026-09-03 the writer died and /readyz said 200 for
/// hours while every command failed; readiness that ignores the one thread
/// every command needs is not readiness.
#[derive(Clone, Default)]
pub struct Readiness {
    ready: Arc<AtomicBool>,
    writer_beat: Option<Arc<AtomicI64>>,
}

impl Readiness {
    /// Vouch for the writer too, via the driver's heartbeat.
    pub fn with_writer_beat(mut self, beat: Arc<AtomicI64>) -> Self {
        self.writer_beat = Some(beat);
        self
    }
    pub fn set_ready(&self) {
        self.ready.store(true, Ordering::Release);
    }
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire) && self.writer_fresh(now_ms())
    }
    fn writer_fresh(&self, now_ms: i64) -> bool {
        self.writer_beat.as_ref().map_or(true, |b| {
            now_ms - b.load(Ordering::Acquire) < WRITER_STALE_MS
        })
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn serve(
    bind: &str,
    readiness: Readiness,
    site: crate::site::SiteHandle,
    assets_dir: Option<std::path::PathBuf>,
    upload: Option<crate::web::upload::UploadCtx>,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(bind)?;
    tracing::info!(
        { nocturnal_telemetry::attr::NOCTURNAL_HEALTH_BIND } = bind,
        "health endpoints up"
    );
    std::thread::Builder::new()
        .name("health".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                // The head, however many reads it takes (a browser's POST
                // arrives in pieces); then the path decides what to do.
                let mut buf = Vec::with_capacity(4096);
                let head = loop {
                    let mut chunk = [0u8; 4096];
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break None;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(h) = crate::web::upload::parse_head(&buf) {
                        break Some(h);
                    }
                    if buf.len() > 64 * 1024 {
                        break None;
                    }
                };
                let Some((head, body_at)) = head else { continue };
                let path = head.path.clone();
                let write_response = |stream: &mut std::net::TcpStream, r: crate::web::Response| {
                    let mut out = format!(
                        "HTTP/1.1 {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n",
                        r.status,
                        r.content_type,
                        r.body.len()
                    );
                    for h in &r.headers {
                        out.push_str(h);
                        out.push_str("\r\n");
                    }
                    out.push_str("\r\n");
                    let _ = stream.write_all(out.as_bytes());
                    let _ = stream.write_all(&r.body);
                };
                // The one POST: a member's Zeal export for the site's drop zone.
                if head.method == "POST" {
                    let r = match (&upload, path.split('?').next()) {
                        (Some(ctx), Some("/upload")) => {
                            if head.content_length > crate::web::upload::MAX_BODY {
                                crate::web::upload::reply(
                                    "413 Content Too Large",
                                    false,
                                    "That file is too large to be an export.",
                                )
                            } else {
                                let body = crate::web::upload::read_body(
                                    &mut stream,
                                    &buf[body_at.min(buf.len())..],
                                    head.content_length,
                                );
                                crate::web::upload::handle(ctx, &head, &body)
                            }
                        }
                        _ => crate::web::Response::not_found(),
                    };
                    write_response(&mut stream, r);
                    continue;
                }
                let (status, body): (&str, &[u8]) = match path.as_str() {
                    "/healthz" => ("200 OK", b"ok\n"),
                    "/readyz" if readiness.is_ready() => ("200 OK", b"ready\n"),
                    "/readyz" => ("503 Service Unavailable", b"not ready (starting, or the ledger writer is not beating)\n"),
                    // Everything else is the guild site, rendered from the
                    // live snapshot. Caddy has already put it behind the login.
                    _ => {
                        let r = crate::web::respond(&path, &site, assets_dir.as_deref());
                        write_response(&mut stream, r);
                        continue;
                    }
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(body);
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dead_writer_is_not_ready() {
        let beat = Arc::new(AtomicI64::new(now_ms()));
        let r = Readiness::default().with_writer_beat(beat.clone());
        assert!(!r.is_ready(), "not before the gateway is up");
        r.set_ready();
        assert!(r.is_ready(), "fresh beat");
        beat.store(now_ms() - WRITER_STALE_MS - 1, Ordering::Release);
        assert!(!r.is_ready(), "the writer went quiet: 503, not 200");
        assert!(
            Readiness::default().writer_fresh(now_ms()),
            "no beat wired = not checked"
        );
    }
}
