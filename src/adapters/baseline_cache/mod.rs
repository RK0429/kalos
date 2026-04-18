use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::domains::ScopeId;
use crate::domains::impact::{
    BaselineFingerprint, DependencyIndexManifest, DiffBaseline, ScopeDiagnosticSnapshot,
};
use crate::domains::metrics::ScopeMetrics;
use crate::ports::cache::CachePort;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineCacheAdapter {
    cache_dir: PathBuf,
}

impl BaselineCacheAdapter {
    pub fn new() -> Result<Self, CacheError> {
        Ok(Self {
            cache_dir: resolve_cache_dir()?,
        })
    }

    pub fn with_cache_dir(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
        }
    }

    fn cache_file_path(&self, fingerprint: &BaselineFingerprint) -> PathBuf {
        self.cache_dir
            .join("baselines")
            .join(format!("{}.json", fingerprint_cache_key(fingerprint)))
    }
}

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct StoredDiffBaseline {
    fingerprint: BaselineFingerprint,
    dependency_index: StoredDependencyIndexManifest,
    scope_metrics: Vec<StoredScopeMetricsEntry>,
    diagnostic_snapshots: Vec<StoredDiagnosticSnapshotEntry>,
    overall_score: crate::domains::metrics::OverallScore,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct StoredDependencyIndexManifest {
    reverse_dependencies: Vec<StoredReverseDependencyEntry>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct StoredReverseDependencyEntry {
    scope_id: ScopeId,
    dependents: Vec<ScopeId>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct StoredScopeMetricsEntry {
    scope_id: ScopeId,
    metrics: ScopeMetrics,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct StoredDiagnosticSnapshotEntry {
    scope_id: ScopeId,
    snapshot: ScopeDiagnosticSnapshot,
}

impl CachePort for BaselineCacheAdapter {
    type Error = CacheError;

    fn load(&self, fingerprint: &BaselineFingerprint) -> Result<Option<DiffBaseline>, Self::Error> {
        let cache_file = self.cache_file_path(fingerprint);
        let bytes = match fs::read(&cache_file) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let baseline: DiffBaseline = serde_json::from_slice::<StoredDiffBaseline>(&bytes)?.into();

        if baseline.fingerprint == *fingerprint {
            Ok(Some(baseline))
        } else {
            Ok(None)
        }
    }

    fn store(&self, baseline: &DiffBaseline) -> Result<(), Self::Error> {
        let cache_file = self.cache_file_path(&baseline.fingerprint);
        let parent = cache_file.parent().ok_or_else(|| {
            io::Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "cache file `{}` has no parent directory",
                    cache_file.display()
                ),
            )
        })?;
        fs::create_dir_all(parent)?;

        let payload = serde_json::to_vec_pretty(&StoredDiffBaseline::from(baseline.clone()))?;
        let stem = cache_file
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("baseline");
        let (temp_path, mut temp_file) = create_temp_file(parent, stem)?;
        temp_file.write_all(&payload)?;
        temp_file.sync_all()?;
        drop(temp_file);
        fs::rename(&temp_path, &cache_file)?;
        Ok(())
    }
}

impl From<DiffBaseline> for StoredDiffBaseline {
    fn from(value: DiffBaseline) -> Self {
        Self {
            fingerprint: value.fingerprint,
            dependency_index: StoredDependencyIndexManifest::from(value.dependency_index),
            scope_metrics: value
                .scope_metrics
                .into_iter()
                .map(|(scope_id, metrics)| StoredScopeMetricsEntry { scope_id, metrics })
                .collect(),
            diagnostic_snapshots: value
                .diagnostic_snapshots
                .into_iter()
                .map(|(scope_id, snapshot)| StoredDiagnosticSnapshotEntry { scope_id, snapshot })
                .collect(),
            overall_score: value.overall_score,
        }
    }
}

impl From<StoredDiffBaseline> for DiffBaseline {
    fn from(value: StoredDiffBaseline) -> Self {
        Self {
            fingerprint: value.fingerprint,
            dependency_index: value.dependency_index.into(),
            scope_metrics: value
                .scope_metrics
                .into_iter()
                .map(|entry| (entry.scope_id, entry.metrics))
                .collect(),
            diagnostic_snapshots: value
                .diagnostic_snapshots
                .into_iter()
                .map(|entry| (entry.scope_id, entry.snapshot))
                .collect(),
            overall_score: value.overall_score,
        }
    }
}

impl From<DependencyIndexManifest> for StoredDependencyIndexManifest {
    fn from(value: DependencyIndexManifest) -> Self {
        Self {
            reverse_dependencies: value
                .reverse_dependencies
                .into_iter()
                .map(|(scope_id, dependents)| StoredReverseDependencyEntry {
                    scope_id,
                    dependents: dependents.into_iter().collect(),
                })
                .collect(),
        }
    }
}

impl From<StoredDependencyIndexManifest> for DependencyIndexManifest {
    fn from(value: StoredDependencyIndexManifest) -> Self {
        Self {
            reverse_dependencies: value
                .reverse_dependencies
                .into_iter()
                .map(|entry| (entry.scope_id, entry.dependents.into_iter().collect()))
                .collect(),
        }
    }
}

pub fn resolve_cache_dir() -> Result<PathBuf, CacheError> {
    resolve_cache_dir_with(|key| env::var_os(key)).map_err(CacheError::from)
}

fn resolve_cache_dir_with<F>(get_var: F) -> io::Result<PathBuf>
where
    F: Fn(&str) -> Option<OsString>,
{
    if let Some(path) = get_var("KALOS_CACHE_DIR") {
        return Ok(PathBuf::from(path));
    }

    if let Some(path) = get_var("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(path).join("kalos"));
    }

    #[cfg(windows)]
    {
        if let Some(path) = get_var("LOCALAPPDATA") {
            return Ok(PathBuf::from(path).join("kalos"));
        }

        Err(io::Error::new(
            ErrorKind::NotFound,
            "LOCALAPPDATA is not set",
        ))
    }

    #[cfg(not(windows))]
    {
        if let Some(path) = get_var("HOME") {
            return Ok(PathBuf::from(path).join(".cache").join("kalos"));
        }

        Err(io::Error::new(ErrorKind::NotFound, "HOME is not set"))
    }
}

fn fingerprint_cache_key(fingerprint: &BaselineFingerprint) -> String {
    let mut hasher = Sha256::new();

    for (index, part) in fingerprint_parts(fingerprint).iter().enumerate() {
        if index > 0 {
            hasher.update([0]);
        }
        hasher.update(part.as_bytes());
    }

    format!("{:x}", hasher.finalize())
}

fn fingerprint_parts(fingerprint: &BaselineFingerprint) -> [&str; 7] {
    [
        fingerprint.workspace_root_hash.as_str(),
        fingerprint.base_snapshot_hash.as_str(),
        fingerprint.config_hash.as_str(),
        fingerprint.analysis_targets_hash.as_str(),
        fingerprint.rule_catalog_version.as_str(),
        fingerprint.extractor_version.as_str(),
        fingerprint.kalos_version.as_str(),
    ]
}

fn create_temp_file(directory: &Path, stem: &str) -> io::Result<(PathBuf, File)> {
    for attempt in 0..32 {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp_path = directory.join(format!(
            ".{stem}.{}.{}.tmp",
            process::id(),
            nonce + u128::from(attempt as u64)
        ));
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        ErrorKind::AlreadyExists,
        format!("failed to allocate temp file in `{}`", directory.display()),
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::{BaselineCacheAdapter, StoredDiffBaseline, resolve_cache_dir_with};
    use crate::domains::impact::{
        BaselineFingerprint, DependencyIndexManifest, DiffBaseline, ScopeDiagnosticSnapshot,
    };
    use crate::domains::metrics::{OverallScore, ScopeMetrics};
    use crate::domains::{AnalysisLevel, DiagnosticId, ScopeId};
    use crate::ports::cache::CachePort;

    #[test]
    fn baseline_cache_round_trips_matching_fingerprint() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let adapter = BaselineCacheAdapter::with_cache_dir(temp_dir.path());
        let baseline = sample_baseline(sample_fingerprint("matching"));

        adapter.store(&baseline).expect("store should succeed");
        let loaded = adapter
            .load(&baseline.fingerprint)
            .expect("load should succeed")
            .expect("baseline should exist");

        assert_eq!(loaded, baseline);
    }

    #[test]
    fn baseline_cache_returns_none_for_mismatched_payload_fingerprint() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let adapter = BaselineCacheAdapter::with_cache_dir(temp_dir.path());
        let requested_fingerprint = sample_fingerprint("requested");
        let stored = sample_baseline(sample_fingerprint("stored"));

        let cache_file = adapter.cache_file_path(&requested_fingerprint);
        fs::create_dir_all(cache_file.parent().expect("cache file should have parent"))
            .expect("cache dir should be created");
        fs::write(
            &cache_file,
            serde_json::to_vec_pretty(&StoredDiffBaseline::from(stored))
                .expect("baseline should serialize"),
        )
        .expect("baseline should be written");

        assert!(
            adapter
                .load(&requested_fingerprint)
                .expect("load should succeed")
                .is_none()
        );
    }

    #[test]
    fn baseline_cache_returns_none_when_entry_is_missing() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let adapter = BaselineCacheAdapter::with_cache_dir(temp_dir.path());

        assert!(
            adapter
                .load(&sample_fingerprint("missing"))
                .expect("load should succeed")
                .is_none()
        );
    }

    #[test]
    fn baseline_cache_store_creates_cache_directories_and_final_json_file() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let cache_root = temp_dir.path().join("nested/cache");
        let adapter = BaselineCacheAdapter::with_cache_dir(&cache_root);
        let baseline = sample_baseline(sample_fingerprint("created"));

        adapter.store(&baseline).expect("store should succeed");

        let baselines_dir = cache_root.join("baselines");
        assert!(baselines_dir.is_dir());

        let entries = fs::read_dir(&baselines_dir)
            .expect("cache dir should be readable")
            .map(|entry| entry.expect("dir entry should be readable").file_name())
            .collect::<Vec<_>>();

        assert_eq!(entries.len(), 1);
        assert!(
            entries[0]
                .to_str()
                .expect("cache filename should be utf-8")
                .ends_with(".json")
        );
    }

    #[test]
    fn resolve_cache_dir_prefers_kalos_cache_dir() {
        let resolved = resolve_cache_dir_with(|key| match key {
            "KALOS_CACHE_DIR" => Some("/tmp/kalos-cache".into()),
            "XDG_CACHE_HOME" => Some("/tmp/xdg-cache".into()),
            "HOME" => Some("/tmp/home".into()),
            _ => None,
        })
        .expect("cache dir should resolve");

        assert_eq!(resolved, PathBuf::from("/tmp/kalos-cache"));
    }

    #[cfg(not(windows))]
    #[test]
    fn resolve_cache_dir_falls_back_to_xdg_cache_home_then_home() {
        let xdg = resolve_cache_dir_with(|key| match key {
            "XDG_CACHE_HOME" => Some("/tmp/xdg-cache".into()),
            "HOME" => Some("/tmp/home".into()),
            _ => None,
        })
        .expect("xdg cache dir should resolve");
        assert_eq!(xdg, PathBuf::from("/tmp/xdg-cache/kalos"));

        let home = resolve_cache_dir_with(|key| match key {
            "HOME" => Some("/tmp/home".into()),
            _ => None,
        })
        .expect("home cache dir should resolve");
        assert_eq!(home, PathBuf::from("/tmp/home/.cache/kalos"));
    }

    #[cfg(windows)]
    #[test]
    fn resolve_cache_dir_falls_back_to_localappdata() {
        let resolved = resolve_cache_dir_with(|key| match key {
            "LOCALAPPDATA" => Some(r"C:\Users\kalos\AppData\Local".into()),
            _ => None,
        })
        .expect("local app data should resolve");

        assert_eq!(
            resolved,
            PathBuf::from(r"C:\Users\kalos\AppData\Local").join("kalos")
        );
    }

    fn sample_baseline(fingerprint: BaselineFingerprint) -> DiffBaseline {
        let scope_id = ScopeId::new(AnalysisLevel::Function, "crate::f", "src/lib.rs");

        DiffBaseline {
            fingerprint,
            dependency_index: DependencyIndexManifest {
                reverse_dependencies: BTreeMap::from([(
                    scope_id.clone(),
                    BTreeSet::from([ScopeId::new(AnalysisLevel::Project, "<project>", ".")]),
                )]),
            },
            scope_metrics: BTreeMap::from([(
                scope_id.clone(),
                ScopeMetrics {
                    scope_id: scope_id.clone(),
                    values: Vec::new(),
                    scope_risk: 0.0,
                },
            )]),
            diagnostic_snapshots: BTreeMap::from([(
                scope_id.clone(),
                ScopeDiagnosticSnapshot {
                    scope_id,
                    diagnostic_ids: vec![DiagnosticId::from("diag-1")],
                    summary: crate::domains::diagnostics::DiagnosticSummary {
                        error_count: 0,
                        warning_count: 1,
                        info_count: 0,
                    },
                },
            )]),
            overall_score: OverallScore {
                function_risk: None,
                module_risk: None,
                project_risk: Some(0.0),
                overall_risk: 0.0,
                overall_score: 100,
                function_score: None,
                module_score: None,
                project_score: Some(100),
            },
        }
    }

    fn sample_fingerprint(seed: &str) -> BaselineFingerprint {
        BaselineFingerprint {
            workspace_root_hash: format!("workspace-{seed}"),
            base_snapshot_hash: format!("base-{seed}"),
            config_hash: format!("config-{seed}"),
            analysis_targets_hash: format!("targets-{seed}"),
            rule_catalog_version: format!("rules-{seed}"),
            extractor_version: format!("extractor-{seed}"),
            kalos_version: format!("kalos-{seed}"),
        }
    }
}
