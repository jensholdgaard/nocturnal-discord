//! The single writer. One dedicated OS thread owns the `Ledger` and the
//! `Store`; everything else talks to it through a channel. Writes follow
//! decide → append (fsync) → apply, strictly one at a time — the property
//! that makes the audit's whole race-condition track unrepresentable.
//! Reads are closures executed on the same thread: always consistent,
//! never locking.

use anyhow::Context as _;
use nocturnal_core::{Actor, Command, Envelope, GuildId, Ledger, Rejection};
use nocturnal_store::Store;
use nocturnal_telemetry::{attr, Metrics};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug)]
pub enum ExecError {
    Rejected(Rejection),
    /// WAL/storage failure — the command did NOT happen.
    Storage(String),
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecError::Rejected(r) => write!(f, "rejected: {r}"),
            ExecError::Storage(e) => write!(f, "storage: {e}"),
        }
    }
}

enum Request {
    Execute {
        guild: GuildId,
        actor: Actor,
        cmd: Box<Command>,
        reply: oneshot::Sender<Result<Vec<Envelope>, ExecError>>,
        /// In-process trace context: the caller's span, so the ledger work
        /// done on the writer thread nests under the interaction that caused
        /// it instead of starting its own orphan trace. (OTel guidance:
        /// propagate context explicitly across threads you start yourself.)
        parent: tracing::Span,
    },
    Query(Box<dyn FnOnce(&Ledger) + Send>),
    /// Roll sealed WAL segments into Parquet. Runs on the writer thread
    /// because only it may touch the `Store`, and it blocks that thread —
    /// commands queue behind it, which is why it belongs on a slow timer.
    Compact(oneshot::Sender<Result<nocturnal_store::CompactionReport, String>>),
}

#[derive(Clone)]
pub struct DriverHandle {
    tx: mpsc::Sender<Request>,
}

impl DriverHandle {
    /// Execute a command through the single writer. `Ok` means the events are
    /// fsynced and applied.
    pub async fn execute(
        &self,
        guild: GuildId,
        actor: Actor,
        cmd: Command,
    ) -> Result<Vec<Envelope>, ExecError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Request::Execute {
                guild,
                actor,
                cmd: Box::new(cmd),
                reply,
                parent: tracing::Span::current(),
            })
            .await
            .map_err(|_| ExecError::Storage("driver gone".into()))?;
        rx.await
            .map_err(|_| ExecError::Storage("driver gone".into()))?
    }

    /// Compact sealed WAL segments into Parquet.
    ///
    /// Every run is counted by partition and outcome: a compaction that starts
    /// failing is silent otherwise, and the WAL it should have drained just
    /// keeps growing (hazard B5).
    pub async fn compact(&self) -> Result<nocturnal_store::CompactionReport, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Request::Compact(reply))
            .await
            .map_err(|_| "driver gone".to_owned())?;
        rx.await.map_err(|_| "driver gone".to_owned())?
    }

    /// Run a read against the live projections on the writer thread.
    pub async fn query<R, F>(&self, f: F) -> R
    where
        R: Send + 'static,
        F: FnOnce(&Ledger) -> R + Send + 'static,
    {
        let (reply, rx) = oneshot::channel::<R>();
        let req = Request::Query(Box::new(move |ledger| {
            let _ = reply.send(f(ledger));
        }));
        // Both failure modes mean the driver thread is dead; nothing sane to
        // do for a read but propagate the panic.
        #[allow(clippy::expect_used)]
        {
            self.tx.send(req).await.expect("driver alive");
            rx.await.expect("driver alive")
        }
    }
}

/// How often the writer thread re-reads the gauges that describe its own
/// backlog. They are sampled here rather than from an observable callback
/// because only this thread may touch the `Store` and the `Ledger`; the
/// interval keeps a directory scan off the per-command hot path.
const GAUGE_SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Saturation gauges that only the single writer can see: how much WAL is
/// waiting for compaction, and how much work is currently open.
fn sample_gauges(store: &nocturnal_store::Store, ledger: &Ledger, metrics: &Metrics) {
    match store.wal_bytes() {
        Ok(bytes) => metrics.wal_size.record(bytes, &[]),
        // Never fatal: a stat failure must not take the writer down.
        Err(e) => {
            tracing::debug!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "could not measure the WAL")
        }
    }
    let mut short = 0u64;
    let mut long = 0u64;
    let mut raids = 0u64;
    for guild in ledger.state().guilds.values() {
        if guild.active_raid.is_some() {
            raids += 1;
        }
        for auction in guild.auctions.values() {
            if auction.status == nocturnal_core::state::AuctionStatus::Open {
                match auction.flavor {
                    nocturnal_core::event::Flavor::Short => short += 1,
                    nocturnal_core::event::Flavor::Long => long += 1,
                }
            }
        }
    }
    metrics.auctions_active.record(
        short,
        &[opentelemetry::KeyValue::new(
            attr::NOCTURNAL_AUCTION_FLAVOR,
            "short",
        )],
    );
    metrics.auctions_active.record(
        long,
        &[opentelemetry::KeyValue::new(
            attr::NOCTURNAL_AUCTION_FLAVOR,
            "long",
        )],
    );
    metrics.raids_active.record(raids, &[]);
}

fn now_ms() -> i64 {
    #[allow(clippy::expect_used)]
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after 1970");
    d.as_millis() as i64
}

/// Boot: lock is already held; open the store, replay, start the writer
/// thread. Returns the handle and the replayed event count.
/// Boot without an archive (tests, and any deployment that keeps history
/// only on local disk).
#[cfg(test)]
pub fn start(data_dir: &std::path::Path) -> anyhow::Result<(DriverHandle, usize)> {
    start_with_archive(data_dir, None)
}

/// Boot with an off-site archive: Parquet partitions the local disk lacks are
/// restored from object storage before replay.
pub fn start_with_archive(
    data_dir: &std::path::Path,
    archive: Option<nocturnal_store::Archive>,
) -> anyhow::Result<(DriverHandle, usize)> {
    let t0 = std::time::Instant::now();
    let (mut store, envelopes) = Store::open_with_archive(data_dir, archive)
        .with_context(|| format!("opening store in {}", data_dir.display()))?;
    let mut ledger = Ledger::new();
    for env in &envelopes {
        ledger.replay(env);
    }
    let replayed = envelopes.len();
    tracing::info!(
        { attr::NOCTURNAL_REPLAY_EVENT_COUNT } = replayed,
        { attr::NOCTURNAL_REPLAY_DURATION } = ?t0.elapsed(),
        "ledger replayed"
    );

    let (tx, mut rx) = mpsc::channel::<Request>(256);
    std::thread::Builder::new()
        .name("ledger-writer".into())
        .spawn(move || {
            let metrics = Metrics::new();
            // Seed the gauges so dashboards show the ledger head and the
            // compaction backlog from boot, not only after the first write.
            metrics.ledger_seq.record(ledger.next_seq(), &[]);
            sample_gauges(&store, &ledger, &metrics);
            let mut sampled_at = std::time::Instant::now();
            while let Some(req) = rx.blocking_recv() {
                if sampled_at.elapsed() >= GAUGE_SAMPLE_INTERVAL {
                    sample_gauges(&store, &ledger, &metrics);
                    sampled_at = std::time::Instant::now();
                }
                match req {
                    Request::Query(f) => f(&ledger),
                    Request::Compact(reply) => {
                        let span = tracing::info_span!("store.compact", otel.kind = "internal");
                        let outcome = span.in_scope(|| {
                            // Only *sealed* segments compact, and a segment
                            // seals at 16 MB. Sealing first is what makes a
                            // scheduled run mean "drain the WAL" rather than
                            // "drain it only once it got big"; merging dedupes
                            // by seq, so the extra partial partitions are free.
                            store.wal().seal()?;
                            store.compact()
                        });
                        let result = match outcome {
                            Ok(report) => {
                                for partition in &report.partitions_written {
                                    metrics.compaction_runs.add(
                                        1,
                                        &[
                                            opentelemetry::KeyValue::new(
                                                attr::NOCTURNAL_COMPACTION_PARTITION,
                                                partition.clone(),
                                            ),
                                            opentelemetry::KeyValue::new(
                                                attr::NOCTURNAL_DECISION_OUTCOME,
                                                "accepted",
                                            ),
                                        ],
                                    );
                                }
                                tracing::info!(
                                    { attr::NOCTURNAL_COMPACTION_EVENT_COUNT } = report.events_moved,
                                    { attr::NOCTURNAL_COMPACTION_PARTITIONS } = ?report.partitions_written,
                                    { attr::NOCTURNAL_COMPACTION_SEGMENTS_DELETED } = report.segments_deleted,
                                    "compacted sealed WAL segments into Parquet"
                                );
                                Ok(report)
                            }
                            Err(e) => {
                                // No partition attribute: a failed run does not
                                // know which one it would have written.
                                metrics.compaction_runs.add(
                                    1,
                                    &[opentelemetry::KeyValue::new(
                                        attr::NOCTURNAL_DECISION_OUTCOME,
                                        "error",
                                    )],
                                );
                                tracing::error!(
                                    { attr::NOCTURNAL_ERROR_MESSAGE } = %e,
                                    "compaction failed; the WAL was not drained"
                                );
                                Err(e.to_string())
                            }
                        };
                        // The backlog just changed by design — report it now
                        // rather than waiting for the sample interval.
                        sample_gauges(&store, &ledger, &metrics);
                        sampled_at = std::time::Instant::now();
                        let _ = reply.send(result);
                    }
                    Request::Execute { guild, actor, cmd, reply, parent } => {
                        let started = std::time::Instant::now();
                        let ctx = nocturnal_core::Ctx { guild, actor, now_ms: now_ms() };
                        let cmd = *cmd;
                        let command_kind = cmd.kind();
                        // Child of the interaction span (context propagated
                        // across the channel), INTERNAL: no process boundary.
                        let span = tracing::info_span!(
                            parent: &parent,
                            "ledger.execute",
                            otel.kind = "internal",
                            otel.status_code = tracing::field::Empty,
                            { attr::NOCTURNAL_GUILD_ID } = %guild,
                            { attr::NOCTURNAL_COMMAND } = command_kind,
                            { attr::NOCTURNAL_DECISION_OUTCOME } = tracing::field::Empty,
                            { attr::NOCTURNAL_EVENT_SEQ } = tracing::field::Empty,
                        );
                        let _entered = span.enter();

                        // decide → append(fsync) → apply, each its own span so
                        // a slow command says *which phase* was slow.
                        let decided = tracing::info_span!("ledger.decide", otel.kind = "internal")
                            .in_scope(|| ledger.propose(&ctx, &cmd));

                        let result = match decided {
                            Err(rej) => {
                                span.record(attr::NOCTURNAL_DECISION_OUTCOME, "rejected");
                                span.record("otel.status_code", "OK"); // a refusal is a valid outcome
                                metrics.record_command(
                                    command_kind,
                                    "rejected",
                                    Some(rej.slug()),
                                    started.elapsed().as_secs_f64(),
                                );
                                Err(ExecError::Rejected(rej))
                            }
                            Ok(envelopes) => {
                                let fsync_started = std::time::Instant::now();
                                let append_span = tracing::info_span!(
                                    "wal.append",
                                    otel.kind = "internal",
                                    events = envelopes.len(),
                                );
                                let appended = append_span.in_scope(|| store.append(&envelopes));
                                match appended {
                                    Ok(()) => {
                                        metrics
                                            .wal_fsync_duration
                                            .record(fsync_started.elapsed().as_secs_f64(), &[]);
                                        tracing::info_span!("ledger.apply", otel.kind = "internal")
                                            .in_scope(|| ledger.commit(&envelopes));
                                        span.record(attr::NOCTURNAL_DECISION_OUTCOME, "accepted");
                                        span.record("otel.status_code", "OK");
                                        if let Some(last) = envelopes.last() {
                                            span.record(attr::NOCTURNAL_EVENT_SEQ, last.seq);
                                        }
                                        for env in &envelopes {
                                            metrics.ledger_events.add(
                                                1,
                                                &[opentelemetry::KeyValue::new(
                                                    attr::NOCTURNAL_EVENT_KIND,
                                                    env.event.kind(),
                                                )],
                                            );
                                        }
                                        metrics.ledger_seq.record(ledger.next_seq(), &[]);
                                        metrics.record_command(
                                            command_kind,
                                            "accepted",
                                            None,
                                            started.elapsed().as_secs_f64(),
                                        );
                                        Ok(envelopes)
                                    }
                                    Err(e) => {
                                        span.record(attr::NOCTURNAL_DECISION_OUTCOME, "error");
                                        span.record("otel.status_code", "ERROR");
                                        tracing::error!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "WAL append failed; command dropped");
                                        metrics.record_command(
                                            command_kind,
                                            "error",
                                            None,
                                            started.elapsed().as_secs_f64(),
                                        );
                                        Err(ExecError::Storage(e.to_string()))
                                    }
                                }
                            }
                        };
                        let _ = reply.send(result);
                    }
                }
            }
            tracing::info!("ledger writer stopped");
        })
        .context("spawning ledger-writer thread")?;

    Ok((DriverHandle { tx }, replayed))
}

#[cfg(test)]
mod tests {
    use nocturnal_core::{Actor, Command};

    /// Compaction is the one write path with no user in front of it: it runs on
    /// a timer, and if it starts failing nothing notices except the WAL, which
    /// just keeps growing. This pins the whole loop — that it reaches the
    /// writer thread, that it actually drains sealed segments, and that the
    /// backlog gauge the dashboard reads moves as a result.
    #[tokio::test]
    async fn compaction_runs_through_the_writer_and_drains_the_wal() {
        const GUILD: u64 = 42;
        let dir = tempfile::tempdir().expect("tempdir");
        let (driver, _) = super::start(dir.path()).expect("driver");

        for i in 0..30 {
            driver
                .execute(
                    GUILD,
                    Actor::System,
                    Command::AdjustDkp {
                        player: 7,
                        delta: 1,
                        comment: format!("seed {i}"),
                        item: None,
                    },
                )
                .await
                .expect("append");
        }

        let before = wal_bytes(dir.path());
        assert!(before > 0, "the test needs events on disk");

        let report = driver.compact().await.expect("compaction succeeded");
        assert!(
            report.events_moved > 0,
            "compaction reported nothing moved: {report:?}"
        );
        assert!(
            !report.partitions_written.is_empty(),
            "no Parquet partition was written"
        );

        let after = wal_bytes(dir.path());
        assert!(
            after < before,
            "the WAL did not shrink: {before} -> {after}"
        );

        // Idempotent: a second run has nothing left to move and must not fail.
        let again = driver.compact().await.expect("second run succeeded");
        assert_eq!(again.events_moved, 0, "a re-run moved events twice");
    }

    fn wal_bytes(data_dir: &std::path::Path) -> u64 {
        std::fs::read_dir(data_dir.join("wal"))
            .expect("wal dir")
            .filter_map(|e| e.ok())
            .map(|e| e.metadata().expect("metadata").len())
            .sum()
    }
}
