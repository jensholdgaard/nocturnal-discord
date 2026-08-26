//! Compaction (hazard B5): WAL → Parquet is crash-safe, idempotent, and
//! replay across both stores is seamless.

#![allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion

use nocturnal_core::event::Event;
use nocturnal_core::{Actor, Envelope};
use nocturnal_store::{Store, Wal};

const AUG_2026: i64 = 1_786_000_000_000;
const SEP_2026: i64 = 1_789_000_000_000;

fn env(seq: u64, ts_ms: i64) -> Envelope {
    Envelope {
        seq,
        ts_ms,
        guild: 1,
        actor: Actor::System,
        v: 1,
        correlation_id: None,
        event: Event::AuctionClosed {
            auction_id: format!("a{seq}"),
            ended_ts_ms: None,
        },
    }
}

#[test]
fn compact_moves_sealed_segments_into_monthly_parquet() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, initial) = Store::open(dir.path()).unwrap();
    assert!(initial.is_empty());

    // Two months of events, sealed, plus a live tail.
    let batch: Vec<Envelope> = (0..40)
        .map(|i| env(i, if i < 25 { AUG_2026 } else { SEP_2026 }))
        .collect();
    store.append(&batch).unwrap();
    store.wal().seal().unwrap();
    let tail: Vec<Envelope> = (40..45).map(|i| env(i, SEP_2026)).collect();
    store.append(&tail).unwrap();

    let report = store.compact().unwrap();
    assert_eq!(report.events_moved, 40);
    assert_eq!(
        report.partitions_written,
        vec!["2026-08.parquet", "2026-09.parquet"]
    );
    assert_eq!(report.segments_deleted, 1);

    // Reopen: parquet history + wal tail replay seamlessly, in order.
    drop(store);
    let (_, replayed) = Store::open(dir.path()).unwrap();
    assert_eq!(replayed.len(), 45);
    let mut all = batch;
    all.extend(tail);
    assert_eq!(replayed, all);
}

#[test]
fn compaction_is_idempotent_after_partial_crash() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, _) = Store::open(dir.path()).unwrap();
    let batch: Vec<Envelope> = (0..10).map(|i| env(i, AUG_2026)).collect();
    store.append(&batch).unwrap();
    store.wal().seal().unwrap();

    // Snapshot the sealed segment, compact, then put the segment back —
    // simulating a crash after the Parquet rename but before WAL deletion.
    let sealed = store.wal().sealed_segments().unwrap();
    let seg = sealed[0].clone();
    let bytes = std::fs::read(&seg).unwrap();
    store.compact().unwrap();
    std::fs::write(&seg, &bytes).unwrap();

    // Re-run: dedupes by seq, no duplicates, segment cleaned up.
    let report = store.compact().unwrap();
    assert_eq!(report.events_moved, 10);
    assert_eq!(report.segments_deleted, 1);
    drop(store);
    let (_, replayed) = Store::open(dir.path()).unwrap();
    assert_eq!(replayed, batch);
}

#[test]
fn appends_continue_after_full_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, _) = Store::open(dir.path()).unwrap();
    store
        .append(&(0..8).map(|i| env(i, AUG_2026)).collect::<Vec<_>>())
        .unwrap();
    store.wal().seal().unwrap();
    store.compact().unwrap();
    drop(store);

    // Boot with an empty WAL and all history in Parquet: appends resume at 8.
    let (mut store, replayed) = Store::open(dir.path()).unwrap();
    assert_eq!(replayed.len(), 8);
    store.append(&[env(8, SEP_2026)]).unwrap();
    drop(store);
    let (_, replayed) = Store::open(dir.path()).unwrap();
    assert_eq!(replayed.len(), 9);
}

#[test]
fn mismatched_wal_start_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, _) = Store::open(dir.path()).unwrap();
    store
        .append(&(0..5).map(|i| env(i, AUG_2026)).collect::<Vec<_>>())
        .unwrap();
    store.wal().seal().unwrap();
    store.compact().unwrap();
    drop(store);

    // A WAL that starts beyond the parquet tail (lost segment) must refuse.
    let (mut wal, _) = Wal::open(dir.path().join("wal")).unwrap();
    wal.align_next_seq(7); // pretend seq 5,6 vanished
    wal.append(&[env(7, SEP_2026)]).unwrap();
    drop(wal);
    assert!(Store::open(dir.path()).is_err());
}

/// The archive is exercised through a local-filesystem object store: the
/// trait is the same one the S3 client implements, so this covers the
/// write-through and the boot-time restore without touching a network.
#[tokio::test]
async fn partitions_are_archived_and_restored_on_a_fresh_disk() {
    use nocturnal_store::Archive;
    use std::sync::Arc;

    let bucket = tempfile::tempdir().unwrap();
    let store =
        Arc::new(object_store::local::LocalFileSystem::new_with_prefix(bucket.path()).unwrap());
    let archive = Archive::with_store(store, "nocturnal");

    // A ledger that compacts with the archive attached.
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = Store::open_with_archive(dir.path(), Some(archive.clone())).unwrap();
    let batch: Vec<Envelope> = (0..12).map(|i| env(i, AUG_2026)).collect();
    s.append(&batch).unwrap();
    s.wal().seal().unwrap();
    let report = s.compact().unwrap();
    assert_eq!(report.partitions_written, vec!["2026-08.parquet"]);
    drop(s);

    // It reached the archive.
    assert_eq!(
        archive.list_partitions().await.unwrap(),
        vec!["2026-08.parquet"]
    );

    // A brand-new disk rebuilds its history from the archive alone.
    let fresh = tempfile::tempdir().unwrap();
    let (_, replayed) = Store::open_with_archive(fresh.path(), Some(archive.clone())).unwrap();
    assert_eq!(replayed, batch, "history restored from object storage");

    // An unreachable archive must never stop a boot: local history stands.
    let broken = Archive::with_store(
        Arc::new(
            object_store::local::LocalFileSystem::new_with_prefix(
                tempfile::tempdir().unwrap().path(),
            )
            .unwrap(),
        ),
        "nocturnal",
    );
    let (_, replayed) = Store::open_with_archive(dir.path(), Some(broken)).unwrap();
    assert_eq!(replayed, batch);
}
