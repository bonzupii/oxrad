// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//! Persistent response cache for downloaded radar assets.
//!
//! A store is a directory of bodies and metadata sidecars. Every body is
//! verified against its SHA-256 digest before use. Writes use a temporary file
//! followed by a rename, and pruning keeps the directory within an entry and
//! byte budget.

use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, SystemTimeError};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::CachePolicy;
use crate::error::PathError;
use crate::paths;

const SCHEMA_VERSION: u64 = 2;
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// What one prune removed and the budget it enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pruned {
    pub removed: usize,
    pub max_entries: usize,
    pub max_bytes: u64,
}

#[derive(Debug)]
pub enum CacheError {
    Path {
        source: PathError,
    },
    Create {
        path: String,
        source: std::io::Error,
    },
    Read {
        path: String,
        source: std::io::Error,
    },
    Write {
        path: String,
        source: std::io::Error,
    },
    Json {
        path: String,
        source: serde_json::Error,
    },
    Corrupt {
        key: String,
        detail: &'static str,
    },
    TooLarge {
        key: String,
        bytes: usize,
    },
    Clock {
        source: SystemTimeError,
    },
}

impl CacheError {
    fn message(&self) -> String {
        match self {
            Self::Path { source } => source.to_string(),
            Self::Create { path, source } => {
                format!("The cache directory {path} could not be created: {source}")
            }
            Self::Read { path, source } => {
                format!("The cache file {path} could not be read: {source}")
            }
            Self::Write { path, source } => {
                format!("The cache file {path} could not be written: {source}")
            }
            Self::Json { path, source } => {
                format!("The cache metadata {path} is not valid JSON: {source}")
            }
            Self::Corrupt { key, detail } => {
                format!("The cached response for {key} failed integrity checking: {detail}.")
            }
            Self::TooLarge { key, bytes } => {
                format!("The response for {key} is {bytes} bytes, too large to cache.")
            }
            Self::Clock { source } => {
                format!("The system clock could not time a cache entry: {source}")
            }
        }
    }
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for CacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Path { source } => Some(source),
            Self::Create { source, .. }
            | Self::Read { source, .. }
            | Self::Write { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            Self::Clock { source } => Some(source),
            Self::Corrupt { .. } | Self::TooLarge { .. } => None,
        }
    }
}

impl From<PathError> for CacheError {
    fn from(source: PathError) -> Self {
        Self::Path { source }
    }
}

impl From<SystemTimeError> for CacheError {
    fn from(source: SystemTimeError) -> Self {
        Self::Clock { source }
    }
}

/// The HTTP validators an origin supplied with a body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Validators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// A verified cached body and its retrieval metadata.
#[derive(Debug)]
pub struct Entry {
    pub body: Vec<u8>,
    pub fetched_at_unix_ms: u64,
    pub validators: Validators,
}

/// A directory-backed response cache.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Opens the shared cache below `~/.oxrad/cache`.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Path`] when the home directory is unavailable.
    pub fn default_location() -> Result<Self, CacheError> {
        Ok(Self {
            root: paths::cache_directory()?.join("responses").join("v1"),
        })
    }

    /// Uses an explicit cache namespace directory.
    #[cfg(test)]
    pub const fn at(root: PathBuf) -> Self {
        Self { root }
    }

    /// The directory this store holds bodies and metadata in.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn digest(key: &str) -> String {
        let digest = Sha256::digest(key.as_bytes());
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn stem(key: &str) -> String {
        Self::digest(key)
    }

    fn meta_path(&self, key: &str) -> PathBuf {
        self.root.join(format!("{}.meta.json", Self::stem(key)))
    }

    fn body_path(&self, key: &str, body_digest: &str) -> PathBuf {
        self.root
            .join(format!("{}.{body_digest}.body", Self::stem(key)))
    }

    /// Reads and verifies one cached body. An absent entry is a miss.
    ///
    /// # Errors
    ///
    /// Returns a typed cache error when metadata, the body, or its digest is
    /// invalid. The caller can remove the entry and refetch it.
    #[cfg(test)]
    pub fn load(&self, key: &str) -> Result<Option<Entry>, CacheError> {
        self.load_aged(key)
    }

    /// Reads a cached body together with the instant it was fetched.
    ///
    /// # Errors
    ///
    /// Returns the same typed errors as [`Self::load`].
    pub fn load_aged(&self, key: &str) -> Result<Option<Entry>, CacheError> {
        let meta_path = self.meta_path(key);
        let text = match fs::read_to_string(&meta_path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(CacheError::Read {
                    path: meta_path.display().to_string(),
                    source,
                });
            }
        };
        let metadata: Value = serde_json::from_str(&text).map_err(|source| CacheError::Json {
            path: meta_path.display().to_string(),
            source,
        })?;
        if metadata.get("schema").and_then(Value::as_u64) != Some(SCHEMA_VERSION) {
            return Err(CacheError::Corrupt {
                key: String::from(key),
                detail: "unsupported metadata schema",
            });
        }
        if metadata.get("key").and_then(Value::as_str) != Some(key) {
            return Err(CacheError::Corrupt {
                key: String::from(key),
                detail: "metadata belongs to another key",
            });
        }
        let stored_len = metadata
            .get("body_len")
            .and_then(Value::as_u64)
            .ok_or_else(|| CacheError::Corrupt {
                key: String::from(key),
                detail: "missing or invalid body length",
            })?;
        let expected_len = usize::try_from(stored_len).map_err(|_| CacheError::Corrupt {
            key: String::from(key),
            detail: "body length exceeds the platform limit",
        })?;
        let fetched_at_unix_ms = metadata
            .get("fetched_at_unix_ms")
            .and_then(Value::as_u64)
            .ok_or_else(|| CacheError::Corrupt {
                key: String::from(key),
                detail: "missing fetch time",
            })?;
        let expected_digest = metadata
            .get("body_sha256")
            .and_then(Value::as_str)
            .ok_or_else(|| CacheError::Corrupt {
                key: String::from(key),
                detail: "missing body digest",
            })?;
        if expected_digest.len() != 64
            || !expected_digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(CacheError::Corrupt {
                key: String::from(key),
                detail: "body digest is not SHA-256 hexadecimal",
            });
        }
        let body_path = self.body_path(key, expected_digest);
        let body = fs::read(&body_path).map_err(|source| CacheError::Read {
            path: body_path.display().to_string(),
            source,
        })?;
        if body.len() != expected_len {
            return Err(CacheError::Corrupt {
                key: String::from(key),
                detail: "body length does not match metadata",
            });
        }
        let actual_digest = Self::digest_body(&body);
        if actual_digest != expected_digest {
            return Err(CacheError::Corrupt {
                key: String::from(key),
                detail: "body digest does not match metadata",
            });
        }
        let validators = Validators {
            etag: metadata
                .get("etag")
                .and_then(Value::as_str)
                .map(String::from),
            last_modified: metadata
                .get("last_modified")
                .and_then(Value::as_str)
                .map(String::from),
        };
        Ok(Some(Entry {
            body,
            fetched_at_unix_ms,
            validators,
        }))
    }

    fn digest_body(body: &[u8]) -> String {
        let digest = Sha256::digest(body);
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Stores a body and its metadata. The metadata file is the commit point.
    ///
    /// # Errors
    ///
    /// Returns a typed cache error for oversized bodies or failed writes.
    #[cfg(test)]
    pub fn store(&self, key: &str, body: &[u8]) -> Result<(), CacheError> {
        self.store_with(key, body, &Validators::default())
    }

    /// Stores a body with the validators that can revalidate it later.
    ///
    /// The body is written to a digest-named file first. The metadata file is
    /// the single commit point, so a failure before that rename leaves the
    /// previous entry, if any, entirely readable.
    ///
    /// # Errors
    ///
    /// Returns a typed cache error for oversized bodies or failed writes.
    pub fn store_with(
        &self,
        key: &str,
        body: &[u8],
        validators: &Validators,
    ) -> Result<(), CacheError> {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_millis();
        let now = u64::try_from(now).map_err(|_| CacheError::Corrupt {
            key: String::from(key),
            detail: "fetch time exceeds the supported range",
        })?;
        self.store_with_at(key, body, now, validators)
    }

    #[cfg(test)]
    fn store_at(&self, key: &str, body: &[u8], fetched_at_unix_ms: u64) -> Result<(), CacheError> {
        self.store_with_at(key, body, fetched_at_unix_ms, &Validators::default())
    }

    fn store_with_at(
        &self,
        key: &str,
        body: &[u8],
        fetched_at_unix_ms: u64,
        validators: &Validators,
    ) -> Result<(), CacheError> {
        if body.len() > MAX_BODY_BYTES {
            return Err(CacheError::TooLarge {
                key: String::from(key),
                bytes: body.len(),
            });
        }
        fs::create_dir_all(&self.root).map_err(|source| CacheError::Create {
            path: self.root.display().to_string(),
            source,
        })?;
        let meta_path = self.meta_path(key);
        let previous_digest = self.stored_digest(key);
        let body_digest = Self::digest_body(body);
        let body_path = self.body_path(key, &body_digest);
        let metadata = json!({
            "schema": SCHEMA_VERSION,
            "key": key,
            "fetched_at_unix_ms": fetched_at_unix_ms,
            "body_len": body.len(),
            "body_sha256": body_digest,
            "etag": validators.etag,
            "last_modified": validators.last_modified,
        });
        let encoded = serde_json::to_vec(&metadata).map_err(|source| CacheError::Json {
            path: meta_path.display().to_string(),
            source,
        })?;
        self.write_atomic(&body_path, body)?;
        self.write_atomic(&meta_path, &encoded)?;
        if let Some(previous) = previous_digest
            && previous != body_digest
        {
            let _ = fs::remove_file(self.body_path(key, &previous));
        }
        Ok(())
    }

    fn stored_digest(&self, key: &str) -> Option<String> {
        let text = fs::read_to_string(self.meta_path(key)).ok()?;
        let metadata: Value = serde_json::from_str(&text).ok()?;
        metadata
            .get("body_sha256")
            .and_then(Value::as_str)
            .map(String::from)
    }

    /// Marks a cached body freshly revalidated without rewriting the body.
    ///
    /// Returns `false` when there is no metadata to update. The body digest is
    /// unchanged, so a repeated 304 keeps the same verified bytes.
    ///
    /// # Errors
    ///
    /// Returns a typed cache error when the metadata cannot be read or written.
    pub fn revalidated(&self, key: &str, validators: &Validators) -> Result<bool, CacheError> {
        let meta_path = self.meta_path(key);
        let text = match fs::read_to_string(&meta_path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(source) => {
                return Err(CacheError::Read {
                    path: meta_path.display().to_string(),
                    source,
                });
            }
        };
        let mut metadata: Value =
            serde_json::from_str(&text).map_err(|source| CacheError::Json {
                path: meta_path.display().to_string(),
                source,
            })?;
        if metadata.get("schema").and_then(Value::as_u64) != Some(SCHEMA_VERSION)
            || metadata.get("key").and_then(Value::as_str) != Some(key)
        {
            return Err(CacheError::Corrupt {
                key: String::from(key),
                detail: "metadata cannot be revalidated",
            });
        }
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_millis();
        let now = u64::try_from(now).map_err(|_| CacheError::Corrupt {
            key: String::from(key),
            detail: "fetch time exceeds the supported range",
        })?;
        if let Some(object) = metadata.as_object_mut() {
            object.insert(String::from("fetched_at_unix_ms"), json!(now));
            if let Some(etag) = &validators.etag {
                object.insert(String::from("etag"), json!(etag));
            }
            if let Some(modified) = &validators.last_modified {
                object.insert(String::from("last_modified"), json!(modified));
            }
        }
        let encoded = serde_json::to_vec(&metadata).map_err(|source| CacheError::Json {
            path: meta_path.display().to_string(),
            source,
        })?;
        self.write_atomic(&meta_path, &encoded)?;
        Ok(true)
    }

    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<(), CacheError> {
        let parent = path.parent().ok_or_else(|| CacheError::Write {
            path: path.display().to_string(),
            source: std::io::Error::new(ErrorKind::InvalidInput, "cache path has no parent"),
        })?;
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| CacheError::Write {
                path: path.display().to_string(),
                source: std::io::Error::new(ErrorKind::InvalidInput, "cache path has no name"),
            })?;
        let stamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos();
        let temporary = parent.join(format!(".{name}.{}.{stamp}.tmp", std::process::id()));
        fs::write(&temporary, bytes).map_err(|source| CacheError::Write {
            path: temporary.display().to_string(),
            source,
        })?;
        match fs::rename(&temporary, path) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == ErrorKind::AlreadyExists => {
                fs::remove_file(path).map_err(|source| CacheError::Write {
                    path: path.display().to_string(),
                    source,
                })?;
                fs::rename(&temporary, path).map_err(|source| CacheError::Write {
                    path: path.display().to_string(),
                    source,
                })
            }
            Err(source) => Err(CacheError::Write {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    /// Removes one cached entry. A missing entry is not an error.
    ///
    /// # Errors
    ///
    /// Returns a typed cache error when an existing file cannot be removed.
    pub fn remove(&self, key: &str) -> Result<(), CacheError> {
        let stem = Self::stem(key);
        let prefix = format!("{stem}.");
        if let Ok(directory) = fs::read_dir(&self.root) {
            for item in directory {
                let item = item.map_err(|source| CacheError::Read {
                    path: self.root.display().to_string(),
                    source,
                })?;
                let path = item.path();
                let remove = path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".body"));
                if remove {
                    match fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == ErrorKind::NotFound => {}
                        Err(source) => {
                            return Err(CacheError::Write {
                                path: path.display().to_string(),
                                source,
                            });
                        }
                    }
                }
            }
        }
        let meta_path = self.meta_path(key);
        match fs::remove_file(&meta_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(source) => Err(CacheError::Write {
                path: meta_path.display().to_string(),
                source,
            }),
        }
    }

    /// Enforces the configured entry and byte budget.
    ///
    /// The budget travels in from the effective configuration; the store holds
    /// no limit of its own, so the printed budget and the enforced budget are
    /// the same value.
    ///
    /// # Errors
    ///
    /// Returns a typed cache error when the directory cannot be inspected or
    /// an entry cannot be removed.
    pub fn prune(&self, policy: CachePolicy) -> Result<Pruned, CacheError> {
        self.prune_to(policy.max_entries(), policy.max_bytes())
    }

    fn prune_to(&self, max_entries: usize, max_bytes: u64) -> Result<Pruned, CacheError> {
        let directory = match fs::read_dir(&self.root) {
            Ok(directory) => directory,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(Pruned {
                    removed: 0,
                    max_entries,
                    max_bytes,
                });
            }
            Err(source) => {
                return Err(CacheError::Read {
                    path: self.root.display().to_string(),
                    source,
                });
            }
        };
        let mut entries: Vec<(u64, u64, PathBuf, PathBuf)> = Vec::new();
        for item in directory {
            let item = item.map_err(|source| CacheError::Read {
                path: self.root.display().to_string(),
                source,
            })?;
            let path = item.path();
            let name = match path.file_name().and_then(OsStr::to_str) {
                Some(name) => name,
                None => continue,
            };
            let Some(stem) = name.strip_suffix(".meta.json") else {
                continue;
            };
            let text = fs::read_to_string(&path).map_err(|source| CacheError::Read {
                path: path.display().to_string(),
                source,
            })?;
            let metadata: Value =
                serde_json::from_str(&text).map_err(|source| CacheError::Json {
                    path: path.display().to_string(),
                    source,
                })?;
            let fetched = metadata
                .get("fetched_at_unix_ms")
                .and_then(Value::as_u64)
                .ok_or_else(|| CacheError::Corrupt {
                    key: String::from(stem),
                    detail: "missing fetch time during pruning",
                })?;
            let length = metadata
                .get("body_len")
                .and_then(Value::as_u64)
                .ok_or_else(|| CacheError::Corrupt {
                    key: String::from(stem),
                    detail: "missing or invalid body length during pruning",
                })?;
            let body_digest = metadata
                .get("body_sha256")
                .and_then(Value::as_str)
                .ok_or_else(|| CacheError::Corrupt {
                    key: String::from(stem),
                    detail: "missing body digest during pruning",
                })?;
            let body = path.with_file_name(format!("{stem}.{body_digest}.body"));
            entries.push((fetched, length, body, path));
        }
        entries.sort_by_key(|entry| entry.0);
        let mut total: u64 = entries.iter().map(|entry| entry.1).sum();
        let mut count = entries.len();
        let mut removed = 0_usize;
        for (_, length, body, meta) in entries {
            if count <= max_entries && total <= max_bytes {
                break;
            }
            for path in [body, meta] {
                fs::remove_file(&path).map_err(|source| CacheError::Write {
                    path: path.display().to_string(),
                    source,
                })?;
            }
            total = total.saturating_sub(length);
            count = count.saturating_sub(1);
            removed = removed.saturating_add(1);
        }
        Ok(Pruned {
            removed,
            max_entries,
            max_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestError;

    fn cleanup(root: &Path) -> Result<(), TestError> {
        fs::remove_dir_all(root).map_err(|source| CacheError::Write {
            path: root.display().to_string(),
            source,
        })?;
        Ok(())
    }

    fn directory(name: &str) -> Result<PathBuf, TestError> {
        let stamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("oxrad-cache-{name}-{}-{stamp}", std::process::id()));
        fs::create_dir_all(&root).map_err(|source| CacheError::Create {
            path: root.display().to_string(),
            source,
        })?;
        Ok(root)
    }

    #[test]
    fn a_stored_body_round_trips_with_its_metadata() -> Result<(), TestError> {
        let root = directory("round-trip")?;
        let store = Store::at(root.clone());
        store.store("https://example.test/a.png", b"png bytes")?;
        let entry = store
            .load("https://example.test/a.png")?
            .ok_or("cache miss after store")?;
        assert_eq!(entry.body, b"png bytes");
        cleanup(&root)?;
        Ok(())
    }

    #[test]
    fn a_changed_body_is_rejected() -> Result<(), TestError> {
        let root = directory("corrupt")?;
        let store = Store::at(root.clone());
        store.store("tile", b"original")?;
        let metadata = fs::read_to_string(store.meta_path("tile"))
            .map_err(|error| TestError::from(error.to_string()))?;
        let metadata: Value =
            serde_json::from_str(&metadata).map_err(|error| TestError::from(error.to_string()))?;
        let digest = metadata
            .get("body_sha256")
            .and_then(Value::as_str)
            .ok_or("stored metadata has no digest")?;
        let body = store.body_path("tile", digest);
        fs::write(body, b"changed").map_err(|source| CacheError::Write {
            path: String::from("test body"),
            source,
        })?;
        assert!(matches!(
            store.load("tile"),
            Err(CacheError::Corrupt { .. })
        ));
        cleanup(&root)?;
        Ok(())
    }

    #[test]
    fn a_truncated_body_is_rejected() -> Result<(), TestError> {
        let root = directory("truncated")?;
        let store = Store::at(root.clone());
        store.store("tile", b"original bytes")?;
        let metadata = fs::read_to_string(store.meta_path("tile"))
            .map_err(|error| TestError::from(error.to_string()))?;
        let metadata: Value =
            serde_json::from_str(&metadata).map_err(|error| TestError::from(error.to_string()))?;
        let digest = metadata
            .get("body_sha256")
            .and_then(Value::as_str)
            .ok_or("stored metadata has no digest")?;
        fs::write(store.body_path("tile", digest), b"orig").map_err(|source| {
            CacheError::Write {
                path: String::from("test body"),
                source,
            }
        })?;
        assert!(matches!(
            store.load("tile"),
            Err(CacheError::Corrupt { .. })
        ));
        cleanup(&root)?;
        Ok(())
    }

    #[test]
    fn pruning_rejects_metadata_without_a_valid_body_length() -> Result<(), TestError> {
        let root = directory("invalid-length")?;
        let store = Store::at(root.clone());
        store.store_at("broken", b"data", 1)?;
        let metadata_path = store.meta_path("broken");
        fs::write(&metadata_path, br#"{"fetched_at_unix_ms":1}"#).map_err(|source| {
            CacheError::Write {
                path: metadata_path.display().to_string(),
                source,
            }
        })?;
        assert!(matches!(
            store.prune_to(1, 1024),
            Err(CacheError::Corrupt { key, detail })
                if key == Store::digest("broken")
                    && detail == "missing or invalid body length during pruning"
        ));
        cleanup(&root)?;
        Ok(())
    }

    #[test]
    fn pruning_honours_a_configured_byte_budget() -> Result<(), TestError> {
        let root = directory("budget")?;
        let store = Store::at(root.clone());
        let body = vec![7_u8; 1024 * 1024];
        for index in 0..17 {
            store.store(&format!("body-{index}"), &body)?;
        }
        let Ok(small) = CachePolicy::from_mib(8) else {
            return Err("8 MiB must be an admissible budget".into());
        };
        let pruned = store.prune(small)?;
        assert_eq!(pruned.max_bytes, 8 * 1024 * 1024);
        assert_eq!(pruned.max_entries, 256);
        assert_eq!(pruned.removed, 9, "17 MiB must fall to the 8 MiB budget");
        assert_eq!(
            store.prune(small)?.removed,
            0,
            "a second prune has nothing to do"
        );
        let Ok(large) = CachePolicy::from_mib(4096) else {
            return Err("4096 MiB must be an admissible budget".into());
        };
        assert_eq!(
            store.prune(large)?.removed,
            0,
            "a large budget evicts nothing"
        );
        cleanup(&root)?;
        Ok(())
    }

    #[test]
    fn pruning_removes_oldest_entries_first() -> Result<(), TestError> {
        let root = directory("prune")?;
        let store = Store::at(root.clone());
        store.store_at("old", b"old", 1)?;
        store.store_at("middle", b"middle", 2)?;
        store.store_at("new", b"new", 3)?;
        let pruned = store.prune_to(2, 1024)?;
        assert_eq!(pruned.removed, 1);
        assert!(store.load("old")?.is_none());
        assert!(store.load("middle")?.is_some());
        assert!(store.load("new")?.is_some());
        cleanup(&root)?;
        Ok(())
    }
}
