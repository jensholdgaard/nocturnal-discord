use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use nocturnal_core::Envelope;

const SEGMENT_EXT: &str = "jsonl";
/// Default rotation threshold; tiny events mean this is years of guild play.
pub const DEFAULT_SEGMENT_MAX_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug)]
pub enum WalError {
    Io(std::io::Error),
    /// Corruption anywhere but the trailing record of the last segment.
    Corrupt {
        segment: PathBuf,
        line: usize,
        reason: String,
    },
    /// Replayed sequence numbers are not contiguous.
    SequenceGap {
        expected: u64,
        found: u64,
    },
}

impl From<std::io::Error> for WalError {
    fn from(e: std::io::Error) -> Self {
        WalError::Io(e)
    }
}

impl std::fmt::Display for WalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WalError::Io(e) => write!(f, "wal io: {e}"),
            WalError::Corrupt {
                segment,
                line,
                reason,
            } => {
                write!(f, "wal corrupt at {}:{line}: {reason}", segment.display())
            }
            WalError::SequenceGap { expected, found } => {
                write!(f, "wal sequence gap: expected {expected}, found {found}")
            }
        }
    }
}

impl std::error::Error for WalError {}

/// Append-only, fsync-per-record write-ahead log.
#[derive(Debug)]
pub struct Wal {
    dir: PathBuf,
    file: File,
    path: PathBuf,
    bytes: u64,
    segment_max_bytes: u64,
    next_seq: u64,
}

fn encode(env: &Envelope) -> Result<String, WalError> {
    let json = serde_json::to_string(env).map_err(|e| WalError::Io(std::io::Error::other(e)))?;
    let crc = crc32fast::hash(json.as_bytes());
    Ok(format!("{crc:08x} {json}\n"))
}

fn decode(line: &str) -> Result<Envelope, String> {
    let (crc_hex, json) = line
        .split_once(' ')
        .ok_or_else(|| "missing crc field".to_owned())?;
    let expected = u32::from_str_radix(crc_hex, 16).map_err(|_| "bad crc field".to_owned())?;
    let actual = crc32fast::hash(json.as_bytes());
    if expected != actual {
        return Err(format!(
            "crc mismatch (stored {expected:08x}, computed {actual:08x})"
        ));
    }
    serde_json::from_str(json).map_err(|e| format!("bad json: {e}"))
}

fn segment_path(dir: &Path, first_seq: u64) -> PathBuf {
    dir.join(format!("{first_seq:012}.{SEGMENT_EXT}"))
}

impl Wal {
    /// Open (creating the directory if needed), recover, and replay.
    /// Returns the WAL positioned for appends plus every stored envelope in
    /// sequence order.
    pub fn open(dir: impl Into<PathBuf>) -> Result<(Wal, Vec<Envelope>), WalError> {
        Self::open_with(dir, DEFAULT_SEGMENT_MAX_BYTES)
    }

    pub fn open_with(
        dir: impl Into<PathBuf>,
        segment_max_bytes: u64,
    ) -> Result<(Wal, Vec<Envelope>), WalError> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;

        let mut segments: Vec<PathBuf> = fs::read_dir(&dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == SEGMENT_EXT))
            .collect();
        segments.sort();

        let mut envelopes: Vec<Envelope> = Vec::new();
        for (i, seg) in segments.iter().enumerate() {
            let last_segment = i == segments.len() - 1;
            Self::read_segment(seg, last_segment, &mut envelopes)?;
        }

        let mut next_seq = 0u64;
        for env in &envelopes {
            if env.seq != next_seq {
                return Err(WalError::SequenceGap {
                    expected: next_seq,
                    found: env.seq,
                });
            }
            next_seq += 1;
        }

        // Append to the last segment, or start the first one.
        let path = segments
            .last()
            .cloned()
            .unwrap_or_else(|| segment_path(&dir, 0));
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let bytes = file.metadata()?.len();

        Ok((
            Wal {
                dir,
                file,
                path,
                bytes,
                segment_max_bytes,
                next_seq,
            },
            envelopes,
        ))
    }

    fn read_segment(
        path: &Path,
        allow_torn_tail: bool,
        out: &mut Vec<Envelope>,
    ) -> Result<(), WalError> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut offset: u64 = 0;
        let mut line_no = 0usize;
        let mut buf = String::new();
        loop {
            buf.clear();
            let read = reader.read_line(&mut buf)?;
            if read == 0 {
                break;
            }
            line_no += 1;
            let complete = buf.ends_with('\n');
            let candidate = buf.trim_end_matches('\n');
            match (complete, decode(candidate)) {
                (true, Ok(env)) => {
                    offset += read as u64;
                    out.push(env);
                }
                (complete, result) => {
                    let reason = match result {
                        Ok(_) => "torn record (no trailing newline)".to_owned(),
                        Err(e) => e,
                    };
                    // Only a *trailing* bad record of the *last* segment is a
                    // crash artifact; anything else is real corruption.
                    let rest = reader.fill_buf()?;
                    let at_eof = rest.is_empty();
                    if allow_torn_tail && at_eof && !(complete && candidate.is_empty()) {
                        let f = OpenOptions::new().write(true).open(path)?;
                        f.set_len(offset)?;
                        f.sync_data()?;
                        return Ok(());
                    }
                    return Err(WalError::Corrupt {
                        segment: path.to_path_buf(),
                        line: line_no,
                        reason,
                    });
                }
            }
        }
        Ok(())
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Durably append envelopes: written, flushed, and fsynced before Ok.
    pub fn append(&mut self, envelopes: &[Envelope]) -> Result<(), WalError> {
        for env in envelopes {
            if env.seq != self.next_seq {
                return Err(WalError::SequenceGap {
                    expected: self.next_seq,
                    found: env.seq,
                });
            }
            if self.bytes >= self.segment_max_bytes {
                self.rotate(env.seq)?;
            }
            let line = encode(env)?;
            self.file.write_all(line.as_bytes())?;
            self.bytes += line.len() as u64;
            self.next_seq += 1;
        }
        self.file.sync_data()?;
        Ok(())
    }

    fn rotate(&mut self, first_seq: u64) -> Result<(), WalError> {
        self.file.sync_data()?;
        let path = segment_path(&self.dir, first_seq);
        self.file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)?;
        self.path = path;
        self.bytes = 0;
        Ok(())
    }

    /// Segment files at and below this path are sealed (not the append target)
    /// — the compaction input set (M2).
    pub fn current_segment(&self) -> &Path {
        &self.path
    }
}
