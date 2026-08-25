//! `Store`: the full event home — WAL tail plus month-partitioned Parquet —
//! and the compaction that moves sealed WAL segments into Parquet.
//!
//! Crash-safety (hazard B5): Parquet is written to a temp path, fsynced,
//! renamed over the target, and read back and counted before any WAL segment
//! is deleted. Merging dedupes by `seq`, so a re-run after a crash at any
//! step is idempotent.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{Array, ArrayRef, Int64Array, RecordBatch, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;

use nocturnal_core::Envelope;

use crate::wal::{Wal, WalError};

/// Gregorian (year, month) from a unix-ms timestamp — Hinnant's
/// civil-from-days, no chrono dependency.
fn year_month(ts_ms: i64) -> (i64, u32) {
    let days = ts_ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m as u32)
}

fn partition_name(ts_ms: i64) -> String {
    let (y, m) = year_month(ts_ms);
    format!("{y:04}-{m:02}.parquet")
}

#[derive(Debug, Default)]
pub struct CompactionReport {
    pub events_moved: usize,
    pub partitions_written: Vec<String>,
    pub segments_deleted: usize,
}

pub struct Store {
    wal: Wal,
    events_dir: PathBuf,
    /// Optional off-site mirror of compacted partitions.
    archive: Option<crate::Archive>,
}

impl Store {
    /// Bytes of WAL not yet compacted into Parquet — the compaction backlog.
    pub fn wal_bytes(&self) -> Result<u64, WalError> {
        self.wal.size_bytes()
    }

    /// Open `<data_dir>/{events,wal}`, replaying Parquet history then the WAL
    /// tail. Returns every stored envelope in sequence order and refuses any
    /// gap or overlap mismatch between the two.
    pub fn open(data_dir: impl Into<PathBuf>) -> Result<(Store, Vec<Envelope>), WalError> {
        Self::open_with_archive(data_dir, None)
    }

    /// Open with an archive: any partition the archive holds but the local
    /// disk lacks is restored *before* replay, so a fresh disk rebuilds
    /// itself from object storage.
    pub fn open_with_archive(
        data_dir: impl Into<PathBuf>,
        archive: Option<crate::Archive>,
    ) -> Result<(Store, Vec<Envelope>), WalError> {
        let data_dir = data_dir.into();
        let events_dir = data_dir.join("events");
        fs::create_dir_all(&events_dir)?;

        if let Some(archive) = &archive {
            match futures::executor::block_on(archive.restore_missing(&events_dir)) {
                Ok(names) if !names.is_empty() => {
                    tracing::info!(count = names.len(), "restored partitions from the archive");
                }
                Ok(_) => {}
                // A cold archive must never stop the bot: local history is
                // authoritative for replay, the archive is the safety net.
                Err(e) => {
                    tracing::warn!(error = %e, "archive unreachable; continuing with local history")
                }
            }
        }

        let mut envelopes = read_all_parquet(&events_dir)?;
        envelopes.sort_by_key(|e| e.seq);
        for (i, env) in envelopes.iter().enumerate() {
            if env.seq != i as u64 {
                return Err(WalError::SequenceGap {
                    expected: i as u64,
                    found: env.seq,
                });
            }
        }

        let (mut wal, tail) = Wal::open(data_dir.join("wal"))?;
        let expected = envelopes.len() as u64;
        match tail.first() {
            Some(first) if first.seq != expected => {
                return Err(WalError::SequenceGap {
                    expected,
                    found: first.seq,
                });
            }
            Some(_) => {}
            None => wal.align_next_seq(expected),
        }
        envelopes.extend(tail);

        Ok((
            Store {
                wal,
                events_dir,
                archive,
            },
            envelopes,
        ))
    }

    pub fn wal(&mut self) -> &mut Wal {
        &mut self.wal
    }

    pub fn append(&mut self, envelopes: &[Envelope]) -> Result<(), WalError> {
        self.wal.append(envelopes)
    }

    /// Move all sealed WAL segments into monthly Parquet partitions.
    pub fn compact(&mut self) -> Result<CompactionReport, WalError> {
        let sealed = self.wal.sealed_segments()?;
        let mut report = CompactionReport::default();
        if sealed.is_empty() {
            return Ok(report);
        }

        let mut moving: Vec<Envelope> = Vec::new();
        for seg in &sealed {
            moving.extend(Wal::read_sealed(seg)?);
        }
        report.events_moved = moving.len();

        // Group by month partition.
        let mut by_partition: BTreeMap<String, Vec<Envelope>> = BTreeMap::new();
        for env in moving {
            by_partition
                .entry(partition_name(env.ts_ms))
                .or_default()
                .push(env);
        }

        for (name, mut envs) in by_partition {
            let target = self.events_dir.join(&name);
            if target.exists() {
                envs.extend(read_parquet(&target)?);
            }
            // Idempotence: dedupe by seq (re-runs and partial crashes).
            envs.sort_by_key(|e| e.seq);
            envs.dedup_by_key(|e| e.seq);
            let expected_rows = envs.len();

            let tmp = self.events_dir.join(format!(".tmp-{name}"));
            write_parquet(&tmp, &envs)?;
            fs::rename(&tmp, &target)?;
            // Durability of the rename itself.
            File::open(&self.events_dir)?.sync_all()?;

            // Read-back verification before any WAL deletion (B5).
            let back = read_parquet(&target)?;
            if back.len() != expected_rows {
                return Err(WalError::Corrupt {
                    segment: target.clone(),
                    line: 0,
                    reason: format!("read-back rows {} != written {expected_rows}", back.len()),
                });
            }
            // Write-through: only a partition that verified locally is
            // mirrored off-site.
            if let Some(archive) = &self.archive {
                match futures::executor::block_on(archive.put_partition(&target)) {
                    Ok(()) => tracing::info!(partition = %name, "partition archived"),
                    Err(e) => {
                        // Local history is intact; the WAL segments are still
                        // deleted because the partition is durable on disk.
                        tracing::warn!(partition = %name, error = %e, "archive upload failed");
                    }
                }
            }
            report.partitions_written.push(name);
        }

        for seg in &sealed {
            fs::remove_file(seg)?;
            report.segments_deleted += 1;
        }
        Ok(report)
    }
}

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("seq", DataType::UInt64, false),
        Field::new("ts_ms", DataType::Int64, false),
        Field::new("guild", DataType::UInt64, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("json", DataType::Utf8, false),
    ]))
}

fn write_parquet(path: &Path, envs: &[Envelope]) -> Result<(), WalError> {
    let io = |e: parquet::errors::ParquetError| WalError::Io(std::io::Error::other(e));
    let mut jsons = Vec::with_capacity(envs.len());
    for e in envs {
        jsons.push(serde_json::to_string(e).map_err(|e| WalError::Io(std::io::Error::other(e)))?);
    }
    let batch = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(UInt64Array::from_iter_values(envs.iter().map(|e| e.seq))) as ArrayRef,
            Arc::new(Int64Array::from_iter_values(envs.iter().map(|e| e.ts_ms))),
            Arc::new(UInt64Array::from_iter_values(envs.iter().map(|e| e.guild))),
            Arc::new(StringArray::from_iter_values(
                envs.iter().map(|e| e.event.kind()),
            )),
            Arc::new(StringArray::from_iter_values(
                jsons.iter().map(String::as_str),
            )),
        ],
    )
    .map_err(|e| WalError::Io(std::io::Error::other(e)))?;

    let file = File::create(path)?;
    let props = WriterProperties::builder()
        .set_compression(Compression::SNAPPY)
        .build();
    let mut writer = ArrowWriter::try_new(file, schema(), Some(props)).map_err(io)?;
    writer.write(&batch).map_err(io)?;
    let file = writer.into_inner().map_err(io)?;
    file.sync_all()?;
    Ok(())
}

fn read_parquet(path: &Path) -> Result<Vec<Envelope>, WalError> {
    let corrupt = |reason: String| WalError::Corrupt {
        segment: path.to_path_buf(),
        line: 0,
        reason,
    };
    let file = File::open(path)?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| corrupt(e.to_string()))?
        .build()
        .map_err(|e| corrupt(e.to_string()))?;
    let mut out = Vec::new();
    for batch in reader {
        let batch = batch.map_err(|e| corrupt(e.to_string()))?;
        let json = batch
            .column_by_name("json")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            .ok_or_else(|| corrupt("missing json column".into()))?;
        for i in 0..json.len() {
            let env: Envelope = serde_json::from_str(json.value(i))
                .map_err(|e| corrupt(format!("row {i}: {e}")))?;
            out.push(env);
        }
    }
    Ok(out)
}

fn read_all_parquet(dir: &Path) -> Result<Vec<Envelope>, WalError> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().is_some_and(|e| e == "parquet")
                && !p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(".tmp-"))
        })
        .collect();
    files.sort();
    let mut out = Vec::new();
    for f in files {
        out.extend(read_parquet(&f)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::year_month;

    #[test]
    fn civil_dates() {
        assert_eq!(year_month(0), (1970, 1));
        assert_eq!(year_month(1_712_944_829_490), (2024, 4));
        assert_eq!(year_month(1_755_800_000_000), (2025, 8));
        // month boundary: 2024-12-31T23:59:59.999Z vs 2025-01-01T00:00:00Z
        assert_eq!(year_month(1_735_689_599_999), (2024, 12));
        assert_eq!(year_month(1_735_689_600_000), (2025, 1));
    }
}
