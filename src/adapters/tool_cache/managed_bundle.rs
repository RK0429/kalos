use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use tar::Archive;
use thiserror::Error;

use crate::ports::tool_cache::{ResolvedToolBundle, ToolCachePort, ToolCacheRequest};

pub const BUNDLE_MARKER_FILE: &str = "bundle.marker";

/// Kalos CPG extraction queries embedded in the binary.
///
/// These are deployed to the CodeQL bundle cache alongside the CLI so that
/// `codeql query run` can find them. Each query emits named predicates that
/// decode into the JSON shape expected by `CpgNormalizer`.
const BUNDLED_QUERIES: &[(&str, &str)] = &[
    (
        "extract-python.ql",
        r#"import python
import semmle.python.objects.ObjectAPI

private string moduleId(Module m) { result = "mod_" + m.getFile().getRelativePath() }

private string functionId(Function f) {
  result =
    "fn_" + f.getLocation().getFile().getRelativePath() + ":" +
      f.getLocation().getStartLine().toString() + ":" + f.getQualifiedName()
}

private string classId(Class c) {
  result =
    "cls_" + c.getLocation().getFile().getRelativePath() + ":" +
      c.getLocation().getStartLine().toString() + ":" + c.getQualifiedName()
}

query predicate modules(string id, string name, string file, int start_line, int end_line) {
  exists(Module m |
    id = moduleId(m) and
    name = m.getFile().getRelativePath() and
    file = m.getFile().getRelativePath() and
    start_line = 1 and
    end_line = 1
  )
}

query predicate classes(string id, string name, string file, int start_line, int end_line) {
  exists(Class c |
    exists(c.getQualifiedName()) and
    id = classId(c) and
    name = c.getName() and
    file = c.getLocation().getFile().getRelativePath() and
    start_line = c.getLocation().getStartLine() and
    end_line = c.getLocation().getEndLine()
  )
}

query predicate functions(string id, string name, string file, int start_line, int end_line) {
  exists(Function f |
    exists(f.getQualifiedName()) and
    id = functionId(f) and
    name = f.getName() and
    file = f.getLocation().getFile().getRelativePath() and
    start_line = f.getLocation().getStartLine() and
    end_line = f.getLocation().getEndLine()
  )
}

query predicate contains(string source, string target) {
  exists(Module m, Function f |
    f.getScope() = m and
    source = moduleId(m) and
    target = functionId(f)
  )
  or
  exists(Module m, Class c |
    c.getScope() = m and
    source = moduleId(m) and
    target = classId(c)
  )
}

query predicate calls(string source, string target) {
  exists(Call call, Function caller, FunctionValue callee_value, Function callee |
    call.getScope() = caller and
    call.getFunc().pointsTo(callee_value) and
    callee_value.getScope() = callee and
    source = functionId(caller) and
    target = functionId(callee)
  )
}
"#,
    ),
    (
        "extract-javascript-typescript.ql",
        r#"import javascript

private string moduleId(TopLevel tl) { result = "mod_" + tl.getFile().getRelativePath() }

private string functionId(Function f) {
  result =
    "fn_" + f.getLocation().getFile().getRelativePath() + ":" +
      f.getLocation().getStartLine().toString() + ":" + f.getName()
}

private string classId(ClassDefinition c) {
  result =
    "cls_" + c.getLocation().getFile().getRelativePath() + ":" +
      c.getLocation().getStartLine().toString() + ":" + c.getName()
}

query predicate modules(string id, string name, string file, int start_line, int end_line) {
  exists(TopLevel tl |
    id = moduleId(tl) and
    name = tl.getFile().getRelativePath() and
    file = tl.getFile().getRelativePath() and
    start_line = tl.getLocation().getStartLine() and
    end_line = tl.getLocation().getEndLine()
  )
}

query predicate classes(string id, string name, string file, int start_line, int end_line) {
  exists(ClassDefinition c |
    exists(c.getName()) and
    id = classId(c) and
    name = c.getName() and
    file = c.getLocation().getFile().getRelativePath() and
    start_line = c.getLocation().getStartLine() and
    end_line = c.getLocation().getEndLine()
  )
}

query predicate functions(string id, string name, string file, int start_line, int end_line) {
  exists(Function f |
    exists(f.getName()) and
    id = functionId(f) and
    name = f.getName() and
    file = f.getLocation().getFile().getRelativePath() and
    start_line = f.getLocation().getStartLine() and
    end_line = f.getLocation().getEndLine()
  )
}

query predicate contains(string source, string target) {
  exists(TopLevel tl, Function f |
    f.getTopLevel() = tl and
    exists(f.getName()) and
    source = moduleId(tl) and
    target = functionId(f)
  )
  or
  exists(TopLevel tl, ClassDefinition c |
    c.getTopLevel() = tl and
    exists(c.getName()) and
    source = moduleId(tl) and
    target = classId(c)
  )
}

query predicate calls(string source, string target) {
  exists(CallExpr call, Function caller, Function callee |
    call.getEnclosingFunction() = caller and
    callee = call.getResolvedCallee() and
    exists(caller.getName()) and
    exists(callee.getName()) and
    source = functionId(caller) and
    target = functionId(callee)
  )
}
"#,
    ),
    (
        "extract-rust.ql",
        r#"import rust
import codeql.files.FileSystem

private string moduleId(File f) { result = "mod_" + f.getRelativePath() }

private string functionId(Function f) {
  result =
    "fn_" + f.getFile().getRelativePath() + ":" + f.getLocation().getStartLine().toString() +
      ":" + f.getName().getText()
}

private string structId(Struct s) {
  result =
    "cls_" + s.getFile().getRelativePath() + ":" + s.getLocation().getStartLine().toString() +
      ":" + s.getName().getText()
}

private string enumId(Enum e) {
  result =
    "cls_" + e.getFile().getRelativePath() + ":" + e.getLocation().getStartLine().toString() +
      ":" + e.getName().getText()
}

private string traitId(Trait t) {
  result =
    "cls_" + t.getFile().getRelativePath() + ":" + t.getLocation().getStartLine().toString() +
      ":" + t.getName().getText()
}

query predicate modules(string id, string name, string file, int start_line, int end_line) {
  exists(File f |
    (
      exists(Function func | func.fromSource() and func.getFile() = f)
      or
      exists(Struct s | s.fromSource() and s.getFile() = f)
      or
      exists(Enum e | e.fromSource() and e.getFile() = f)
      or
      exists(Trait t | t.fromSource() and t.getFile() = f)
    ) and
    id = moduleId(f) and
    name = f.getRelativePath() and
    file = f.getRelativePath() and
    start_line = 1 and
    end_line = 1
  )
}

query predicate classes(string id, string name, string file, int start_line, int end_line) {
  exists(Struct s |
    s.fromSource() and
    id = structId(s) and
    name = s.getName().getText() and
    file = s.getFile().getRelativePath() and
    start_line = s.getLocation().getStartLine() and
    end_line = s.getLocation().getEndLine()
  )
  or
  exists(Enum e |
    e.fromSource() and
    id = enumId(e) and
    name = e.getName().getText() and
    file = e.getFile().getRelativePath() and
    start_line = e.getLocation().getStartLine() and
    end_line = e.getLocation().getEndLine()
  )
  or
  exists(Trait t |
    t.fromSource() and
    id = traitId(t) and
    name = t.getName().getText() and
    file = t.getFile().getRelativePath() and
    start_line = t.getLocation().getStartLine() and
    end_line = t.getLocation().getEndLine()
  )
}

query predicate functions(string id, string name, string file, int start_line, int end_line) {
  exists(Function f |
    f.fromSource() and
    id = functionId(f) and
    name = f.getName().getText() and
    file = f.getFile().getRelativePath() and
    start_line = f.getLocation().getStartLine() and
    end_line = f.getLocation().getEndLine()
  )
}

query predicate contains(string source, string target) {
  exists(Function f |
    f.fromSource() and
    source = moduleId(f.getFile()) and
    target = functionId(f)
  )
  or
  exists(Struct s |
    s.fromSource() and
    source = moduleId(s.getFile()) and
    target = structId(s)
  )
  or
  exists(Enum e |
    e.fromSource() and
    source = moduleId(e.getFile()) and
    target = enumId(e)
  )
  or
  exists(Trait t |
    t.fromSource() and
    source = moduleId(t.getFile()) and
    target = traitId(t)
  )
}

query predicate calls(string source, string target) {
  exists(Call call, Function caller, Function callee |
    caller = call.getEnclosingCallable() and
    callee = call.getStaticTarget() and
    caller.fromSource() and
    callee.fromSource() and
    source = functionId(caller) and
    target = functionId(callee)
  )
}
"#,
    ),
    (
        "extract-go.ql",
        r#"import go

private string functionId(FuncDecl fd) {
  result =
    "fn_" + fd.getFile().getRelativePath() + ":" +
      fd.getLocation().getStartLine().toString() + ":" + fd.getName()
}

private string classId(TypeSpec t) {
  result =
    "cls_" + t.getFile().getRelativePath() + ":" +
      t.getLocation().getStartLine().toString() + ":" + t.getName()
}

query predicate modules(string id, string name, string file, int start_line, int end_line) {
  exists(FuncDecl fd |
    id = "mod_" + fd.getFile().getRelativePath() and
    name = fd.getFile().getRelativePath() and
    file = fd.getFile().getRelativePath() and
    start_line = 1 and
    end_line = 1
  )
  or
  exists(TypeSpec t |
    id = "mod_" + t.getFile().getRelativePath() and
    name = t.getFile().getRelativePath() and
    file = t.getFile().getRelativePath() and
    start_line = 1 and
    end_line = 1
  )
}

query predicate classes(string id, string name, string file, int start_line, int end_line) {
  exists(TypeSpec t |
    id = classId(t) and
    name = t.getName() and
    file = t.getFile().getRelativePath() and
    start_line = t.getLocation().getStartLine() and
    end_line = t.getLocation().getEndLine()
  )
}

query predicate functions(string id, string name, string file, int start_line, int end_line) {
  exists(FuncDecl fd |
    id = functionId(fd) and
    name = fd.getName() and
    file = fd.getFile().getRelativePath() and
    start_line = fd.getLocation().getStartLine() and
    end_line = fd.getLocation().getEndLine()
  )
}

query predicate contains(string source, string target) {
  exists(FuncDecl fd |
    source = "mod_" + fd.getFile().getRelativePath() and
    target = functionId(fd)
  )
  or
  exists(TypeSpec t |
    source = "mod_" + t.getFile().getRelativePath() and
    target = classId(t)
  )
}

query predicate calls(string source, string target) {
  exists(CallExpr call, FuncDecl caller, FuncDecl callee |
    call.getEnclosingFunction() = caller and
    callee.getFunction() = call.getTarget() and
    source = functionId(caller) and
    target = functionId(callee)
  )
}
"#,
    ),
];

/// Per-language qlpack.yml contents deployed alongside extraction queries.
///
/// Each language needs its own qlpack because CodeQL requires a single
/// dbscheme per pack. The pack name and dependency are derived from the
/// language identifier.
const BUNDLED_QLPACKS: &[(&str, &str)] = &[
    (
        "python",
        "name: kalos/extract-python\nversion: 0.0.1\ndependencies:\n  codeql/python-all: \"*\"\n",
    ),
    (
        "javascript-typescript",
        "name: kalos/extract-js-ts\nversion: 0.0.1\ndependencies:\n  codeql/javascript-all: \"*\"\n",
    ),
    (
        "rust",
        "name: kalos/extract-rust\nversion: 0.0.1\ndependencies:\n  codeql/rust-all: \"*\"\n",
    ),
    (
        "go",
        "name: kalos/extract-go\nversion: 0.0.1\ndependencies:\n  codeql/go-all: \"*\"\n",
    ),
];

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

    /// Returns true when the CodeQL bundle uses x86_64 binary on a non-x86_64 host.
    /// On Apple Silicon this means Rosetta 2 emulation; on Linux ARM64 this means
    /// QEMU or similar emulation.
    pub fn is_emulated(&self) -> bool {
        matches!(self, Self::MacosArm64 | Self::LinuxArm64)
    }

    /// Returns a user-facing notice about emulation overhead, if applicable.
    pub fn emulation_notice(&self) -> Option<&'static str> {
        match self {
            Self::MacosArm64 => Some(
                "note: CodeQL does not provide a native ARM64 bundle for macOS. \
                 The x86_64 bundle will run via Rosetta 2, which may be significantly \
                 slower on first invocation.",
            ),
            Self::LinuxArm64 => Some(
                "note: CodeQL does not provide a native aarch64 bundle for Linux. \
                 The x86_64 bundle requires emulation (e.g. QEMU), which may be \
                 significantly slower.",
            ),
            _ => None,
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
        let content_length = response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
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

        let progress_message = format!("Downloading CodeQL bundle v{}", self.manifest.version);
        let progress_bar = if let Some(content_length) = content_length {
            let progress_bar = ProgressBar::new(content_length);
            progress_bar.set_style(
                ProgressStyle::with_template(
                    "{msg} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
                )
                .expect("valid progress template"),
            );
            progress_bar
        } else {
            let progress_bar = ProgressBar::new_spinner();
            progress_bar.enable_steady_tick(Duration::from_millis(100));
            progress_bar.set_style(
                ProgressStyle::with_template("{msg} {spinner} {bytes} ({bytes_per_sec})")
                    .expect("valid progress template"),
            );
            progress_bar
        };
        progress_bar.set_message(progress_message);

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
            progress_bar.inc(read as u64);
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
        progress_bar.finish_with_message(format!(
            "Downloaded CodeQL bundle v{}",
            self.manifest.version
        ));

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

        deploy_bundled_queries(&cache_path).map_err(|source| {
            ManagedToolCacheError::BootstrapExtract {
                version: self.manifest.version.clone(),
                source,
            }
        })?;

        Ok(ResolvedToolBundle {
            tool_name: request.tool_name.clone(),
            version: request.version.clone(),
            cache_path,
            checksum: self.manifest.sha256.clone(),
        })
    }
}

fn deploy_bundled_queries(bundle_dir: &Path) -> io::Result<()> {
    let queries_dir = bundle_dir.join("queries");
    fs::create_dir_all(&queries_dir)?;
    for (filename, content) in BUNDLED_QUERIES {
        let lang_dir = filename
            .strip_prefix("extract-")
            .and_then(|name| name.strip_suffix(".ql"))
            .unwrap_or(filename);
        let subdir = queries_dir.join(lang_dir);
        fs::create_dir_all(&subdir)?;
        fs::write(subdir.join(filename), content)?;
    }
    for (lang_dir, qlpack_content) in BUNDLED_QLPACKS {
        let subdir = queries_dir.join(lang_dir);
        fs::create_dir_all(&subdir)?;
        fs::write(subdir.join("qlpack.yml"), qlpack_content)?;
    }
    Ok(())
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
        BUNDLE_MARKER_FILE, BUNDLED_QLPACKS, BUNDLED_QUERIES, BundleManifest,
        ManagedToolCacheAdapter, Platform, codeql_bundle_manifest, deploy_bundled_queries,
    };
    use crate::ports::tool_cache::{ToolCachePort, ToolCacheRequest};

    fn bundled_query_lang_dir(filename: &str) -> &str {
        filename
            .strip_prefix("extract-")
            .and_then(|name| name.strip_suffix(".ql"))
            .unwrap_or(filename)
    }

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
    fn platform_is_emulated_returns_true_for_arm64_variants() {
        assert!(Platform::MacosArm64.is_emulated());
        assert!(Platform::LinuxArm64.is_emulated());
        assert!(!Platform::MacosX64.is_emulated());
        assert!(!Platform::LinuxX64.is_emulated());
        assert!(!Platform::WindowsX64.is_emulated());
    }

    #[test]
    fn platform_emulation_notice_present_for_arm64_variants() {
        assert!(Platform::MacosArm64.emulation_notice().is_some());
        assert!(Platform::LinuxArm64.emulation_notice().is_some());
        assert!(Platform::MacosX64.emulation_notice().is_none());
        assert!(Platform::LinuxX64.emulation_notice().is_none());
        assert!(Platform::WindowsX64.emulation_notice().is_none());
    }

    #[test]
    fn platform_emulation_notice_mentions_rosetta_for_macos_arm64() {
        let notice = Platform::MacosArm64.emulation_notice().unwrap();
        assert!(notice.contains("Rosetta 2"));
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
                .join("rust")
                .join("extract-rust.ql")
                .exists()
        );
        assert!(
            bundle
                .cache_path
                .join("queries")
                .join("rust")
                .join("qlpack.yml")
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
    fn download_archive_reports_progress() {
        let temp = TempDir::new().unwrap();
        let archive_bytes = fixture_archive_bytes();
        let checksum = format!("{:x}", Sha256::digest(&archive_bytes));
        let (download_url, server) = spawn_http_server(archive_bytes);
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: checksum,
                download_url,
            },
            temp.path(),
        );
        let archive_path = temp.path().join("codeql.tar.gz");

        adapter.download_archive(&archive_path).unwrap();
        server.join().unwrap();

        assert!(archive_path.exists());
    }

    #[test]
    fn download_archive_works_without_content_length() {
        let temp = TempDir::new().unwrap();
        let archive_bytes = fixture_archive_bytes();
        let checksum = format!("{:x}", Sha256::digest(&archive_bytes));
        let (download_url, server) = spawn_http_server_no_content_length(archive_bytes);
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: checksum,
                download_url,
            },
            temp.path(),
        );
        let archive_path = temp.path().join("codeql.tar.gz");

        adapter.download_archive(&archive_path).unwrap();
        server.join().unwrap();

        assert!(archive_path.exists());
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

    fn spawn_http_server_no_content_length(body: Vec<u8>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            write!(stream, "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n").unwrap();
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

    #[test]
    fn deploy_bundled_queries_creates_missing_query_files() {
        let temp = TempDir::new().unwrap();
        let bundle_dir = temp.path().join("codeql").join("2.0.0");
        fs::create_dir_all(&bundle_dir).unwrap();

        deploy_bundled_queries(&bundle_dir).unwrap();

        for (filename, expected_content) in BUNDLED_QUERIES {
            let path = bundle_dir
                .join("queries")
                .join(bundled_query_lang_dir(filename))
                .join(filename);
            assert!(path.exists(), "{filename} should be created");
            assert_eq!(fs::read_to_string(&path).unwrap(), *expected_content);
        }
        for (lang_dir, expected_content) in BUNDLED_QLPACKS {
            let path = bundle_dir.join("queries").join(lang_dir).join("qlpack.yml");
            assert!(path.exists(), "{lang_dir}/qlpack.yml should be created");
            assert_eq!(fs::read_to_string(&path).unwrap(), *expected_content);
        }
    }

    #[test]
    fn deploy_bundled_queries_overwrites_stale_content() {
        let temp = TempDir::new().unwrap();
        let bundle_dir = temp.path().join("codeql").join("2.0.0");
        let query_dir = bundle_dir.join("queries").join("rust");
        fs::create_dir_all(&query_dir).unwrap();
        fs::write(query_dir.join("extract-rust.ql"), "select 1\n").unwrap();

        deploy_bundled_queries(&bundle_dir).unwrap();

        let content = fs::read_to_string(query_dir.join("extract-rust.ql")).unwrap();
        assert_ne!(
            content, "select 1\n",
            "stale query should be overwritten with bundled content"
        );
        assert!(
            content.contains("query predicate"),
            "overwritten query should contain real predicates"
        );
    }

    #[test]
    fn bundled_queries_use_named_predicates_instead_of_select_stubs() {
        for (filename, query) in BUNDLED_QUERIES {
            assert!(
                !query.contains("select 1"),
                "{filename} should not contain a select stub"
            );
            for predicate in ["modules", "functions", "classes", "contains", "calls"] {
                assert!(
                    query.contains(&format!("query predicate {predicate}")),
                    "{filename} should define `{predicate}`"
                );
            }
        }
    }

    #[test]
    fn bundled_go_query_matches_bundled_codeql_pack_capabilities() {
        let (_, query) = BUNDLED_QUERIES
            .iter()
            .find(|(filename, _)| *filename == "extract-go.ql")
            .expect("extract-go.ql should be bundled");

        assert!(
            !query.contains("import codeql.files.FileSystem"),
            "Go bundled query should avoid unavailable FileSystem imports"
        );
        assert!(
            query.contains("FuncDecl"),
            "Go bundled query should use FuncDecl for file and location access"
        );
    }
}
