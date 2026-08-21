//! OTLP wiring: traces, logs, and metrics exporters plus the tracing
//! subscriber stack. With no endpoint configured (and none in the standard
//! `OTEL_EXPORTER_OTLP_ENDPOINT` env), only local fmt logging is installed —
//! the bot runs identically, just unexported.

use anyhow::Context as _;
use opentelemetry::global;
use opentelemetry::metrics::{Counter, Gauge, Histogram};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::Layer as _;

use crate::metric;

#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// Explicit OTLP endpoint; `None` falls back to the standard
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` env var; neither = export disabled.
    pub endpoint: Option<String>,
    /// "grpc" or "http/protobuf".
    pub protocol: String,
    pub service_name: String,
    /// tracing filter directive (e.g. "info").
    pub log_filter: String,
    pub log_json: bool,
}

/// Keeps providers alive; flushes and shuts them down on drop.
pub struct TelemetryGuard {
    tracer: Option<SdkTracerProvider>,
    logger: Option<SdkLoggerProvider>,
    meter: Option<SdkMeterProvider>,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(p) = &self.tracer {
            let _ = p.shutdown();
        }
        if let Some(p) = &self.logger {
            let _ = p.shutdown();
        }
        if let Some(p) = &self.meter {
            let _ = p.shutdown();
        }
    }
}

fn export_enabled(cfg: &TelemetryConfig) -> bool {
    cfg.endpoint.is_some() || std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok()
}

macro_rules! exporter {
    ($builder:expr, $cfg:expr) => {{
        let b = $builder;
        let b = if $cfg.protocol == "http/protobuf" {
            b.with_http().build()
        } else {
            let b = b.with_tonic();
            match &$cfg.endpoint {
                Some(e) => opentelemetry_otlp::WithExportConfig::with_endpoint(b, e).build(),
                None => b.build(),
            }
        };
        b
    }};
}

/// Install the global tracing subscriber (fmt + OTLP layers) and the global
/// meter provider. Call once, early; hold the guard until exit.
pub fn init(cfg: &TelemetryConfig) -> anyhow::Result<TelemetryGuard> {
    let filter = tracing_subscriber::EnvFilter::try_from_env("NOCTURNAL_LOG")
        .or_else(|_| tracing_subscriber::EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&cfg.log_filter));

    let fmt_layer = if cfg.log_json {
        tracing_subscriber::fmt::layer().json().boxed()
    } else {
        tracing_subscriber::fmt::layer().boxed()
    };

    if !export_enabled(cfg) {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .init();
        return Ok(TelemetryGuard {
            tracer: None,
            logger: None,
            meter: None,
        });
    }

    let resource = Resource::builder()
        .with_service_name(cfg.service_name.clone())
        .build();

    let span_exporter = exporter!(opentelemetry_otlp::SpanExporter::builder(), cfg)
        .context("building OTLP span exporter")?;
    let tracer_provider = SdkTracerProvider::builder()
        .with_batch_exporter(span_exporter)
        .with_resource(resource.clone())
        .build();
    let otel_trace_layer =
        tracing_opentelemetry::layer().with_tracer(tracer_provider.tracer("nocturnal"));

    let log_exporter = exporter!(opentelemetry_otlp::LogExporter::builder(), cfg)
        .context("building OTLP log exporter")?;
    let logger_provider = SdkLoggerProvider::builder()
        .with_batch_exporter(log_exporter)
        .with_resource(resource.clone())
        .build();
    let otel_log_layer = OpenTelemetryTracingBridge::new(&logger_provider);

    let metric_exporter = exporter!(opentelemetry_otlp::MetricExporter::builder(), cfg)
        .context("building OTLP metric exporter")?;
    let meter_provider = SdkMeterProvider::builder()
        .with_periodic_exporter(metric_exporter)
        .with_resource(resource)
        .build();
    global::set_meter_provider(meter_provider.clone());

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(otel_trace_layer)
        .with(otel_log_layer)
        .init();

    tracing::info!(
        endpoint = cfg
            .endpoint
            .as_deref()
            .unwrap_or("(from OTEL_EXPORTER_OTLP_ENDPOINT)"),
        protocol = cfg.protocol,
        "OTLP export enabled"
    );

    Ok(TelemetryGuard {
        tracer: Some(tracer_provider),
        logger: Some(logger_provider),
        meter: Some(meter_provider),
    })
}

/// The ledger's metric instruments, names straight from the registry. With no
/// exporter installed these are no-ops — always safe to record.
pub struct Metrics {
    pub commands: Counter<u64>,
    pub commit_duration: Histogram<f64>,
    pub ledger_events: Counter<u64>,
    pub ledger_seq: Gauge<u64>,
    pub wal_fsync_duration: Histogram<f64>,
    pub discord_reconnects: Counter<u64>,
}

impl Metrics {
    pub fn new() -> Metrics {
        let meter = global::meter("nocturnal");
        Metrics {
            commands: meter
                .u64_counter(metric::NOCTURNAL_COMMANDS)
                .with_unit("{interaction}")
                .build(),
            commit_duration: meter
                .f64_histogram(metric::NOCTURNAL_INTERACTION_COMMIT_DURATION)
                .with_unit("s")
                .build(),
            ledger_events: meter
                .u64_counter(metric::NOCTURNAL_LEDGER_EVENTS)
                .with_unit("{event}")
                .build(),
            ledger_seq: meter
                .u64_gauge(metric::NOCTURNAL_LEDGER_SEQ)
                .with_unit("{event}")
                .build(),
            wal_fsync_duration: meter
                .f64_histogram(metric::NOCTURNAL_WAL_FSYNC_DURATION)
                .with_unit("s")
                .build(),
            discord_reconnects: meter
                .u64_counter(metric::NOCTURNAL_DISCORD_RECONNECTS)
                .with_unit("{reconnect}")
                .build(),
        }
    }

    /// One command through the single writer: count + latency, tagged with
    /// registry attributes only (never player ids — cardinality rule).
    pub fn record_command(
        &self,
        command: &'static str,
        outcome: &'static str,
        rejection: Option<&'static str>,
        elapsed_s: f64,
    ) {
        let mut attrs = vec![
            KeyValue::new(crate::attr::NOCTURNAL_COMMAND, command),
            KeyValue::new(crate::attr::NOCTURNAL_DECISION_OUTCOME, outcome),
        ];
        if let Some(r) = rejection {
            attrs.push(KeyValue::new(crate::attr::NOCTURNAL_DECISION_REJECTION, r));
        }
        self.commands.add(1, &attrs);
        self.commit_duration.record(
            elapsed_s,
            &[KeyValue::new(crate::attr::NOCTURNAL_COMMAND, command)],
        );
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics::new()
    }
}
