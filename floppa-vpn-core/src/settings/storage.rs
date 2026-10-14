use super::{Settings, SettingsCapabilities};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::{Path, PathBuf};

const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct SettingsDocument {
    pub schema_version: u32,
    pub revision: u32,
    #[serde(flatten)]
    pub settings: Settings,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("settings I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid settings document: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported settings schema {0}")]
    Version(u32),
    #[error("settings conflict: expected revision {expected}, current revision {actual}")]
    Conflict { expected: u32, actual: u32 },
    #[error("invalid settings: {0}")]
    Invalid(String),
    #[error("this deployment does not support {0}")]
    Unsupported(&'static str),
    #[error("none of the selected protocols has an available configuration")]
    NoAvailableProtocol,
}

/// An owner-scoped store, not a concurrent file editing API. The caller must
/// hold the ownership guard for its lifetime and serialize commands to it.
/// Construction never changes files; creating defaults is an explicit operation.
#[derive(Debug)]
pub struct SettingsStore {
    path: PathBuf,
    capabilities: SettingsCapabilities,
}

impl SettingsStore {
    pub fn new(path: PathBuf, capabilities: SettingsCapabilities) -> Self {
        Self { path, capabilities }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn read(&self) -> Result<Option<SettingsDocument>, SettingsError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let document: SettingsDocument = serde_json::from_slice(&bytes)?;
        if document.schema_version != VERSION {
            return Err(SettingsError::Version(document.schema_version));
        }
        if document.revision == 0 {
            return Err(SettingsError::Invalid("revision must be positive".into()));
        }
        document.settings.validate(self.capabilities)?;
        Ok(Some(document))
    }

    /// Revision zero means creating a new document. Never treat an unreadable or
    /// invalid file as absent. Do not cache a successful write before it commits.
    pub fn replace(
        &mut self,
        expected_revision: u32,
        settings: Settings,
    ) -> Result<SettingsDocument, SettingsError> {
        let current = self.read()?.map_or(0, |document| document.revision);
        if current != expected_revision {
            return Err(SettingsError::Conflict {
                expected: expected_revision,
                actual: current,
            });
        }
        settings.validate(self.capabilities)?;
        let revision = current
            .checked_add(1)
            .ok_or_else(|| SettingsError::Invalid("revision exhausted".into()))?;
        let document = SettingsDocument {
            schema_version: VERSION,
            revision,
            settings,
        };
        let bytes = serde_json::to_vec_pretty(&document)?;
        crate::private_file::write_private(&self.path, &bytes)?;
        // Persist the directory entry after the atomic replacement on Unix.
        // On failure the caller receives an error and must re-read; it must not
        // assume the rename was rolled back or retry with stale revision data.
        #[cfg(unix)]
        std::fs::File::open(
            self.path
                .parent()
                .ok_or_else(|| SettingsError::Invalid("missing settings parent".into()))?,
        )?
        .sync_all()?;
        Ok(document)
    }
}
