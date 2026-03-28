use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use tar::Archive;
use thiserror::Error;

use crate::ports::tool_cache::{ResolvedToolBundle, ToolCachePort, ToolCacheRequest};

pub const BUNDLE_MARKER_FILE: &str = "bundle.marker";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    LinuxX64,
    LinuxArm64,
    MacosX64,
    MacosArm64,
    WindowsX64,
}

impl Platform {
    pub fn detect() -> Option<Self> {
        if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
            Some(Self::LinuxX64)
        } else if cfg!(target_os = "linux") && cfg!(target_arch = "aarch64") {
            Some(Self::LinuxArm64)
        } else if cfg!(target_os = "macos") && cfg!(target_arch = "x86_64") {
            Some(Self::MacosX64)
        } else if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
            Some(Self::MacosArm64)
        } else if cfg!(target_os = "windows") && cfg!(target_arch = "x86_64") {
            Some(Self::WindowsX64)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleManifest {
    pub version: String,
    pub sha256: String,
    pub download_url: String,
}

pub fn codeql_bundle_manifest() -> Result<BundleManifest, ManagedToolCacheError> {
    let platform = Platform::detect().ok_or(ManagedToolCacheError::UnsupportedPlatform)?;
    let (download_url, sha256) = match platform {
        Platform::LinuxX64 => (
            "https://github.com/github/codeql-action/releases/download/codeql-bundle-v2.25.1/codeql-bundle-linux64.tar.gz",
            "6f867b8734a39b55929a1785d3ae843126ec68564f5598807ae8e126a5c35bba",
        ),
        // CodeQL does not publish a native aarch64 Linux bundle; use x86_64 (requires emulation)
        Platform::LinuxArm64 => (
            "https://github.com/github/codeql-action/releases/download/codeql-bundle-v2.25.1/codeql-bundle-linux64.tar.gz",
            "6f867b8734a39b55929a1785d3ae843126ec68564f5598807ae8e126a5c35bba",
        ),
        Platform::MacosX64 => (
            "https://github.com/github/codeql-action/releases/download/codeql-bundle-v2.25.1/codeql-bundle-osx64.tar.gz",
            "a5e0bc832929c0ba0a93a123abae6111ab4c3fd64a5477485074f2e131e55486",
        ),
        // CodeQL x86_64 bundle runs on Apple Silicon via Rosetta 2
        Platform::MacosArm64 => (
            "https://github.com/github/codeql-action/releases/download/codeql-bundle-v2.25.1/codeql-bundle-osx64.tar.gz",
            "a5e0bc832929c0ba0a93a123abae6111ab4c3fd64a5477485074f2e131e55486",
        ),
        Platform::WindowsX64 => (
            "https://github.com/github/codeql-action/releases/download/codeql-bundle-v2.25.1/codeql-bundle-win64.tar.gz",
            "791030c4201d4a35afb7f9efa4b8b667bd5c5902514c8ab1815907f5b158ab43",
        ),
    };

    Ok(BundleManifest {
        version: "2.25.1".to_owned(),
        sha256: sha256.to_owned(),
        download_url: download_url.to_owned(),
    })
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

    fn bootstrap_bundle(&self, bundle_dir: &Path) -> Result<(), ManagedToolCacheError> {
        eprintln!("Downloading CodeQL bundle v{}...", self.manifest.version);

        let bundle_root =
            bundle_dir
                .parent()
                .ok_or_else(|| ManagedToolCacheError::BootstrapExtract {
                    version: self.manifest.version.clone(),
                    source: io::Error::other("bundle directory has no parent"),
                })?;
        fs::create_dir_all(bundle_root).map_err(|source| {
            ManagedToolCacheError::BootstrapExtract {
                version: self.manifest.version.clone(),
                source,
            }
        })?;

        let archive_path = temp_archive_path(bundle_root, &self.manifest.version);
        if let Err(error) = self.download_archive(&archive_path) {
            let _ = fs::remove_file(&archive_path);
            return Err(error);
        }
        let install_result = self.install_bundle_from_archive(&archive_path, bundle_dir);
        let cleanup_result = fs::remove_file(&archive_path);
        if let Err(error) = install_result {
            let _ = cleanup_result;
            return Err(error);
        }
        if let Err(source) = cleanup_result {
            return Err(ManagedToolCacheError::BootstrapExtract {
                version: self.manifest.version.clone(),
                source,
            });
        }

        eprintln!(
            "CodeQL bundle v{} installed to {}",
            self.manifest.version,
            bundle_dir.display()
        );
        Ok(())
    }

    fn download_archive(&self, archive_path: &Path) -> Result<(), ManagedToolCacheError> {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_body(Some(Duration::from_secs(600)))
            .build()
            .new_agent();
        let response = agent
            .get(&self.manifest.download_url)
            .call()
            .map_err(|error| ManagedToolCacheError::BootstrapDownload {
                version: self.manifest.version.clone(),
                url: self.manifest.download_url.clone(),
                message: error.to_string(),
            })?;
        let mut reader = response.into_body().into_reader();
        let mut writer = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(archive_path)
            .map_err(|error| ManagedToolCacheError::BootstrapDownload {
                version: self.manifest.version.clone(),
                url: self.manifest.download_url.clone(),
                message: error.to_string(),
            })?;

        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            let read = reader.read(&mut buffer).map_err(|error| {
                ManagedToolCacheError::BootstrapDownload {
                    version: self.manifest.version.clone(),
                    url: self.manifest.download_url.clone(),
                    message: error.to_string(),
                }
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            writer.write_all(&buffer[..read]).map_err(|error| {
                ManagedToolCacheError::BootstrapDownload {
                    version: self.manifest.version.clone(),
                    url: self.manifest.download_url.clone(),
                    message: error.to_string(),
                }
            })?;
        }
        writer
            .flush()
            .map_err(|error| ManagedToolCacheError::BootstrapDownload {
                version: self.manifest.version.clone(),
                url: self.manifest.download_url.clone(),
                message: error.to_string(),
            })?;

        let actual = format!("{:x}", hasher.finalize());
        if actual != self.manifest.sha256 {
            let _ = fs::remove_file(archive_path);
            return Err(ManagedToolCacheError::ChecksumMismatch {
                path: archive_path.to_path_buf(),
                expected: self.manifest.sha256.clone(),
                actual,
            });
        }

        Ok(())
    }

    fn install_bundle_from_archive(
        &self,
        archive_path: &Path,
        bundle_dir: &Path,
    ) -> Result<(), ManagedToolCacheError> {
        let archive =
            File::open(archive_path).map_err(|source| ManagedToolCacheError::BootstrapExtract {
                version: self.manifest.version.clone(),
                source,
            })?;
        self.install_bundle_from_reader(archive, bundle_dir)
    }

    fn install_bundle_from_reader<R: Read>(
        &self,
        archive: R,
        bundle_dir: &Path,
    ) -> Result<(), ManagedToolCacheError> {
        let staging_dir = staging_dir(bundle_dir, &self.manifest.version);
        if staging_dir.exists() {
            fs::remove_dir_all(&staging_dir).map_err(|source| {
                ManagedToolCacheError::BootstrapExtract {
                    version: self.manifest.version.clone(),
                    source,
                }
            })?;
        }
        fs::create_dir_all(&staging_dir).map_err(|source| {
            ManagedToolCacheError::BootstrapExtract {
                version: self.manifest.version.clone(),
                source,
            }
        })?;

        let install_result = self
            .unpack_archive_into(archive, &staging_dir)
            .and_then(|_| {
                fs::write(
                    staging_dir.join(BUNDLE_MARKER_FILE),
                    self.manifest.sha256.as_bytes(),
                )
                .map_err(|source| ManagedToolCacheError::BootstrapExtract {
                    version: self.manifest.version.clone(),
                    source,
                })
            });

        if let Err(error) = install_result {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(error);
        }

        if bundle_dir.exists() {
            fs::remove_dir_all(bundle_dir).map_err(|source| {
                ManagedToolCacheError::BootstrapExtract {
                    version: self.manifest.version.clone(),
                    source,
                }
            })?;
        }
        fs::rename(&staging_dir, bundle_dir).map_err(|source| {
            ManagedToolCacheError::BootstrapExtract {
                version: self.manifest.version.clone(),
                source,
            }
        })?;

        Ok(())
    }

    fn unpack_archive_into<R: Read>(
        &self,
        archive: R,
        destination: &Path,
    ) -> Result<(), ManagedToolCacheError> {
        let decoder = GzDecoder::new(archive);
        let mut tar = Archive::new(decoder);
        let mut extracted_entry = false;

        for entry in tar
            .entries()
            .map_err(|source| ManagedToolCacheError::BootstrapExtract {
                version: self.manifest.version.clone(),
                source,
            })?
        {
            let mut entry = entry.map_err(|source| ManagedToolCacheError::BootstrapExtract {
                version: self.manifest.version.clone(),
                source,
            })?;
            let entry_path =
                entry
                    .path()
                    .map_err(|source| ManagedToolCacheError::BootstrapExtract {
                        version: self.manifest.version.clone(),
                        source,
                    })?;
            let stripped = entry_path.strip_prefix("codeql").map_err(|_| {
                ManagedToolCacheError::BootstrapExtract {
                    version: self.manifest.version.clone(),
                    source: io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "archive entry `{}` does not start with `codeql/`",
                            entry_path.display()
                        ),
                    ),
                }
            })?;
            let relative_path = sanitize_archive_path(stripped).map_err(|source| {
                ManagedToolCacheError::BootstrapExtract {
                    version: self.manifest.version.clone(),
                    source,
                }
            })?;
            if relative_path.as_os_str().is_empty() {
                continue;
            }

            let output_path = destination.join(relative_path);
            if let Some(parent) = output_path.parent() {
                fs::create_dir_all(parent).map_err(|source| {
                    ManagedToolCacheError::BootstrapExtract {
                        version: self.manifest.version.clone(),
                        source,
                    }
                })?;
            }

            if entry.header().entry_type().is_dir() {
                fs::create_dir_all(&output_path).map_err(|source| {
                    ManagedToolCacheError::BootstrapExtract {
                        version: self.manifest.version.clone(),
                        source,
                    }
                })?;
            } else {
                entry.unpack(&output_path).map_err(|source| {
                    ManagedToolCacheError::BootstrapExtract {
                        version: self.manifest.version.clone(),
                        source,
                    }
                })?;
            }
            extracted_entry = true;
        }

        if !extracted_entry {
            return Err(ManagedToolCacheError::BootstrapExtract {
                version: self.manifest.version.clone(),
                source: io::Error::new(
                    io::ErrorKind::InvalidData,
                    "archive did not contain any extractable entries under `codeql/`",
                ),
            });
        }

        Ok(())
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
    #[error(
        "failed to download CodeQL bundle v{version} from {url}: {message}. Ensure network connectivity or pre-populate the cache."
    )]
    BootstrapDownload {
        version: String,
        url: String,
        message: String,
    },
    #[error("failed to extract CodeQL bundle v{version}: {source}")]
    BootstrapExtract {
        version: String,
        #[source]
        source: io::Error,
    },
    #[error("unsupported platform for managed CodeQL bundle")]
    UnsupportedPlatform,
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
        let marker_path = cache_path.join(BUNDLE_MARKER_FILE);
        if !cache_path.exists() || !marker_path.exists() {
            self.bootstrap_bundle(&cache_path)?;
        }

        let marker_content = fs::read_to_string(&marker_path).map_err(|source| {
            ManagedToolCacheError::ReadBundleMarker {
                path: marker_path.clone(),
                source,
            }
        })?;
        let actual = marker_content.trim().to_owned();
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

fn sanitize_archive_path(path: &Path) -> io::Result<PathBuf> {
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => cleaned.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported archive path component in `{}`", path.display()),
                ));
            }
        }
    }

    Ok(cleaned)
}

fn temp_archive_path(bundle_root: &Path, version: &str) -> PathBuf {
    bundle_root.join(format!(
        ".codeql-bundle-{version}-{}.tar.gz",
        unique_suffix()
    ))
}

fn staging_dir(bundle_dir: &Path, version: &str) -> PathBuf {
    let bundle_root = bundle_dir.parent().unwrap_or(bundle_dir);
    bundle_root.join(format!(".codeql-bundle-{version}-{}", unique_suffix()))
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Cursor, Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use sha2::{Digest, Sha256};
    use tar::{Builder, Header};
    use tempfile::TempDir;

    use super::{
        BUNDLE_MARKER_FILE, BundleManifest, ManagedToolCacheAdapter, Platform,
        codeql_bundle_manifest,
    };
    use crate::ports::tool_cache::{ToolCachePort, ToolCacheRequest};

    #[test]
    fn codeql_bundle_manifest_returns_pinned_supported_manifest() {
        let manifest = codeql_bundle_manifest().unwrap();

        assert_eq!(manifest.version, "2.25.1");
        assert_eq!(manifest.sha256.len(), 64);
        match Platform::detect().unwrap() {
            Platform::LinuxX64 | Platform::LinuxArm64 => {
                assert!(
                    manifest
                        .download_url
                        .ends_with("codeql-bundle-linux64.tar.gz")
                );
            }
            Platform::MacosX64 | Platform::MacosArm64 => {
                assert!(
                    manifest
                        .download_url
                        .ends_with("codeql-bundle-osx64.tar.gz")
                );
            }
            Platform::WindowsX64 => {
                assert!(
                    manifest
                        .download_url
                        .ends_with("codeql-bundle-win64.tar.gz")
                );
            }
        }
    }

    #[test]
    fn resolve_bundle_returns_cache_hit() {
        let temp = TempDir::new().unwrap();
        let bundle_dir = temp.path().join("codeql").join("2.0.0");
        fs::create_dir_all(&bundle_dir).unwrap();
        let checksum = "a".repeat(64);
        fs::write(bundle_dir.join(BUNDLE_MARKER_FILE), &checksum).unwrap();
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
    fn bootstrap_extracts_and_writes_marker() {
        let temp = TempDir::new().unwrap();
        let archive_bytes = fixture_archive_bytes();
        let checksum = format!("{:x}", Sha256::digest(&archive_bytes));
        let bundle_dir = temp.path().join("codeql").join("2.0.0");
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: checksum.clone(),
                download_url: "https://example.invalid/codeql.tgz".to_owned(),
            },
            temp.path(),
        );

        adapter
            .install_bundle_from_reader(Cursor::new(archive_bytes), &bundle_dir)
            .unwrap();

        assert_eq!(
            fs::read_to_string(bundle_dir.join(BUNDLE_MARKER_FILE))
                .unwrap()
                .trim(),
            checksum
        );
        assert_eq!(
            fs::read_to_string(bundle_dir.join("queries").join("extract-rust.ql")).unwrap(),
            "// fixture query\n"
        );
        assert!(bundle_dir.join("codeql").exists());
    }

    #[test]
    fn resolve_bundle_bootstraps_bundle_on_cache_miss() {
        let temp = TempDir::new().unwrap();
        let archive_bytes = fixture_archive_bytes();
        let checksum = format!("{:x}", Sha256::digest(&archive_bytes));
        let (download_url, server) = spawn_http_server(archive_bytes);
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: checksum.clone(),
                download_url,
            },
            temp.path(),
        );

        let bundle = adapter
            .resolve_bundle(&ToolCacheRequest {
                tool_name: "codeql".to_owned(),
                version: "2.0.0".to_owned(),
            })
            .unwrap();
        server.join().unwrap();

        assert_eq!(bundle.checksum, checksum);
        assert!(bundle.cache_path.join("codeql").exists());
        assert!(
            bundle
                .cache_path
                .join("queries")
                .join("extract-rust.ql")
                .exists()
        );
        assert_eq!(
            fs::read_to_string(bundle.cache_path.join(BUNDLE_MARKER_FILE))
                .unwrap()
                .trim(),
            checksum
        );
    }

    #[test]
    fn resolve_bundle_reports_download_error_without_bootstrap_hint() {
        let temp = TempDir::new().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: "a".repeat(64),
                download_url: format!("http://{addr}/codeql.tgz"),
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
        assert!(message.contains("failed to download CodeQL bundle v2.0.0"));
        assert!(message.contains("Ensure network connectivity or pre-populate the cache"));
        assert!(!message.contains("kalos bootstrap"));
    }

    #[test]
    fn resolve_bundle_cleans_up_temp_archive_on_download_error() {
        let temp = TempDir::new().unwrap();
        let (download_url, server) = spawn_truncated_http_server(b"partial".to_vec(), 32);
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: "a".repeat(64),
                download_url,
            },
            temp.path(),
        );

        let error = adapter
            .resolve_bundle(&ToolCacheRequest {
                tool_name: "codeql".to_owned(),
                version: "2.0.0".to_owned(),
            })
            .unwrap_err();
        server.join().unwrap();

        assert!(
            error
                .to_string()
                .contains("failed to download CodeQL bundle v2.0.0")
        );
        let temp_archives: Vec<_> = fs::read_dir(temp.path().join("codeql"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".codeql-bundle-2.0.0-") && name.ends_with(".tar.gz"))
            .collect();
        assert!(
            temp_archives.is_empty(),
            "unexpected temp archives: {temp_archives:?}"
        );
    }

    #[test]
    fn resolve_bundle_detects_checksum_mismatch() {
        let temp = TempDir::new().unwrap();
        let bundle_dir = temp.path().join("codeql").join("2.0.0");
        fs::create_dir_all(&bundle_dir).unwrap();
        fs::write(bundle_dir.join(BUNDLE_MARKER_FILE), "b".repeat(64)).unwrap();
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

        let message = error.to_string();
        assert!(message.contains("checksum mismatch"));
        assert!(message.contains(&bundle_dir.join(BUNDLE_MARKER_FILE).display().to_string()));
    }

    fn fixture_archive_bytes() -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = Builder::new(&mut encoder);
            append_tar_entry(&mut builder, "codeql/codeql", b"#!/bin/sh\nexit 0\n");
            append_tar_entry(
                &mut builder,
                "codeql/queries/extract-rust.ql",
                b"// fixture query\n",
            );
            builder.finish().unwrap();
        }
        encoder.finish().unwrap()
    }

    fn append_tar_entry(builder: &mut Builder<&mut GzEncoder<Vec<u8>>>, path: &str, bytes: &[u8]) {
        let mut header = Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, bytes).unwrap();
    }

    fn spawn_http_server(body: Vec<u8>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
            stream.flush().unwrap();
        });

        (format!("http://{addr}/codeql.tgz"), handle)
    }

    fn spawn_truncated_http_server(
        body: Vec<u8>,
        advertised_len: usize,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {advertised_len}\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            stream.write_all(&body).unwrap();
            stream.flush().unwrap();
        });

        (format!("http://{addr}/codeql.tgz"), handle)
    }
}
