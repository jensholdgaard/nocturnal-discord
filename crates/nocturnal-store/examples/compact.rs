//! Manual compaction: `cargo run --example compact -- <data-dir>`

use nocturnal_store::Store;

fn main() {
    let dir = std::env::args().nth(1).expect("usage: compact <data-dir>");
    let t0 = std::time::Instant::now();
    let (mut store, envelopes) = Store::open(&dir).expect("store opens");
    println!(
        "boot: {} events replayed in {:?}",
        envelopes.len(),
        t0.elapsed()
    );
    store.wal().seal().expect("seal");
    let report = store.compact().expect("compact");
    println!(
        "compacted: {} events -> {:?} ({} wal segment(s) deleted)",
        report.events_moved, report.partitions_written, report.segments_deleted
    );
    let t1 = std::time::Instant::now();
    let (_, replayed) = Store::open(&dir).expect("store reopens");
    println!("reboot: {} events in {:?}", replayed.len(), t1.elapsed());
    assert_eq!(replayed, envelopes, "replay identical after compaction");
    println!("replay identical: OK");
}
