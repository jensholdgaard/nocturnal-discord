//! Health endpoints and the guild site on one plain std thread: `/healthz`
//! (process live), `/readyz` (replay done + gateway connected), and every
//! other path handed to `crate::web`, which renders a page from the live
//! snapshot. No async, no framework: a page is a string, a request is a
//! line, and Caddy in front does TLS, the login wall and keep-alive.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct Readiness(Arc<AtomicBool>);

impl Readiness {
    pub fn set_ready(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_ready(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub fn serve(
    bind: &str,
    readiness: Readiness,
    site: crate::site::SiteHandle,
    assets_dir: Option<std::path::PathBuf>,
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
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let (status, body): (&str, &[u8]) = match path.as_str() {
                    "/healthz" => ("200 OK", b"ok\n"),
                    "/readyz" if readiness.is_ready() => ("200 OK", b"ready\n"),
                    "/readyz" => ("503 Service Unavailable", b"starting\n"),
                    // Everything else is the guild site, rendered from the
                    // live snapshot. Caddy has already put it behind the login.
                    _ => {
                        let r = crate::web::respond(&path, &site, assets_dir.as_deref());
                        let mut head = format!(
                            "HTTP/1.1 {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n",
                            r.status,
                            r.content_type,
                            r.body.len()
                        );
                        for h in &r.headers {
                            head.push_str(h);
                            head.push_str("\r\n");
                        }
                        head.push_str("\r\n");
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(&r.body);
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
