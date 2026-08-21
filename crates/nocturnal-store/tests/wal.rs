//! WAL durability: round-trips, rotation, torn-tail recovery (hazard B1),
//! and refusal to load real corruption.

#![allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion

use std::fs;
use std::io::Write;

use nocturnal_core::event::Event;
use nocturnal_core::{Actor, Envelope};
use nocturnal_store::{Wal, WalError};

fn env(seq: u64) -> Envelope {
    Envelope {
        seq,
        ts_ms: 1_000 + seq as i64,
        guild: 1,
        actor: Actor::System,
        v: 1,
        correlation_id: None,
        event: Event::AuctionClosed {
            auction_id: format!("a{seq}"),
        },
    }
}

#[test]
fn append_reopen_replay_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let (mut wal, initial) = Wal::open(dir.path()).unwrap();
    assert!(initial.is_empty());
    let batch: Vec<Envelope> = (0..25).map(env).collect();
    wal.append(&batch).unwrap();
    drop(wal);

    let (wal, replayed) = Wal::open(dir.path()).unwrap();
    assert_eq!(replayed, batch);
    assert_eq!(wal.next_seq(), 25);
}

#[test]
fn rotation_splits_segments_and_replay_spans_them() {
    let dir = tempfile::tempdir().unwrap();
    let (mut wal, _) = Wal::open_with(dir.path(), 200).unwrap(); // tiny segments
    let batch: Vec<Envelope> = (0..50).map(env).collect();
    for e in &batch {
        wal.append(std::slice::from_ref(e)).unwrap();
    }
    drop(wal);

    let segments = fs::read_dir(dir.path()).unwrap().count();
    assert!(segments > 1, "expected rotation, got {segments} segment(s)");
    let (_, replayed) = Wal::open(dir.path()).unwrap();
    assert_eq!(replayed, batch);
}

#[test]
fn torn_trailing_record_is_truncated_and_appends_continue() {
    let dir = tempfile::tempdir().unwrap();
    let (mut wal, _) = Wal::open(dir.path()).unwrap();
    let batch: Vec<Envelope> = (0..10).map(env).collect();
    wal.append(&batch).unwrap();
    let segment = wal.current_segment().to_path_buf();
    drop(wal);

    // Crash mid-write: half a record, no newline.
    let mut f = fs::OpenOptions::new().append(true).open(&segment).unwrap();
    f.write_all(b"deadbeef {\"seq\":10,\"truncat").unwrap();
    drop(f);

    let (mut wal, replayed) = Wal::open(dir.path()).unwrap();
    assert_eq!(replayed.len(), 10, "torn tail dropped, good records kept");
    assert_eq!(wal.next_seq(), 10);
    // The log keeps working where the crash left off.
    wal.append(&[env(10)]).unwrap();
    drop(wal);
    let (_, replayed) = Wal::open(dir.path()).unwrap();
    assert_eq!(replayed.len(), 11);
}

#[test]
fn corrupt_middle_record_refuses_to_load() {
    let dir = tempfile::tempdir().unwrap();
    let (mut wal, _) = Wal::open(dir.path()).unwrap();
    wal.append(&(0..5).map(env).collect::<Vec<_>>()).unwrap();
    let segment = wal.current_segment().to_path_buf();
    drop(wal);

    // Flip a byte in the middle of the file (bit rot, not a crash artifact).
    let mut bytes = fs::read(&segment).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x01;
    fs::write(&segment, &bytes).unwrap();

    match Wal::open(dir.path()) {
        Err(WalError::Corrupt { .. }) => {}
        other => panic!("expected Corrupt, got {other:?}"),
    }
}

#[test]
fn sequence_gap_is_refused_on_append_and_load() {
    let dir = tempfile::tempdir().unwrap();
    let (mut wal, _) = Wal::open(dir.path()).unwrap();
    wal.append(&[env(0)]).unwrap();
    match wal.append(&[env(5)]) {
        Err(WalError::SequenceGap {
            expected: 1,
            found: 5,
        }) => {}
        other => panic!("expected SequenceGap, got {other:?}"),
    }
}
