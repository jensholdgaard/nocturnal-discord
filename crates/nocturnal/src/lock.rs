//! Hazard B2: two instances must never both write the ledger. An exclusive
//! flock on `<data-dir>/LOCK`, taken before anything else and held for the
//! process lifetime; a second instance fails loudly instead of corrupting.

use std::fs::File;
use std::path::Path;

use anyhow::{bail, Context as _};
use rustix::fs::{flock, FlockOperation};

pub struct InstanceLock {
    _file: File,
}

pub fn acquire(data_dir: &Path) -> anyhow::Result<InstanceLock> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("creating {}", data_dir.display()))?;
    let path = data_dir.join("LOCK");
    let file = File::create(&path).with_context(|| format!("creating {}", path.display()))?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(InstanceLock { _file: file }),
        Err(rustix::io::Errno::WOULDBLOCK) => {
            bail!(
                "another nocturnal instance already holds {} — refusing to start (two writers would corrupt the ledger)",
                path.display()
            )
        }
        Err(e) => Err(anyhow::anyhow!("flock {}: {e}", path.display())),
    }
}
