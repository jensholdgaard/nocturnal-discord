//! The same log-export proof as `otlp_logs.rs`, but under production
//! conditions: inside a multi-thread Tokio runtime and with
//! `OTEL_EXPORTER_OTLP_HEADERS` set, because that is how the bot runs.
//!
//! Separate binary because `init` installs a *global* subscriber.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logs_export_from_inside_a_tokio_runtime() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let port = mock_otlp(Arc::clone(&seen));

    let guard = temp_env::with_vars(
        [
            (
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                Some(format!("http://127.0.0.1:{port}")),
            ),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", Some("http/protobuf".into())),
            (
                "OTEL_EXPORTER_OTLP_HEADERS",
                Some("Authorization=Bearer test-token".into()),
            ),
            ("OTEL_SERVICE_NAME", Some("nocturnal-tokio-test".into())),
            ("OTEL_BLRP_SCHEDULE_DELAY", Some("100".into())),
            ("OTEL_BSP_SCHEDULE_DELAY", Some("100".into())),
            ("OTEL_METRIC_EXPORT_INTERVAL", Some("600000".into())),
        ],
        || {
            nocturnal_telemetry::init(&nocturnal_telemetry::TelemetryConfig {
                default_service_name: "nocturnal-tokio-test".into(),
                log_filter: "info".into(),
                log_json: true,
            })
            .expect("telemetry init")
        },
    );

    // Emit from a spawned task, like the bot's command handlers do.
    tokio::spawn(async {
        tracing::info_span!(target: "nocturnal", "test.span").in_scope(|| {
            tracing::info!(target: "nocturnal", "hello from a tokio task");
        });
    })
    .await
    .expect("emitting task");

    // Shutdown flushes; do it off the async worker like the bot's exit path.
    tokio::task::spawn_blocking(move || drop(guard))
        .await
        .expect("telemetry shutdown");

    let requests = seen.lock().expect("seen lock").clone();
    assert!(
        requests.iter().any(|r| r.contains("/v1/logs")),
        "no OTLP log export under Tokio; requests seen: {requests:?}"
    );
}
