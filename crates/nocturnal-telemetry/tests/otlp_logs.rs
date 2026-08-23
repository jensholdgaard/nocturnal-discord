//! Proves the OTLP *log* pipeline actually puts records on the wire.
//!
//! Traces reaching the collector says nothing about logs: they are separate
//! providers with separate exporters, and a broken appender is silent — the
//! process keeps logging to stdout while nothing is exported. This test caught
//! exactly that in production, where Jaeger had bot traces and the log backend
//! had none.
//!
//! `init` installs a global subscriber, so this binary runs a single test.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// Minimal OTLP/HTTP sink: records request lines, answers 200 with an empty
/// body (a valid protobuf `Export*ServiceResponse`).
fn mock_otlp(seen: Arc<Mutex<Vec<String>>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock collector");
    let port = listener.local_addr().expect("mock collector port").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            if let Some(line) = String::from_utf8_lossy(&buf[..n]).lines().next() {
                seen.lock().expect("seen lock").push(line.to_string());
            }
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/x-protobuf\r\nContent-Length: 0\r\n\r\n",
            );
            let _ = stream.flush();
        }
    });
    port
}

#[test]
fn info_events_from_our_crates_are_exported_as_otlp_logs() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let port = mock_otlp(Arc::clone(&seen));

    temp_env::with_vars(
        [
            (
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                Some(format!("http://127.0.0.1:{port}")),
            ),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", Some("http/protobuf".into())),
            ("OTEL_SERVICE_NAME", Some("nocturnal-otlp-test".into())),
            ("OTEL_BLRP_SCHEDULE_DELAY", Some("100".into())),
            ("OTEL_BSP_SCHEDULE_DELAY", Some("100".into())),
            ("OTEL_METRIC_EXPORT_INTERVAL", Some("600000".into())),
        ],
        || {
            let guard = nocturnal_telemetry::init(&nocturnal_telemetry::TelemetryConfig {
                default_service_name: "nocturnal-otlp-test".into(),
                log_filter: "info".into(),
                log_json: false,
            })
            .expect("telemetry init");

            tracing::info_span!(target: "nocturnal", "test.span").in_scope(|| {
                tracing::info!(target: "nocturnal", "hello from the log pipeline");
            });

            // Dropping the guard shuts the providers down, which flushes.
            drop(guard);
        },
    );

    let requests = seen.lock().expect("seen lock").clone();
    assert!(
        requests.iter().any(|r| r.contains("/v1/logs")),
        "no OTLP log export reached the collector; requests seen: {requests:?}"
    );
    assert!(
        requests.iter().any(|r| r.contains("/v1/traces")),
        "no OTLP trace export reached the collector; requests seen: {requests:?}"
    );
}
