//! Minimal health endpoints on a plain std thread: `/healthz` (process live)
//! and `/readyz` (replay done + gateway connected). No async, no framework —
//! the container HEALTHCHECK needs two bytes, not axum.

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

pub fn serve(bind: &str, readiness: Readiness) -> anyhow::Result<()> {
    let listener = TcpListener::bind(bind)?;
    tracing::info!(bind, "health endpoints up");
    std::thread::Builder::new()
        .name("health".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 512];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("/");
                let (status, body) = match path {
                    "/healthz" => ("200 OK", "ok\n"),
                    "/readyz" if readiness.is_ready() => ("200 OK", "ready\n"),
                    "/readyz" => ("503 Service Unavailable", "starting\n"),
                    _ => ("404 Not Found", "\n"),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        })?;
    Ok(())
}
