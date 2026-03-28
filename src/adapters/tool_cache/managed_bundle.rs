use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::ports::tool_cache::{ResolvedToolBundle, ToolCachePort, ToolCacheRequest};

pub const BUNDLE_MARKER_FILE: &str = "bundle.marker";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleManifest {
    pub version: String,
    pub sha256: String,
    pub download_url: String,
}

/// Returns the default CodeQL bundle manifest for managed tool cache resolution.
///
/// All values are placeholders until real CodeQL bundle integration is implemented:
/// - SHA-256 is the hash of empty input (no real bundle content yet)
/// - URL uses `example.invalid` (reserved per RFC 6761)
pub fn default_codeql_bundle_manifest() -> BundleManifest {
    BundleManifest {
        version: "2.16.0".to_owned(),
        sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_owned(),
        download_url: "https://example.invalid/codeql-bundle-2.16.0.tar.gz".to_owned(),
    }
}

#[derive(Clone, Debug)]
pub struct ManagedToolCacheAdapter {
    manifest: BundleManifest,
    cache_dir: Option<PathBuf>,
}

impl ManagedToolCacheAdapter {
    pub fn new(manifest: BundleManifest) -> Self {
        Self {
            manifest,
            cache_dir: None,
        }
    }

    pub fn with_cache_dir(manifest: BundleManifest, cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            manifest,
            cache_dir: Some(cache_dir.into()),
        }
    }

    fn cache_dir(&self) -> PathBuf {
        self.cache_dir.clone().unwrap_or_else(default_cache_dir)
    }

    fn bundle_dir(&self, version: &str) -> PathBuf {
        self.cache_dir().join("codeql").join(version)
    }
}

#[derive(Debug, Error)]
pub enum ManagedToolCacheError {
    #[error("unsupported tool cache lookup for `{tool_name}`; only `codeql` is supported")]
    UnsupportedTool { tool_name: String },
    #[error(
        "requested CodeQL bundle version `{requested}` does not match manifest version `{available}`"
    )]
    UnsupportedVersion {
        requested: String,
        available: String,
    },
    #[error(
        "CodeQL bundle `{version}` is not cached at `{path}`. Run kalos bootstrap while online before retrying offline. Bundle source: {download_url}"
    )]
    CacheMiss {
        version: String,
        path: PathBuf,
        download_url: String,
    },
    #[error(
        "CodeQL bundle `{version}` is incomplete: missing marker file `{path}`. Run kalos bootstrap while online before retrying offline. Bundle source: {download_url}"
    )]
    MarkerMissing {
        version: String,
        path: PathBuf,
        download_url: String,
    },
    #[error("failed to read cached bundle marker `{path}`: {source}")]
    ReadBundleMarker {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "cached bundle marker `{path}` checksum mismatch: expected `{expected}`, got `{actual}`"
    )]
    ChecksumMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
}

impl ToolCachePort for ManagedToolCacheAdapter {
    type Error = ManagedToolCacheError;

    fn resolve_bundle(
        &self,
        request: &ToolCacheRequest,
    ) -> Result<ResolvedToolBundle, Self::Error> {
        if request.tool_name != "codeql" {
            return Err(ManagedToolCacheError::UnsupportedTool {
                tool_name: request.tool_name.clone(),
            });
        }

        if request.version != self.manifest.version {
            return Err(ManagedToolCacheError::UnsupportedVersion {
                requested: request.version.clone(),
                available: self.manifest.version.clone(),
            });
        }

        let cache_path = self.bundle_dir(&request.version);
        if !cache_path.exists() {
            return Err(ManagedToolCacheError::CacheMiss {
                version: request.version.clone(),
                path: cache_path,
                download_url: self.manifest.download_url.clone(),
            });
        }

        let marker_path = cache_path.join(BUNDLE_MARKER_FILE);
        if !marker_path.exists() {
            return Err(ManagedToolCacheError::MarkerMissing {
                version: request.version.clone(),
                path: marker_path,
                download_url: self.manifest.download_url.clone(),
            });
        }

        let marker_bytes =
            fs::read(&marker_path).map_err(|source| ManagedToolCacheError::ReadBundleMarker {
                path: marker_path.clone(),
                source,
            })?;
        let actual = sha256_hex(&marker_bytes);
        if actual != self.manifest.sha256 {
            return Err(ManagedToolCacheError::ChecksumMismatch {
                path: marker_path,
                expected: self.manifest.sha256.clone(),
                actual,
            });
        }

        Ok(ResolvedToolBundle {
            tool_name: request.tool_name.clone(),
            version: request.version.clone(),
            cache_path,
            checksum: self.manifest.sha256.clone(),
        })
    }
}

fn default_cache_dir() -> PathBuf {
    if let Some(cache_dir) = env::var_os("KALOS_CACHE_DIR") {
        return PathBuf::from(cache_dir);
    }

    if let Some(home_dir) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
        return Path::new(&home_dir).join(".cache").join("kalos");
    }

    PathBuf::from(".cache").join("kalos")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use sha2::{Digest, Sha256};
    use tempfile::TempDir;

    use super::{BUNDLE_MARKER_FILE, BundleManifest, ManagedToolCacheAdapter};
    use crate::ports::tool_cache::{ToolCachePort, ToolCacheRequest};

    #[test]
    fn resolve_bundle_returns_cache_hit() {
        let temp = TempDir::new().unwrap();
        let bundle_dir = temp.path().join("codeql").join("2.0.0");
        fs::create_dir_all(&bundle_dir).unwrap();
        fs::write(bundle_dir.join(BUNDLE_MARKER_FILE), "bundle-ready").unwrap();
        let checksum = format!("{:x}", Sha256::digest(b"bundle-ready"));
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: checksum.clone(),
                download_url: "https://example.invalid/codeql.tgz".to_owned(),
            },
            temp.path(),
        );

        let bundle = adapter
            .resolve_bundle(&ToolCacheRequest {
                tool_name: "codeql".to_owned(),
                version: "2.0.0".to_owned(),
            })
            .unwrap();

        assert_eq!(bundle.cache_path, bundle_dir);
        assert_eq!(bundle.checksum, checksum);
    }

    #[test]
    fn resolve_bundle_returns_clear_cache_miss() {
        let temp = TempDir::new().unwrap();
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: "0".repeat(64),
                download_url: "https://example.invalid/codeql.tgz".to_owned(),
            },
            temp.path(),
        );

        let error = adapter
            .resolve_bundle(&ToolCacheRequest {
                tool_name: "codeql".to_owned(),
                version: "2.0.0".to_owned(),
            })
            .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("bootstrap"));
        assert!(message.contains("offline"));
    }

    #[test]
    fn resolve_bundle_detects_checksum_mismatch() {
        let temp = TempDir::new().unwrap();
        let bundle_dir = temp.path().join("codeql").join("2.0.0");
        fs::create_dir_all(&bundle_dir).unwrap();
        fs::write(bundle_dir.join(BUNDLE_MARKER_FILE), "bundle-ready").unwrap();
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: "f".repeat(64),
                download_url: "https://example.invalid/codeql.tgz".to_owned(),
            },
            temp.path(),
        );

        let error = adapter
            .resolve_bundle(&ToolCacheRequest {
                tool_name: "codeql".to_owned(),
                version: "2.0.0".to_owned(),
            })
            .unwrap_err();

        assert!(error.to_string().contains("checksum mismatch"));
    }
}
