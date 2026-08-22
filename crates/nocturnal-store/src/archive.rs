//! Off-site archive for compacted history.
//!
//! The WAL stays local — it is the fsync hot path — but Parquet partitions are
//! immutable once written, which makes them a natural fit for object storage
//! (Hetzner Object Storage, or any S3-compatible endpoint; the same shape
//! Ourios uses for its own Parquet).
//!
//! Two directions, both simple because the data is immutable:
//! * **write-through** — a partition is uploaded only after it has been
//!   written, fsynced and *verified readable* locally, so the archive never
//!   holds a file the local store rejected;
//! * **read-through on boot** — any partition present remotely but missing
//!   locally is downloaded before replay, so the data directory rebuilds
//!   itself on a fresh disk and the VM stops being a single point of failure.
//!
//! Credentials and endpoint come from the standard AWS environment
//! (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_ENDPOINT_URL_S3` or
//! `AWS_ENDPOINT`, `AWS_REGION`); only the bucket and prefix — for which no
//! standard variable exists — are ours to configure.

use std::path::Path;
use std::sync::Arc;

use futures::StreamExt as _;
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, PutPayload};

/// Where compacted partitions are mirrored.
#[derive(Clone)]
pub struct Archive {
    store: Arc<dyn ObjectStore>,
    prefix: String,
}

impl std::fmt::Debug for Archive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Archive")
            .field("prefix", &self.prefix)
            .finish()
    }
}

impl Archive {
    /// S3-compatible archive: bucket from config, everything else from the
    /// standard AWS environment.
    pub fn s3(bucket: &str, prefix: &str) -> Result<Archive, object_store::Error> {
        let store = object_store::aws::AmazonS3Builder::from_env()
            .with_bucket_name(bucket)
            .build()?;
        Ok(Archive {
            store: Arc::new(store),
            prefix: prefix.trim_matches('/').to_owned(),
        })
    }

    /// Any object store — used by tests with a local filesystem double.
    pub fn with_store(store: Arc<dyn ObjectStore>, prefix: &str) -> Archive {
        Archive {
            store,
            prefix: prefix.trim_matches('/').to_owned(),
        }
    }

    fn object_path(&self, file_name: &str) -> ObjectPath {
        if self.prefix.is_empty() {
            ObjectPath::from(format!("events/{file_name}"))
        } else {
            ObjectPath::from(format!("{}/events/{file_name}", self.prefix))
        }
    }

    /// Upload one compacted partition, overwriting any earlier copy (a
    /// partition is rewritten whenever a later month's events arrive for it).
    pub async fn put_partition(&self, local: &Path) -> Result<(), object_store::Error> {
        let file_name = local.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
            object_store::Error::Generic {
                store: "archive",
                source: "partition has no file name".into(),
            }
        })?;
        let bytes = std::fs::read(local).map_err(|e| object_store::Error::Generic {
            store: "archive",
            source: Box::new(e),
        })?;
        self.store
            .put(&self.object_path(file_name), PutPayload::from(bytes))
            .await?;
        Ok(())
    }

    /// Partition file names held in the archive.
    pub async fn list_partitions(&self) -> Result<Vec<String>, object_store::Error> {
        let prefix = if self.prefix.is_empty() {
            ObjectPath::from("events")
        } else {
            ObjectPath::from(format!("{}/events", self.prefix))
        };
        let mut names = Vec::new();
        let mut listing = self.store.list(Some(&prefix));
        while let Some(meta) = listing.next().await {
            let meta = meta?;
            if let Some(name) = meta.location.filename() {
                if name.ends_with(".parquet") {
                    names.push(name.to_owned());
                }
            }
        }
        names.sort();
        Ok(names)
    }

    /// Download a partition into the local events directory.
    pub async fn get_partition(
        &self,
        file_name: &str,
        events_dir: &Path,
    ) -> Result<(), object_store::Error> {
        let bytes = self
            .store
            .get(&self.object_path(file_name))
            .await?
            .bytes()
            .await?;
        let tmp = events_dir.join(format!(".tmp-{file_name}"));
        let target = events_dir.join(file_name);
        std::fs::write(&tmp, &bytes).map_err(|e| object_store::Error::Generic {
            store: "archive",
            source: Box::new(e),
        })?;
        std::fs::rename(&tmp, &target).map_err(|e| object_store::Error::Generic {
            store: "archive",
            source: Box::new(e),
        })?;
        Ok(())
    }

    /// Pull down every partition the local directory is missing. Returns the
    /// names restored, so a boot can say what it fetched.
    pub async fn restore_missing(
        &self,
        events_dir: &Path,
    ) -> Result<Vec<String>, object_store::Error> {
        let mut restored = Vec::new();
        for name in self.list_partitions().await? {
            if !events_dir.join(&name).exists() {
                self.get_partition(&name, events_dir).await?;
                restored.push(name);
            }
        }
        Ok(restored)
    }
}
