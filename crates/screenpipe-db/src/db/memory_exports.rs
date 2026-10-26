// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

//! Managed, owner-requested index snapshots. Only export and destructive/privacy
//! operations acquire this gate; ordinary recording has no additional work.

use super::DatabaseManager;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

type Gates = Mutex<HashMap<PathBuf, Weak<AsyncMutex<()>>>>;
static EXPORT_GATES: OnceLock<Gates> = OnceLock::new();

impl DatabaseManager {
    pub fn memory_export_directory(&self) -> Result<PathBuf, sqlx::Error> {
        if let Some(storage) = &self.storage {
            return Ok(storage.root.join("memory").join("exports"));
        }
        let options = self.pool.connect_options();
        let file = options.get_filename();
        let root = file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| sqlx::Error::Protocol("memory_export_directory_unavailable".into()))?;
        Ok(root.join("memory").join("exports"))
    }

    pub async fn memory_export_gate(&self) -> Result<OwnedMutexGuard<()>, sqlx::Error> {
        let identity = self
            .memory_export_directory()
            .unwrap_or_else(|_| self.pool.connect_options().get_filename().to_owned());
        gate_for_directory(identity).await
    }

    /// Hold the returned guard until the corresponding mutation has committed.
    /// Removing snapshots first makes a crash conservative: captures and authored
    /// records survive, while stale exported content cannot be republished in
    /// the gap before the privacy/deletion transaction.
    pub async fn revoke_memory_exports(&self) -> Result<OwnedMutexGuard<()>, sqlx::Error> {
        let guard = self.memory_export_gate().await?;
        self.clear_memory_exports_locked().await?;
        Ok(guard)
    }

    /// Background privacy publication for a newly captured source cannot revoke
    /// a snapshot that never referenced it. Existing indexed sources still
    /// revoke before replacement; this never runs in a capture callback.
    pub(crate) async fn memory_export_source_guard(
        &self,
        kind: &str,
        id: i64,
    ) -> Result<OwnedMutexGuard<()>, sqlx::Error> {
        let guard = self.memory_export_gate().await?;
        let referenced: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM memory_evidence WHERE source_type=? AND source_id=?)",
        )
        .bind(kind)
        .bind(id)
        .fetch_one(&self.pool)
        .await?;
        if referenced {
            self.clear_memory_exports_locked().await?;
        }
        Ok(guard)
    }

    /// Transcription reconciliation runs after the audio bytes are durable,
    /// outside device callbacks. Only replacement of an indexed transcript can
    /// invalidate a snapshot; a new chunk leaves earlier snapshots intact.
    pub(crate) async fn memory_export_audio_guard(
        &self,
        chunk: i64,
    ) -> Result<OwnedMutexGuard<()>, sqlx::Error> {
        let guard = self.memory_export_gate().await?;
        let referenced: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM audio_transcriptions a JOIN memory_evidence e ON e.source_type='audio' AND e.source_id=a.id WHERE a.audio_chunk_id=?)",
        )
        .bind(chunk)
        .fetch_one(&self.pool)
        .await?;
        if referenced {
            self.clear_memory_exports_locked().await?;
        }
        Ok(guard)
    }

    /// Caller owns memory_export_gate, before acquiring the database writer.
    pub(crate) async fn clear_memory_exports_locked(&self) -> Result<(), sqlx::Error> {
        match self.memory_export_directory() {
            Ok(directory) => clear_directory(directory).await,
            // In-memory databases have no managed filesystem export surface.
            Err(_) => Ok(()),
        }
    }
}

/// Offline vault setup/locking shares the managed snapshot lifecycle without
/// opening a second database writer. The caller keeps the guard through setup.
pub async fn revoke_memory_exports_at(root: &Path) -> Result<OwnedMutexGuard<()>, sqlx::Error> {
    let directory = root.join("memory").join("exports");
    let guard = gate_for_directory(directory.clone()).await?;
    clear_directory(directory).await?;
    Ok(guard)
}

async fn gate_for_directory(path: PathBuf) -> Result<OwnedMutexGuard<()>, sqlx::Error> {
    let gate = {
        let mut gates = EXPORT_GATES
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| sqlx::Error::Protocol("memory_export_gate_unavailable".into()))?;
        if let Some(gate) = gates.get(&path).and_then(Weak::upgrade) {
            gate
        } else {
            let gate = Arc::new(AsyncMutex::new(()));
            gates.insert(path, Arc::downgrade(&gate));
            gate
        }
    };
    Ok(gate.lock_owned().await)
}

async fn clear_directory(directory: PathBuf) -> Result<(), sqlx::Error> {
    // Retry interrupted revocations before admitting another mutation.
    // A successful rename alone never certifies that the content was erased.
    let parent = directory.parent().expect("managed export parent");
    match tokio::fs::symlink_metadata(parent).await {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(sqlx::Error::Protocol(
                "invalid_memory_export_directory".into(),
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    match tokio::fs::read_dir(parent).await {
        Ok(mut entries) => {
            while let Some(entry) = entries.next_entry().await? {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if name
                    .strip_prefix(".exports-revoked-")
                    .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
                {
                    let kind = entry.file_type().await?;
                    if !kind.is_dir() || kind.is_symlink() {
                        return Err(sqlx::Error::Protocol(
                            "invalid_memory_export_directory".into(),
                        ));
                    }
                    tokio::fs::remove_dir_all(entry.path()).await?;
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    match tokio::fs::symlink_metadata(&directory).await {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            let revoked =
                directory.with_file_name(format!(".exports-revoked-{}", uuid::Uuid::new_v4()));
            tokio::fs::rename(&directory, &revoked).await?;
            tokio::fs::remove_dir_all(&revoked).await?;
        }
        Ok(_) => {
            return Err(sqlx::Error::Protocol(
                "invalid_memory_export_directory".into(),
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
