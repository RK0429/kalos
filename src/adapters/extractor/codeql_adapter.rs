use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use thiserror::Error;
use tracing::{debug, warn};

use super::cpg_normalizer::{CodeQlQueryOutput, CpgNormalizer, NormalizationError};
use super::file_collector::FileCollector;
use crate::domains::FilePath;
use crate::domains::cpg::{AnalysisWarning, Language, SourceAnalysis, SourceFile, UnifiedCpg};
use crate::platform::fs::FileSystem;
use crate::platform::process::{CommandRunner, ProcessError, ProcessOutput};
use crate::ports::extractor::{ExtractionRequest, ExtractorPort};
use crate::ports::tool_cache::{ToolCachePort, ToolCacheRequest};

const DEFAULT_EXTENSIONS: [&str; 5] = [".py", ".ts", ".tsx", ".rs", ".go"];
const DEFAULT_MIN_LANGUAGE_RATIO: f64 = 0.05;
const SLOW_PATH_SOURCE_FILE_THRESHOLD: usize = 100;

fn supported_extensions_display() -> String {
    DEFAULT_EXTENSIONS
        .iter()
        .map(|extension| extension.trim_start_matches('*'))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_analysis_targets(analysis_targets: &[FilePath]) -> String {
    if analysis_targets.is_empty() {
        ".".to_owned()
    } else {
        analysis_targets
            .iter()
            .map(FilePath::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn codeql_executable_path(bundle_cache_path: &Path) -> PathBuf {
    bundle_cache_path.join(format!("codeql{}", std::env::consts::EXE_SUFFIX))
}

/// Compute a content-based fingerprint from source files for a given language.
/// Returns None if any file cannot be read (falls through to normal execution).
fn compute_source_fingerprint<F: FileSystem>(
    file_system: &F,
    workspace_root: &Path,
    source_files: &BTreeMap<FilePath, SourceFile>,
    language: Language,
    bundle_version: &str,
    query_content: &str,
) -> Option<String> {
    let mut hasher = Sha256::new();
    hasher.update(bundle_version.as_bytes());
    hasher.update(b"|");
    hasher.update(query_content.as_bytes());
    hasher.update(b"|");
    for (path, source_file) in source_files {
        if source_file.language != language {
            continue;
        }
        let abs_path = workspace_root.join(path.as_str());
        let content = match file_system.read_to_string(&abs_path) {
            Ok(content) => content,
            Err(error) => {
                warn!(
                    path = %abs_path.display(),
                    language = ?language,
                    error = %error,
                    "kalos cache: failed to read source file while computing fingerprint; cache will be disabled this run"
                );
                return None;
            }
        };
        hasher.update(path.as_str().as_bytes());
        hasher.update(b"\0");
        hasher.update(content.as_bytes());
        hasher.update(b"\0");
    }
    Some(format!("{:x}", hasher.finalize()))
}

fn try_load_cache(
    fingerprint: &str,
    cache_key_path: &Path,
    decoded_cache_path: &Path,
) -> Result<CodeQlQueryOutput, CacheMissReason> {
    let stored_key =
        std::fs::read_to_string(cache_key_path).map_err(CacheMissReason::KeyFileMissing)?;
    if stored_key.trim() != fingerprint {
        return Err(CacheMissReason::KeyMismatch);
    }

    let cached_bytes =
        std::fs::read(decoded_cache_path).map_err(CacheMissReason::DecodedFileMissing)?;
    CpgNormalizer::parse_output(&cached_bytes).map_err(CacheMissReason::DecodedParseError)
}

#[derive(Debug)]
enum CacheMissReason {
    KeyFileMissing(io::Error),
    KeyMismatch,
    DecodedFileMissing(io::Error),
    DecodedParseError(NormalizationError),
}

impl std::fmt::Display for CacheMissReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheMissReason::KeyFileMissing(error) => {
                write!(f, "cache key file missing or unreadable ({error})")
            }
            CacheMissReason::KeyMismatch => {
                write!(f, "stored cache key does not match fingerprint")
            }
            CacheMissReason::DecodedFileMissing(error) => {
                write!(f, "decoded cache payload missing or unreadable ({error})")
            }
            CacheMissReason::DecodedParseError(error) => {
                write!(f, "decoded cache payload failed to parse ({error})")
            }
        }
    }
}

fn write_cache_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cache path `{}` has no parent directory", path.display()),
        )
    })?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cache path `{}` has no valid file name", path.display()),
            )
        })?;
    let unique = format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0),
        file_name
    );
    let tmp = parent.join(format!(".{unique}.tmp"));

    if let Err(error) = std::fs::write(&tmp, data) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct CodeQlAdapter<F, R, T> {
    file_system: F,
    command_runner: R,
    tool_cache: T,
    bundle_version: String,
    exclude_patterns: Vec<String>,
    normalizer: CpgNormalizer,
    progress: bool,
    is_emulated: bool,
    min_language_ratio: f64,
    database_root: Option<PathBuf>,
}

impl<F, R, T> CodeQlAdapter<F, R, T> {
    pub fn new(
        file_system: F,
        command_runner: R,
        tool_cache: T,
        bundle_version: impl Into<String>,
        exclude_patterns: Vec<String>,
    ) -> Self {
        Self {
            file_system,
            command_runner,
            tool_cache,
            bundle_version: bundle_version.into(),
            exclude_patterns,
            normalizer: CpgNormalizer,
            progress: false,
            is_emulated: false,
            min_language_ratio: DEFAULT_MIN_LANGUAGE_RATIO,
            database_root: None,
        }
    }

    pub fn with_progress(mut self) -> Self {
        self.progress = true;
        self
    }

    pub fn with_emulated(mut self) -> Self {
        self.is_emulated = true;
        self
    }

    pub fn with_min_language_ratio(mut self, ratio: f64) -> Self {
        self.min_language_ratio = ratio;
        self
    }

    pub fn with_database_root(mut self, database_root: impl Into<PathBuf>) -> Self {
        self.database_root = Some(database_root.into());
        self
    }

    pub fn language_pack(language: Language) -> &'static str {
        match language {
            Language::Python => "python",
            Language::TypeScript => "javascript-typescript",
            Language::Rust => "rust",
            Language::Go => "go",
        }
    }
}

#[derive(Debug, Error)]
pub enum CodeQlAdapterError {
    #[error(
        "failed to collect source files under `{workspace_root}` for analysis target path(s) `{analysis_targets}`: {source}"
    )]
    CollectFiles {
        workspace_root: PathBuf,
        analysis_targets: String,
        #[source]
        source: io::Error,
    },
    #[error("failed to resolve CodeQL bundle: {message}")]
    ResolveBundle { message: String },
    #[error("failed to create CodeQL database directory `{path}`: {source}")]
    CreateDatabaseDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to acquire lock on `{path}`: {source}")]
    AcquireLock {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to execute `{stage}` for `{language}`: {source}")]
    Process {
        stage: &'static str,
        language: String,
        #[source]
        source: ProcessError,
    },
    #[error("failed to parse CodeQL language preflight output for `{language}`: {source}")]
    ResolveLanguagesOutput {
        language: String,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "CodeQL extractor for `{language}` is not installed in the resolved bundle\n\n  cause: CodeQL cannot create a database for this language because its extractor is unavailable\n  available languages: {available_languages}\n\n  next steps:\n    - refresh the managed CodeQL bundle/cache and re-run `kalos check`\n    - if `{language}` remains unavailable, treat this language as unsupported in the current environment"
    )]
    ExtractorUnavailable {
        language: String,
        available_languages: String,
    },
    #[error("CodeQL `{stage}` failed for `{language}` (exit code {exit_code})\n\n{guidance}")]
    CommandFailed {
        stage: &'static str,
        language: String,
        exit_code: i32,
        stderr: String,
        guidance: String,
    },
    #[error(transparent)]
    Normalize(#[from] NormalizationError),
}

impl<F, R, T> ExtractorPort for CodeQlAdapter<F, R, T>
where
    F: FileSystem,
    R: CommandRunner,
    T: ToolCachePort,
    T::Error: std::fmt::Display,
{
    type Error = CodeQlAdapterError;

    fn extract(&self, request: &ExtractionRequest) -> Result<SourceAnalysis, Self::Error> {
        let collector = FileCollector::new(
            &self.file_system,
            &request.workspace_root,
            &DEFAULT_EXTENSIONS,
            &self.exclude_patterns,
        );
        let source_files = collector
            .collect(&request.analysis_targets)
            .map_err(|source| CodeQlAdapterError::CollectFiles {
                workspace_root: request.workspace_root.clone(),
                analysis_targets: format_analysis_targets(&request.analysis_targets),
                source,
            })?;
        if source_files.is_empty() {
            return Ok(SourceAnalysis {
                cpg: UnifiedCpg {
                    id: "codeql".into(),
                    nodes: Vec::new(),
                    edges: Vec::new(),
                },
                source_files,
                suppressions: Vec::new(),
                warnings: vec![AnalysisWarning {
                    file_path: FilePath::from("."),
                    message: format!(
                        "no files with supported extensions ({}) were found in the analysis targets",
                        supported_extensions_display()
                    ),
                    user_facing: true,
                }],
            });
        }

        let language_counts = count_source_files_by_language(&source_files);
        let (languages, language_warnings) =
            filter_incidental_languages(&source_files, self.min_language_ratio);

        if self.progress {
            emit_analysis_inventory_progress(&language_counts);
            let total_files = source_files.len();
            if total_files >= SLOW_PATH_SOURCE_FILE_THRESHOLD {
                eprintln!(
                    "  codeql: slow-path guidance: {} source files may take several minutes on the first run; interrupt and retry with --exclude for generated/vendor paths, --diff for a bounded target set, --cache-dir to reuse CodeQL databases, or --min-language-ratio to skip incidental languages",
                    total_files
                );
            }
            for (language, count) in &language_counts {
                if !languages.contains(language) {
                    eprintln!(
                        "  codeql: skipping {} ({} of {} files, below {:.0}% threshold)",
                        language_name(*language),
                        count,
                        total_files,
                        self.min_language_ratio * 100.0
                    );
                }
            }
        }

        let bundle = self
            .tool_cache
            .resolve_bundle(&ToolCacheRequest {
                tool_name: "codeql".to_owned(),
                version: self.bundle_version.clone(),
            })
            .map_err(|error| CodeQlAdapterError::ResolveBundle {
                message: error.to_string(),
            })?;
        let codeql_program = codeql_executable_path(&bundle.cache_path);
        let mut combined_output = CodeQlQueryOutput::default();

        for language in languages {
            let lang_dir = Self::language_pack(language).replace('/', "-");
            if self.progress {
                eprintln!("  codeql: analyzing {} ...", language_name(language));
            }
            let database_path = self.database_root(&request.workspace_root).join(&lang_dir);
            let cache_key_path = database_path.with_extension("cache_key");
            let decoded_cache_path = database_path.with_extension("decoded.json");
            let bqrs_path = database_path.with_extension("bqrs");
            let database_dir = database_path
                .parent()
                .expect("database path should have parent")
                .to_path_buf();
            let query_path = bundle
                .cache_path
                .join("queries")
                .join(&lang_dir)
                .join(format!("extract-{lang_dir}.ql"));

            self.file_system
                .create_dir_all(&database_dir)
                .map_err(|source| CodeQlAdapterError::CreateDatabaseDir {
                    path: database_dir.clone(),
                    source,
                })?;

            let query_content = self
                .file_system
                .read_to_string(&query_path)
                .unwrap_or_default();

            let source_fingerprint = compute_source_fingerprint(
                &self.file_system,
                &request.workspace_root,
                &source_files,
                language,
                &self.bundle_version,
                &query_content,
            );

            if let Some(ref fingerprint) = source_fingerprint {
                match try_load_cache(fingerprint, &cache_key_path, &decoded_cache_path) {
                    Ok(parsed) => {
                        if self.progress {
                            eprintln!("    cached");
                        }
                        debug!(
                            language = ?language,
                            cache_key = %cache_key_path.display(),
                            "kalos cache hit"
                        );
                        combined_output.extend_from(parsed);
                        continue;
                    }
                    Err(reason) => {
                        debug!(
                            language = ?language,
                            cache_key = %cache_key_path.display(),
                            decoded = %decoded_cache_path.display(),
                            reason = %reason,
                            "kalos cache miss"
                        );
                    }
                }
            } else {
                debug!(
                    language = ?language,
                    "kalos cache skipped (fingerprint unavailable)"
                );
            }

            let lock_path = database_path.with_extension("lock");
            let _lock_guard = self
                .file_system
                .lock_exclusive(&lock_path)
                .map_err(|source| CodeQlAdapterError::AcquireLock {
                    path: lock_path.clone(),
                    source,
                })?;

            // Re-check cache: another process may have completed while we waited for the lock.
            if let Some(ref fingerprint) = source_fingerprint {
                match try_load_cache(fingerprint, &cache_key_path, &decoded_cache_path) {
                    Ok(parsed) => {
                        if self.progress {
                            eprintln!("    cached (after lock)");
                        }
                        debug!(
                            language = ?language,
                            cache_key = %cache_key_path.display(),
                            "kalos cache hit (after lock)"
                        );
                        combined_output.extend_from(parsed);
                        continue;
                    }
                    Err(reason) => {
                        debug!(
                            language = ?language,
                            reason = %reason,
                            "kalos cache miss (after lock)"
                        );
                    }
                }
            }

            self.ensure_extractor_available(&codeql_program, &request.workspace_root, language)?;

            let database_create_started = if self.progress {
                emit_long_running_phase_context(language);
                eprintln!(
                    "    {} ...",
                    database_create_progress_message(self.is_emulated)
                );
                Some(Instant::now())
            } else {
                None
            };
            self.run_checked(
                &codeql_program,
                build_database_create_args(&database_path, &request.workspace_root, language),
                &request.workspace_root,
                "database create",
                language,
            )?;
            if let Some(started) = database_create_started {
                eprintln!(
                    "    database create done ({})",
                    format_elapsed(started.elapsed())
                );
            }

            let query_run_started = if self.progress {
                eprintln!("    query run ...");
                Some(Instant::now())
            } else {
                None
            };
            self.run_checked(
                &codeql_program,
                build_query_run_args(&database_path, &query_path, &bqrs_path, &bundle.cache_path),
                &request.workspace_root,
                "query run",
                language,
            )?;
            if let Some(started) = query_run_started {
                eprintln!("    query run done ({})", format_elapsed(started.elapsed()));
            }

            let bqrs_decode_started = if self.progress {
                eprintln!("    bqrs decode ...");
                Some(Instant::now())
            } else {
                None
            };
            let decode_output = self.run_checked(
                &codeql_program,
                build_bqrs_decode_args(&bqrs_path),
                &request.workspace_root,
                "bqrs decode",
                language,
            )?;
            if let Some(started) = bqrs_decode_started {
                eprintln!(
                    "    bqrs decode done ({})",
                    format_elapsed(started.elapsed())
                );
            }
            if let Some(ref fingerprint) = source_fingerprint {
                if let Err(error) = write_cache_atomic(&decoded_cache_path, &decode_output.stdout) {
                    warn!(
                        path = %decoded_cache_path.display(),
                        error = %error,
                        "kalos cache: failed to persist decoded cache payload; next run will full-rebuild"
                    );
                } else if let Err(error) =
                    write_cache_atomic(&cache_key_path, fingerprint.as_bytes())
                {
                    warn!(
                        path = %cache_key_path.display(),
                        error = %error,
                        "kalos cache: failed to persist cache key; next run will full-rebuild"
                    );
                } else {
                    debug!(
                        language = ?language,
                        cache_key = %cache_key_path.display(),
                        "kalos cache written"
                    );
                }
            }
            combined_output.extend_from(CpgNormalizer::parse_output(&decode_output.stdout)?);
        }

        let source_file_count = source_files.len();
        let mut analysis =
            self.normalizer
                .normalize(&request.workspace_root, source_files, combined_output)?;
        if source_file_count > 0
            && analysis.cpg.functions().is_empty()
            && analysis.cpg.modules().is_empty()
        {
            analysis.warnings.push(AnalysisWarning {
                file_path: FilePath::from("."),
                message: format!(
                    "CodeQL extraction produced no function or module scopes despite finding {} source files; the CodeQL database may be incomplete - try deleting .kalos/codeql/ and re-running",
                    source_file_count
                ),
                user_facing: true,
            });
        }
        analysis.warnings.extend(language_warnings);
        Ok(analysis)
    }
}

impl<F, R, T> CodeQlAdapter<F, R, T> {
    fn database_root(&self, workspace_root: &Path) -> PathBuf {
        self.database_root
            .clone()
            .unwrap_or_else(|| workspace_root.join(".kalos").join("codeql"))
    }
}

impl<F, R, T> CodeQlAdapter<F, R, T>
where
    R: CommandRunner,
{
    fn run_checked(
        &self,
        program: &Path,
        args: Vec<String>,
        cwd: &Path,
        stage: &'static str,
        language: Language,
    ) -> Result<ProcessOutput, CodeQlAdapterError> {
        let program = program.to_string_lossy().into_owned();
        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let output = self
            .command_runner
            .run(program.as_str(), &arg_refs, cwd)
            .map_err(|source| CodeQlAdapterError::Process {
                stage,
                language: language_name(language).to_owned(),
                source,
            })?;
        if output.exit_code != 0 {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(CodeQlAdapterError::CommandFailed {
                stage,
                language: language_name(language).to_owned(),
                exit_code: output.exit_code,
                guidance: format_command_guidance(&stderr),
                stderr,
            });
        }

        Ok(output)
    }

    fn ensure_extractor_available(
        &self,
        program: &Path,
        cwd: &Path,
        language: Language,
    ) -> Result<(), CodeQlAdapterError> {
        let output = self.run_checked(
            program,
            build_resolve_languages_args(),
            cwd,
            "resolve languages",
            language,
        )?;
        let available_languages = parse_resolved_languages(&output.stdout).map_err(|source| {
            CodeQlAdapterError::ResolveLanguagesOutput {
                language: language_name(language).to_owned(),
                source,
            }
        })?;

        if language_extractor_names(language)
            .iter()
            .any(|candidate| available_languages.contains(*candidate))
        {
            return Ok(());
        }

        Err(CodeQlAdapterError::ExtractorUnavailable {
            language: language_name(language).to_owned(),
            available_languages: format_available_languages(&available_languages),
        })
    }
}

fn build_resolve_languages_args() -> Vec<String> {
    vec![
        "resolve".to_owned(),
        "languages".to_owned(),
        "--format=json".to_owned(),
    ]
}

fn build_database_create_args(
    database_path: &Path,
    workspace_root: &Path,
    language: Language,
) -> Vec<String> {
    vec![
        "database".to_owned(),
        "create".to_owned(),
        "--overwrite".to_owned(),
        database_path.to_string_lossy().into_owned(),
        "--language".to_owned(),
        CodeQlAdapter::<(), (), ()>::language_pack(language).to_owned(),
        "--source-root".to_owned(),
        workspace_root.to_string_lossy().into_owned(),
    ]
}

fn build_query_run_args(
    database_path: &Path,
    query_path: &Path,
    bqrs_path: &Path,
    search_path: &Path,
) -> Vec<String> {
    vec![
        "query".to_owned(),
        "run".to_owned(),
        query_path.to_string_lossy().into_owned(),
        "--database".to_owned(),
        database_path.to_string_lossy().into_owned(),
        "--output".to_owned(),
        bqrs_path.to_string_lossy().into_owned(),
        "--search-path".to_owned(),
        search_path.to_string_lossy().into_owned(),
    ]
}

fn build_bqrs_decode_args(bqrs_path: &Path) -> Vec<String> {
    vec![
        "bqrs".to_owned(),
        "decode".to_owned(),
        "--format=json".to_owned(),
        bqrs_path.to_string_lossy().into_owned(),
    ]
}

fn parse_resolved_languages(output: &[u8]) -> Result<BTreeSet<String>, serde_json::Error> {
    let languages = serde_json::from_slice::<BTreeMap<String, serde_json::Value>>(output)?;
    Ok(languages.into_keys().collect())
}

fn language_extractor_names(language: Language) -> &'static [&'static str] {
    match language {
        Language::Python => &["python"],
        Language::TypeScript => &["javascript-typescript", "javascript"],
        Language::Rust => &["rust"],
        Language::Go => &["go"],
    }
}

fn format_available_languages(languages: &BTreeSet<String>) -> String {
    if languages.is_empty() {
        "(none)".to_owned()
    } else {
        languages.iter().cloned().collect::<Vec<_>>().join(", ")
    }
}

fn database_create_progress_message(is_emulated: bool) -> &'static str {
    if is_emulated {
        "database create (first run — this may take several minutes; running under emulation)"
    } else {
        "database create (first run — this may take several minutes)"
    }
}

fn format_elapsed(elapsed: Duration) -> String {
    if elapsed.as_secs() < 60 {
        let secs = elapsed.as_secs();
        let tenths = elapsed.subsec_millis() / 100;
        format!("{secs}.{tenths}s")
    } else {
        let secs = elapsed.as_secs();
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

fn emit_long_running_phase_context(language: Language) {
    eprintln!(
        "    phase timing: long CodeQL phases for {} report elapsed time on completion",
        language_name(language)
    );
    eprintln!(
        "    timeout mitigation: if a CodeQL phase exceeds the harness timeout, retry with --exclude for generated/vendor paths, --diff for a bounded target set, --cache-dir to reuse CodeQL work, or --min-language-ratio to skip incidental languages"
    );
}

fn count_source_files_by_language(
    source_files: &BTreeMap<FilePath, SourceFile>,
) -> BTreeMap<Language, usize> {
    let mut counts = BTreeMap::new();
    for source_file in source_files.values() {
        *counts.entry(source_file.language).or_insert(0) += 1;
    }
    counts
}

fn emit_analysis_inventory_progress(language_counts: &BTreeMap<Language, usize>) {
    let total_files = language_counts.values().sum::<usize>();
    let language_breakdown = language_counts
        .iter()
        .map(|(language, count)| format!("{}={}", language_name(*language), count))
        .collect::<Vec<_>>()
        .join(", ");
    eprintln!(
        "  codeql: found {} source files ({})",
        total_files, language_breakdown
    );
}

fn filter_incidental_languages(
    source_files: &BTreeMap<FilePath, SourceFile>,
    min_ratio: f64,
) -> (BTreeSet<Language>, Vec<AnalysisWarning>) {
    let total_file_count = source_files.len();
    if total_file_count == 0 {
        return (BTreeSet::new(), Vec::new());
    }

    let language_counts = count_source_files_by_language(source_files);
    let mut languages = language_counts
        .iter()
        .filter_map(|(language, count)| {
            let ratio = (*count as f64) / (total_file_count as f64);
            (ratio >= min_ratio).then_some(*language)
        })
        .collect::<BTreeSet<_>>();

    if languages.is_empty() {
        if let Some((language, _)) = language_counts
            .iter()
            .max_by_key(|(language, count)| (**count, **language))
        {
            languages.insert(*language);
        }
    }

    let warnings = language_counts
        .into_iter()
        .filter(|(language, _)| !languages.contains(language))
        .map(|(language, count)| {
            let percentage = (count as f64) * 100.0 / (total_file_count as f64);
            AnalysisWarning {
                file_path: FilePath::from("."),
                message: format!(
                    "skipped CodeQL database creation for {} ({} of {} files, {:.1}%): below minimum language ratio ({:.1}%)",
                    language_name(language),
                    count,
                    total_file_count,
                    percentage,
                    min_ratio * 100.0
                ),
                user_facing: true,
            }
        })
        .collect();

    (languages, warnings)
}

fn format_command_guidance(stderr: &str) -> String {
    let (cause, hint) = classify_codeql_error(stderr);
    let detail = stderr.trim();
    format!("  cause: {cause}\n  detail: {detail}\n\n  next steps:\n    - {hint}")
}

fn classify_codeql_error(stderr: &str) -> (&'static str, &'static str) {
    if stderr.contains(".ql does not exist") {
        (
            "a required CodeQL query file is missing (the bundle may be incomplete)",
            "The CodeQL bundle may be incomplete. Try deleting the cache directory and re-running:\n        rm -rf ~/.cache/kalos/codeql/\n        kalos check\n    - If the issue persists, please report: https://github.com/RK0429/kalos/issues",
        )
    } else if stderr.contains("does not exist") {
        (
            "the CodeQL database output directory does not exist",
            "This is likely a kalos internal error. Please report an issue: https://github.com/RK0429/kalos/issues",
        )
    } else {
        (
            "CodeQL encountered an error during extraction",
            "Verify that CodeQL supports your project's language and build configuration. If the problem persists, please report an issue: https://github.com/RK0429/kalos/issues",
        )
    }
}

fn language_name(language: Language) -> &'static str {
    match language {
        Language::Python => "python",
        Language::TypeScript => "typescript",
        Language::Rust => "rust",
        Language::Go => "go",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::convert::Infallible;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use tempfile::TempDir;

    use super::{
        CacheMissReason, CodeQlAdapter, CodeQlAdapterError, codeql_executable_path,
        compute_source_fingerprint, database_create_progress_message, filter_incidental_languages,
        format_available_languages, format_command_guidance, format_elapsed,
        parse_resolved_languages, supported_extensions_display, try_load_cache, write_cache_atomic,
    };
    use crate::domains::FilePath;
    use crate::domains::cpg::{EdgeKind, Language, NodeKind, SourceFile};
    use crate::platform::fs::{InMemoryFileSystem, RealFileSystem};
    use crate::platform::process::{CommandRunner, MockCommandRunner, ProcessError, ProcessOutput};
    use crate::ports::extractor::{ExtractionRequest, ExtractorPort};
    use crate::ports::tool_cache::{ResolvedToolBundle, ToolCachePort, ToolCacheRequest};

    mod empty_scope {
        use super::*;

        #[test]
        fn extract_warns_when_codeql_output_has_no_scopes_despite_source_files() {
            let mut file_system = InMemoryFileSystem::new();
            file_system.insert("/workspace/src/main.rs", "fn main() {}\n");
            let command_runner = MockCommandRunner::new();
            push_language_resolution_result(
                &command_runner,
                &["go", "javascript", "python", "rust"],
            );
            command_runner
                .push_result(Ok(ProcessOutput {
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    exit_code: 0,
                }))
                .unwrap();
            command_runner
                .push_result(Ok(ProcessOutput {
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    exit_code: 0,
                }))
                .unwrap();
            command_runner
                .push_result(Ok(ProcessOutput {
                    stdout: b"{}".to_vec(),
                    stderr: Vec::new(),
                    exit_code: 0,
                }))
                .unwrap();
            let adapter = CodeQlAdapter::new(
                file_system,
                command_runner,
                MockToolCachePort {
                    bundle: mock_bundle(),
                },
                "2.0.0",
                Vec::new(),
            );

            let analysis = adapter
                .extract(&ExtractionRequest {
                    workspace_root: PathBuf::from("/workspace"),
                    analysis_targets: vec![FilePath::from(".")],
                })
                .unwrap();

            assert!(analysis.cpg.functions().is_empty());
            assert!(analysis.cpg.modules().is_empty());
            assert!(analysis.warnings.iter().any(|warning| {
                warning.user_facing
                    && warning
                        .message
                        .contains("produced no function or module scopes")
                    && warning.message.contains(".kalos/codeql/")
            }));
        }
    }

    #[derive(Clone, Debug)]
    struct MockToolCachePort {
        bundle: ResolvedToolBundle,
    }

    impl ToolCachePort for MockToolCachePort {
        type Error = Infallible;

        fn resolve_bundle(
            &self,
            _request: &ToolCacheRequest,
        ) -> Result<ResolvedToolBundle, Self::Error> {
            Ok(self.bundle.clone())
        }
    }

    /// Command runner for concurrent tests that delays database creation so
    /// competing extractions contend on the same lock path.
    #[derive(Clone)]
    struct SlowCreateCommandRunner {
        invocation_count: Arc<AtomicUsize>,
    }

    impl SlowCreateCommandRunner {
        fn new() -> Self {
            Self {
                invocation_count: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn invocation_count(&self) -> usize {
            self.invocation_count.load(Ordering::Relaxed)
        }
    }

    impl CommandRunner for SlowCreateCommandRunner {
        fn run(
            &self,
            _program: &str,
            args: &[&str],
            _cwd: &Path,
        ) -> Result<ProcessOutput, ProcessError> {
            self.invocation_count.fetch_add(1, Ordering::Relaxed);

            match args {
                ["database", "create", ..] => {
                    std::thread::sleep(Duration::from_millis(200));
                    Ok(ProcessOutput {
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                        exit_code: 0,
                    })
                }
                ["resolve", "languages", "--format=json"] => Ok(ProcessOutput {
                    stdout: br#"{"rust":["/cache/codeql/2.0.0/rust"]}"#.to_vec(),
                    stderr: Vec::new(),
                    exit_code: 0,
                }),
                ["query", "run", ..] => Ok(ProcessOutput {
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    exit_code: 0,
                }),
                ["bqrs", "decode", ..] => Ok(ProcessOutput {
                    stdout: load_fixture("rust.json").into_bytes(),
                    stderr: Vec::new(),
                    exit_code: 0,
                }),
                other => panic!("unexpected command args: {other:?}"),
            }
        }
    }

    #[test]
    fn language_pack_resolution_covers_supported_languages() {
        assert_eq!(
            CodeQlAdapter::<(), (), ()>::language_pack(Language::Python),
            "python"
        );
        assert_eq!(
            CodeQlAdapter::<(), (), ()>::language_pack(Language::TypeScript),
            "javascript-typescript"
        );
        assert_eq!(
            CodeQlAdapter::<(), (), ()>::language_pack(Language::Rust),
            "rust"
        );
        assert_eq!(
            CodeQlAdapter::<(), (), ()>::language_pack(Language::Go),
            "go"
        );
    }

    #[test]
    fn codeql_executable_path_uses_platform_executable_suffix() {
        assert_eq!(
            codeql_executable_path(Path::new("/cache/codeql/2.0.0")),
            Path::new("/cache/codeql/2.0.0")
                .join(format!("codeql{}", std::env::consts::EXE_SUFFIX))
        );
    }

    #[test]
    fn codeql_adapter_extracts_with_mocks() {
        let mut file_system = InMemoryFileSystem::new();
        file_system.insert("/workspace/src/app.py", "def main():\n    return 1\n");
        let command_runner = MockCommandRunner::new();
        push_language_resolution_result(&command_runner, &["go", "javascript", "python", "rust"]);
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: load_fixture("python.json").into_bytes(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
        let adapter = CodeQlAdapter::new(
            file_system,
            command_runner.clone(),
            MockToolCachePort {
                bundle: ResolvedToolBundle {
                    tool_name: "codeql".to_owned(),
                    version: "2.0.0".to_owned(),
                    cache_path: PathBuf::from("/cache/codeql/2.0.0"),
                    checksum: "a".repeat(64),
                },
            },
            "2.0.0",
            Vec::new(),
        );

        let analysis = adapter
            .extract(&ExtractionRequest {
                workspace_root: PathBuf::from("/workspace"),
                analysis_targets: vec![FilePath::from(".")],
            })
            .unwrap();

        assert_eq!(
            analysis.source_files.keys().cloned().collect::<Vec<_>>(),
            vec![FilePath::from("src/app.py")]
        );
        assert_eq!(
            analysis
                .cpg
                .nodes
                .iter()
                .map(|node| node.kind)
                .collect::<Vec<_>>(),
            vec![
                NodeKind::Module,
                NodeKind::Class,
                NodeKind::Function,
                NodeKind::Variable,
                NodeKind::Variable,
                NodeKind::Parameter,
            ]
        );
        assert_eq!(
            analysis
                .cpg
                .edges
                .iter()
                .map(|edge| edge.kind)
                .collect::<Vec<_>>(),
            vec![
                EdgeKind::Contains,
                EdgeKind::Contains,
                EdgeKind::Call,
                EdgeKind::Contains,
                EdgeKind::Contains,
                EdgeKind::ControlFlow,
                EdgeKind::Contains,
                EdgeKind::DataFlow,
                EdgeKind::DataFlow,
                EdgeKind::ControlFlow,
                EdgeKind::ControlFlow,
            ]
        );

        let invocations = command_runner.invocations().unwrap();
        assert_eq!(invocations.len(), 4);
        assert_eq!(
            PathBuf::from(&invocations[0].program),
            codeql_executable_path(Path::new("/cache/codeql/2.0.0"))
        );
        assert_eq!(
            invocations[0].args,
            vec!["resolve", "languages", "--format=json"]
        );
        assert_eq!(invocations[1].args[0], "database");
        assert_eq!(invocations[1].args[1], "create");
        assert_eq!(invocations[1].args[4], "--language");
        assert_eq!(invocations[1].args[5], "python");
        assert_eq!(invocations[2].args[0], "query");
        assert_eq!(invocations[2].args[1], "run");
        assert_eq!(
            invocations[2].args[2],
            "/cache/codeql/2.0.0/queries/python/extract-python.ql"
        );
        assert!(!invocations[2].args.iter().any(|arg| arg == "--format=json"));
        assert!(invocations[2].args.iter().any(|arg| arg == "--output"));
        assert!(invocations[2].args.iter().any(|arg| arg == "--search-path"));
        assert_eq!(
            invocations[2]
                .args
                .windows(2)
                .find(|pair| pair[0] == "--search-path")
                .map(|pair| pair[1].as_str()),
            Some("/cache/codeql/2.0.0")
        );
        assert_eq!(invocations[3].args[0], "bqrs");
        assert_eq!(invocations[3].args[1], "decode");
        assert!(invocations[3].args.iter().any(|arg| arg == "--format=json"));
        assert!(!invocations[3].args.iter().any(|arg| arg == "--output=-"));
    }

    #[test]
    fn codeql_adapter_passes_overwrite_flag() {
        let mut file_system = InMemoryFileSystem::new();
        file_system.insert("/workspace/src/app.py", "def main():\n    return 1\n");
        let command_runner = MockCommandRunner::new();
        push_language_resolution_result(&command_runner, &["go", "javascript", "python", "rust"]);
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: b"{}".to_vec(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
        let adapter = CodeQlAdapter::new(
            file_system,
            command_runner.clone(),
            MockToolCachePort {
                bundle: ResolvedToolBundle {
                    tool_name: "codeql".to_owned(),
                    version: "2.0.0".to_owned(),
                    cache_path: PathBuf::from("/cache/codeql/2.0.0"),
                    checksum: "a".repeat(64),
                },
            },
            "2.0.0",
            Vec::new(),
        );

        adapter
            .extract(&ExtractionRequest {
                workspace_root: PathBuf::from("/workspace"),
                analysis_targets: vec![FilePath::from(".")],
            })
            .unwrap();

        let invocations = command_runner.invocations().unwrap();
        assert_eq!(invocations.len(), 4);
        assert!(invocations[1].args.iter().any(|arg| arg == "--overwrite"));
    }

    #[test]
    fn codeql_adapter_creates_database_parent_directory() {
        let mut file_system = InMemoryFileSystem::new();
        file_system.insert("/workspace/src/app.py", "def main():\n    return 1\n");
        let file_system_for_assertion = file_system.clone();
        let command_runner = MockCommandRunner::new();
        push_language_resolution_result(&command_runner, &["go", "javascript", "python", "rust"]);
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: b"{}".to_vec(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
        let adapter = CodeQlAdapter::new(
            file_system,
            command_runner,
            MockToolCachePort {
                bundle: ResolvedToolBundle {
                    tool_name: "codeql".to_owned(),
                    version: "2.0.0".to_owned(),
                    cache_path: PathBuf::from("/cache/codeql/2.0.0"),
                    checksum: "a".repeat(64),
                },
            },
            "2.0.0",
            Vec::new(),
        );

        adapter
            .extract(&ExtractionRequest {
                workspace_root: PathBuf::from("/workspace"),
                analysis_targets: vec![FilePath::from(".")],
            })
            .unwrap();

        assert!(
            file_system_for_assertion
                .created_dirs()
                .contains(&PathBuf::from("/workspace/.kalos/codeql"))
        );
    }

    #[test]
    fn codeql_adapter_uses_external_database_root_when_configured() {
        let mut file_system = InMemoryFileSystem::new();
        file_system.insert("/workspace/src/app.py", "def main():\n    return 1\n");
        let file_system_for_assertion = file_system.clone();
        let command_runner = MockCommandRunner::new();
        push_language_resolution_result(&command_runner, &["go", "javascript", "python", "rust"]);
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: b"{}".to_vec(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
        let adapter = CodeQlAdapter::new(
            file_system,
            command_runner,
            MockToolCachePort {
                bundle: ResolvedToolBundle {
                    tool_name: "codeql".to_owned(),
                    version: "2.0.0".to_owned(),
                    cache_path: PathBuf::from("/cache/codeql/2.0.0"),
                    checksum: "a".repeat(64),
                },
            },
            "2.0.0",
            Vec::new(),
        )
        .with_database_root("/external/kalos-cache/codeql/databases");

        adapter
            .extract(&ExtractionRequest {
                workspace_root: PathBuf::from("/workspace"),
                analysis_targets: vec![FilePath::from(".")],
            })
            .unwrap();

        let created_dirs = file_system_for_assertion.created_dirs();
        assert!(created_dirs.contains(&PathBuf::from("/external/kalos-cache/codeql/databases")));
        assert!(!created_dirs.contains(&PathBuf::from("/workspace/.kalos/codeql")));
    }

    #[test]
    fn codeql_adapter_normalizes_workspace_relative_paths() {
        let temp = TempDir::new().unwrap();
        let workspace_root = fs::canonicalize(temp.path()).unwrap();
        let mut file_system = InMemoryFileSystem::new();
        let file_path = workspace_root.join("src/lib.rs");
        let file_path_string = file_path.to_string_lossy().into_owned();
        file_system.insert(&file_path_string, "fn main() {}\n");
        let fixture = format!(
            r#"{{
                "modules": [{{"id":"m1","name":"crate","file":"{}","start_line":1,"end_line":1,"language":"rust"}}]
            }}"#,
            file_path.display()
        );
        let command_runner = MockCommandRunner::new();
        push_language_resolution_result(&command_runner, &["go", "javascript", "python", "rust"]);
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: fixture.into_bytes(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
        let adapter = CodeQlAdapter::new(
            file_system,
            command_runner,
            MockToolCachePort {
                bundle: ResolvedToolBundle {
                    tool_name: "codeql".to_owned(),
                    version: "2.0.0".to_owned(),
                    cache_path: PathBuf::from("/cache/codeql/2.0.0"),
                    checksum: "a".repeat(64),
                },
            },
            "2.0.0",
            Vec::new(),
        );

        let analysis = adapter
            .extract(&ExtractionRequest {
                workspace_root: workspace_root.clone(),
                analysis_targets: vec![FilePath::from(".")],
            })
            .unwrap();

        assert_eq!(
            analysis.cpg.nodes[0].location.file_path,
            FilePath::from("src/lib.rs")
        );
    }

    #[test]
    fn codeql_adapter_query_run_does_not_pass_format_flag() {
        let mut file_system = InMemoryFileSystem::new();
        file_system.insert("/workspace/src/app.py", "def main():\n    return 1\n");
        let command_runner = MockCommandRunner::new();
        push_language_resolution_result(&command_runner, &["go", "javascript", "python", "rust"]);
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: load_fixture("python.json").into_bytes(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
        let adapter = CodeQlAdapter::new(
            file_system,
            command_runner.clone(),
            MockToolCachePort {
                bundle: ResolvedToolBundle {
                    tool_name: "codeql".to_owned(),
                    version: "2.0.0".to_owned(),
                    cache_path: PathBuf::from("/cache/codeql/2.0.0"),
                    checksum: "a".repeat(64),
                },
            },
            "2.0.0",
            Vec::new(),
        );

        adapter
            .extract(&ExtractionRequest {
                workspace_root: PathBuf::from("/workspace"),
                analysis_targets: vec![FilePath::from(".")],
            })
            .unwrap();

        let invocations = command_runner.invocations().unwrap();
        assert_eq!(
            invocations[2].args[2],
            "/cache/codeql/2.0.0/queries/python/extract-python.ql"
        );
        assert!(!invocations[2].args.iter().any(|arg| arg == "--format=json"));
        assert!(invocations[2].args.iter().any(|arg| arg == "--search-path"));
        assert!(invocations[3].args.iter().any(|arg| arg == "--format=json"));
    }

    #[test]
    fn codeql_adapter_skips_extraction_on_cache_hit() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path();
        fs::create_dir_all(workspace_root.join("src")).unwrap();
        fs::write(workspace_root.join("src/lib.rs"), "fn main() {}\n").unwrap();
        fs::create_dir_all(workspace_root.join(".kalos/codeql")).unwrap();
        let source_files = single_source_file("src/lib.rs", Language::Rust);
        let fingerprint = compute_source_fingerprint(
            &RealFileSystem,
            workspace_root,
            &source_files,
            Language::Rust,
            "2.0.0",
            "",
        )
        .unwrap();
        fs::write(
            workspace_root.join(".kalos/codeql/rust.cache_key"),
            fingerprint,
        )
        .unwrap();
        fs::write(
            workspace_root.join(".kalos/codeql/rust.decoded.json"),
            load_fixture("rust.json"),
        )
        .unwrap();

        let command_runner = MockCommandRunner::new();
        let adapter = CodeQlAdapter::new(
            RealFileSystem,
            command_runner.clone(),
            MockToolCachePort {
                bundle: mock_bundle(),
            },
            "2.0.0",
            Vec::new(),
        );

        let analysis = adapter
            .extract(&ExtractionRequest {
                workspace_root: workspace_root.to_path_buf(),
                analysis_targets: vec![FilePath::from(".")],
            })
            .unwrap();

        assert_eq!(
            analysis.source_files.keys().cloned().collect::<Vec<_>>(),
            vec![FilePath::from("src/lib.rs")]
        );
        assert!(command_runner.invocations().unwrap().is_empty());
    }

    #[test]
    fn codeql_adapter_runs_extraction_on_cache_miss() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path();
        fs::create_dir_all(workspace_root.join("src")).unwrap();
        fs::write(workspace_root.join("src/lib.rs"), "fn main() {}\n").unwrap();
        fs::create_dir_all(workspace_root.join(".kalos/codeql")).unwrap();
        fs::write(
            workspace_root.join(".kalos/codeql/rust.cache_key"),
            "stale-hash",
        )
        .unwrap();

        let command_runner = MockCommandRunner::new();
        push_successful_language_results(&command_runner, 1);
        let adapter = CodeQlAdapter::new(
            RealFileSystem,
            command_runner.clone(),
            MockToolCachePort {
                bundle: mock_bundle(),
            },
            "2.0.0",
            Vec::new(),
        );

        adapter
            .extract(&ExtractionRequest {
                workspace_root: workspace_root.to_path_buf(),
                analysis_targets: vec![FilePath::from(".")],
            })
            .unwrap();

        assert_eq!(command_runner.invocations().unwrap().len(), 4);
        let source_files = single_source_file("src/lib.rs", Language::Rust);
        let fingerprint = compute_source_fingerprint(
            &RealFileSystem,
            workspace_root,
            &source_files,
            Language::Rust,
            "2.0.0",
            "",
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(workspace_root.join(".kalos/codeql/rust.cache_key"))
                .unwrap()
                .trim(),
            fingerprint
        );
        assert!(
            workspace_root
                .join(".kalos/codeql/rust.decoded.json")
                .exists()
        );
    }

    #[test]
    fn progress_emits_first_run_hint_on_cache_miss() {
        let adapter = CodeQlAdapter::new(
            RealFileSystem,
            MockCommandRunner::new(),
            MockToolCachePort {
                bundle: mock_bundle(),
            },
            "2.0.0",
            Vec::new(),
        )
        .with_progress();

        assert!(adapter.progress);
        assert!(database_create_progress_message(adapter.is_emulated).contains("first run"));
    }

    #[test]
    fn progress_emits_emulation_hint_when_enabled() {
        let adapter = CodeQlAdapter::new(
            RealFileSystem,
            MockCommandRunner::new(),
            MockToolCachePort {
                bundle: mock_bundle(),
            },
            "2.0.0",
            Vec::new(),
        )
        .with_progress()
        .with_emulated();

        assert!(adapter.progress);
        assert!(adapter.is_emulated);
        assert!(
            database_create_progress_message(adapter.is_emulated)
                .contains("running under emulation")
        );
    }

    #[test]
    fn format_elapsed_renders_sub_minute_durations() {
        assert_eq!(format_elapsed(Duration::from_millis(3_240)), "3.2s");
        assert_eq!(format_elapsed(Duration::from_millis(59_990)), "59.9s");
    }

    #[test]
    fn format_elapsed_renders_minute_durations() {
        assert_eq!(format_elapsed(Duration::from_secs(60)), "1m 0s");
        assert_eq!(format_elapsed(Duration::from_secs(1_629)), "27m 9s");
    }

    #[test]
    fn codeql_adapter_cache_miss_when_source_modified() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path();
        fs::create_dir_all(workspace_root.join("src")).unwrap();
        fs::create_dir_all(workspace_root.join(".kalos/codeql")).unwrap();
        let source_path = workspace_root.join("src/lib.rs");
        fs::write(&source_path, "fn main() {}\n").unwrap();

        let first_runner = MockCommandRunner::new();
        push_successful_language_results(&first_runner, 1);
        let first_adapter = CodeQlAdapter::new(
            RealFileSystem,
            first_runner.clone(),
            MockToolCachePort {
                bundle: mock_bundle(),
            },
            "2.0.0",
            Vec::new(),
        );

        first_adapter
            .extract(&ExtractionRequest {
                workspace_root: workspace_root.to_path_buf(),
                analysis_targets: vec![FilePath::from(".")],
            })
            .unwrap();

        let source_files = single_source_file("src/lib.rs", Language::Rust);
        let initial_fingerprint = compute_source_fingerprint(
            &RealFileSystem,
            workspace_root,
            &source_files,
            Language::Rust,
            "2.0.0",
            "",
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(workspace_root.join(".kalos/codeql/rust.cache_key"))
                .unwrap()
                .trim(),
            initial_fingerprint
        );

        fs::write(&source_path, "fn main() { println!(\"changed\"); }\n").unwrap();

        let second_runner = MockCommandRunner::new();
        push_successful_language_results(&second_runner, 1);
        let second_adapter = CodeQlAdapter::new(
            RealFileSystem,
            second_runner.clone(),
            MockToolCachePort {
                bundle: mock_bundle(),
            },
            "2.0.0",
            Vec::new(),
        );

        second_adapter
            .extract(&ExtractionRequest {
                workspace_root: workspace_root.to_path_buf(),
                analysis_targets: vec![FilePath::from(".")],
            })
            .unwrap();

        let updated_fingerprint = compute_source_fingerprint(
            &RealFileSystem,
            workspace_root,
            &source_files,
            Language::Rust,
            "2.0.0",
            "",
        )
        .unwrap();
        assert_ne!(updated_fingerprint, initial_fingerprint);
        assert_eq!(second_runner.invocations().unwrap().len(), 4);
        assert_eq!(
            fs::read_to_string(workspace_root.join(".kalos/codeql/rust.cache_key"))
                .unwrap()
                .trim(),
            updated_fingerprint
        );
    }

    #[test]
    fn query_content_change_invalidates_cache() {
        let source_files = single_source_file("src/lib.rs", Language::Rust);
        let mut file_system = InMemoryFileSystem::new();
        file_system.insert("/workspace/src/lib.rs", "fn main() {}\n");

        let fp_old = compute_source_fingerprint(
            &file_system,
            Path::new("/workspace"),
            &source_files,
            Language::Rust,
            "2.0.0",
            "query predicate modules() { m.isTopLevel() }",
        )
        .unwrap();

        let fp_new = compute_source_fingerprint(
            &file_system,
            Path::new("/workspace"),
            &source_files,
            Language::Rust,
            "2.0.0",
            "query predicate modules() { exists(Module m | ...) }",
        )
        .unwrap();

        assert_ne!(
            fp_old, fp_new,
            "changing query content must invalidate the cache fingerprint"
        );
    }

    #[test]
    fn write_cache_atomic_creates_final_file_and_cleans_up_tmp() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("rust.cache_key");

        write_cache_atomic(&target, b"deadbeef").unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "deadbeef");
        let leftover = fs::read_dir(temp.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .map(|name| name.ends_with(".tmp"))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        assert!(leftover.is_empty(), "no temp files should remain");
    }

    #[test]
    fn write_cache_atomic_replaces_existing_content() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("rust.cache_key");
        fs::write(&target, b"stale").unwrap();

        write_cache_atomic(&target, b"fresh").unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "fresh");
    }

    #[test]
    fn try_load_cache_reports_missing_key_file() {
        let temp = TempDir::new().unwrap();
        let reason = try_load_cache(
            "fingerprint",
            &temp.path().join("rust.cache_key"),
            &temp.path().join("rust.decoded.json"),
        )
        .unwrap_err();

        assert!(matches!(reason, CacheMissReason::KeyFileMissing(_)));
    }

    #[test]
    fn try_load_cache_reports_key_mismatch_when_fingerprint_changed() {
        let temp = TempDir::new().unwrap();
        let cache_key_path = temp.path().join("rust.cache_key");
        let decoded_cache_path = temp.path().join("rust.decoded.json");
        fs::write(&cache_key_path, "old-fingerprint").unwrap();
        fs::write(&decoded_cache_path, b"{}").unwrap();

        let reason =
            try_load_cache("new-fingerprint", &cache_key_path, &decoded_cache_path).unwrap_err();

        assert!(matches!(reason, CacheMissReason::KeyMismatch));
    }

    #[test]
    fn try_load_cache_reports_missing_decoded_payload() {
        let temp = TempDir::new().unwrap();
        let cache_key_path = temp.path().join("rust.cache_key");
        let decoded_cache_path = temp.path().join("rust.decoded.json");
        fs::write(&cache_key_path, "fp").unwrap();

        let reason = try_load_cache("fp", &cache_key_path, &decoded_cache_path).unwrap_err();

        assert!(matches!(reason, CacheMissReason::DecodedFileMissing(_)));
    }

    #[test]
    fn try_load_cache_reports_parse_error_on_truncated_payload() {
        let temp = TempDir::new().unwrap();
        let cache_key_path = temp.path().join("rust.cache_key");
        let decoded_cache_path = temp.path().join("rust.decoded.json");
        fs::write(&cache_key_path, "fp").unwrap();
        fs::write(&decoded_cache_path, b"{\"functions\": [").unwrap();

        let reason = try_load_cache("fp", &cache_key_path, &decoded_cache_path).unwrap_err();

        assert!(matches!(reason, CacheMissReason::DecodedParseError(_)));
    }

    #[test]
    fn concurrent_extractions_serialize_database_creation() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().to_path_buf();
        fs::create_dir_all(workspace_root.join("src")).unwrap();
        fs::write(workspace_root.join("src/lib.rs"), "fn main() {}\n").unwrap();

        let command_runner = SlowCreateCommandRunner::new();
        let adapter = CodeQlAdapter::new(
            RealFileSystem,
            command_runner.clone(),
            MockToolCachePort {
                bundle: mock_bundle(),
            },
            "2.0.0",
            Vec::new(),
        );
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();

        for _ in 0..2 {
            let adapter = adapter.clone();
            let barrier = barrier.clone();
            let workspace_root = workspace_root.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                adapter
                    .extract(&ExtractionRequest {
                        workspace_root,
                        analysis_targets: vec![FilePath::from(".")],
                    })
                    .unwrap()
                    .source_files
                    .len()
            }));
        }

        for handle in handles {
            assert_eq!(handle.join().unwrap(), 1);
        }

        assert_eq!(command_runner.invocation_count(), 4);

        let source_files = single_source_file("src/lib.rs", Language::Rust);
        let fingerprint = compute_source_fingerprint(
            &RealFileSystem,
            &workspace_root,
            &source_files,
            Language::Rust,
            "2.0.0",
            "",
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(workspace_root.join(".kalos/codeql/rust.cache_key"))
                .unwrap()
                .trim(),
            fingerprint
        );
        assert!(
            workspace_root
                .join(".kalos/codeql/rust.decoded.json")
                .exists()
        );
    }

    #[test]
    fn incidental_language_is_skipped_by_default() {
        let file_system = mixed_language_file_system(20, 1);
        let command_runner = MockCommandRunner::new();
        push_successful_language_results(&command_runner, 1);
        let adapter = CodeQlAdapter::new(
            file_system,
            command_runner.clone(),
            MockToolCachePort {
                bundle: mock_bundle(),
            },
            "2.0.0",
            Vec::new(),
        );

        let analysis = adapter
            .extract(&ExtractionRequest {
                workspace_root: PathBuf::from("/workspace"),
                analysis_targets: vec![FilePath::from(".")],
            })
            .unwrap();

        let invocations = command_runner.invocations().unwrap();
        assert_eq!(invocations.len(), 4);
        assert!(analysis.warnings.iter().any(|warning| {
            warning.user_facing
                && warning
                    .message
                    .contains("skipped CodeQL database creation for python")
        }));
    }

    #[test]
    fn empty_source_file_set_emits_supported_extensions_warning() {
        let mut file_system = InMemoryFileSystem::new();
        file_system.insert("/workspace/README.md", "# placeholder\n");
        let command_runner = MockCommandRunner::new();
        let adapter = CodeQlAdapter::new(
            file_system,
            command_runner.clone(),
            MockToolCachePort {
                bundle: mock_bundle(),
            },
            "2.0.0",
            Vec::new(),
        );

        let analysis = adapter
            .extract(&ExtractionRequest {
                workspace_root: PathBuf::from("/workspace"),
                analysis_targets: vec![FilePath::from(".")],
            })
            .unwrap();

        assert!(analysis.source_files.is_empty());
        assert_eq!(analysis.warnings.len(), 1);
        assert_eq!(analysis.warnings[0].file_path, FilePath::from("."));
        assert!(analysis.warnings[0].user_facing);
        assert_eq!(
            analysis.warnings[0].message,
            format!(
                "no files with supported extensions ({}) were found in the analysis targets",
                supported_extensions_display()
            )
        );
        assert!(command_runner.invocations().unwrap().is_empty());
    }

    #[test]
    fn incidental_language_included_when_ratio_set_to_zero() {
        let file_system = mixed_language_file_system(20, 1);
        let command_runner = MockCommandRunner::new();
        push_successful_language_results(&command_runner, 2);
        let adapter = CodeQlAdapter::new(
            file_system,
            command_runner.clone(),
            MockToolCachePort {
                bundle: mock_bundle(),
            },
            "2.0.0",
            Vec::new(),
        )
        .with_min_language_ratio(0.0);

        adapter
            .extract(&ExtractionRequest {
                workspace_root: PathBuf::from("/workspace"),
                analysis_targets: vec![FilePath::from(".")],
            })
            .unwrap();

        let invocations = command_runner.invocations().unwrap();
        assert_eq!(invocations.len(), 8);
    }

    #[test]
    fn filter_incidental_languages_keeps_dominant() {
        let source_files = source_files_from_counts(20, 1);

        let (languages, warnings) = filter_incidental_languages(&source_files, 0.05);

        assert_eq!(languages, BTreeSet::from([Language::Rust]));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("python"));
    }

    #[test]
    fn filter_incidental_languages_keeps_all_when_above_threshold() {
        let source_files = source_files_from_counts(10, 10);

        let (languages, warnings) = filter_incidental_languages(&source_files, 0.05);

        assert_eq!(
            languages,
            BTreeSet::from([Language::Python, Language::Rust])
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn filter_incidental_languages_never_skips_all() {
        let source_files = source_files_from_counts(1, 1);

        let (languages, warnings) = filter_incidental_languages(&source_files, 0.99);

        assert_eq!(languages.len(), 1);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn codeql_adapter_returns_error_on_command_failure() {
        let mut file_system = InMemoryFileSystem::new();
        file_system.insert("/workspace/web/app.tsx", "export const App = () => null;\n");
        let command_runner = MockCommandRunner::new();
        push_language_resolution_result(&command_runner, &["go", "javascript", "python", "rust"]);
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: Vec::new(),
                stderr: b"database create failed".to_vec(),
                exit_code: 1,
            }))
            .unwrap();
        let adapter = CodeQlAdapter::new(
            file_system,
            command_runner,
            MockToolCachePort {
                bundle: ResolvedToolBundle {
                    tool_name: "codeql".to_owned(),
                    version: "2.0.0".to_owned(),
                    cache_path: PathBuf::from("/cache/codeql/2.0.0"),
                    checksum: "a".repeat(64),
                },
            },
            "2.0.0",
            Vec::new(),
        );

        let error = adapter
            .extract(&ExtractionRequest {
                workspace_root: PathBuf::from("/workspace"),
                analysis_targets: vec![FilePath::from(".")],
            })
            .unwrap_err();

        match error {
            CodeQlAdapterError::CommandFailed {
                stage,
                language,
                exit_code,
                stderr,
                ..
            } => {
                assert_eq!(stage, "database create");
                assert_eq!(language, "typescript");
                assert_eq!(exit_code, 1);
                assert_eq!(stderr, "database create failed");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn codeql_adapter_returns_error_on_process_spawn_failure() {
        let mut file_system = InMemoryFileSystem::new();
        file_system.insert("/workspace/web/app.tsx", "export const App = () => null;\n");
        let command_runner = MockCommandRunner::new();
        let expected_program = codeql_executable_path(Path::new("/cache/codeql/2.0.0"));
        command_runner
            .push_result(Err(ProcessError::Io {
                program: expected_program.to_string_lossy().into_owned(),
                cwd: PathBuf::from("/workspace"),
                source: io::Error::new(io::ErrorKind::NotFound, "missing codeql"),
            }))
            .unwrap();
        let adapter = CodeQlAdapter::new(
            file_system,
            command_runner,
            MockToolCachePort {
                bundle: ResolvedToolBundle {
                    tool_name: "codeql".to_owned(),
                    version: "2.0.0".to_owned(),
                    cache_path: PathBuf::from("/cache/codeql/2.0.0"),
                    checksum: "a".repeat(64),
                },
            },
            "2.0.0",
            Vec::new(),
        );

        let error = adapter
            .extract(&ExtractionRequest {
                workspace_root: PathBuf::from("/workspace"),
                analysis_targets: vec![FilePath::from(".")],
            })
            .unwrap_err();

        match error {
            CodeQlAdapterError::Process {
                stage,
                language,
                source: ProcessError::Io { program, cwd, .. },
            } => {
                assert_eq!(stage, "resolve languages");
                assert_eq!(language, "typescript");
                assert_eq!(PathBuf::from(program), expected_program);
                assert_eq!(cwd, PathBuf::from("/workspace"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn command_failed_display_includes_next_steps_and_issue_url() {
        let stderr = "query run failed".to_owned();
        let error = CodeQlAdapterError::CommandFailed {
            stage: "query run",
            language: "rust".to_owned(),
            exit_code: 2,
            stderr: stderr.clone(),
            guidance: format_command_guidance(&stderr),
        };

        let display = error.to_string();
        assert!(display.contains("next steps"));
        assert!(display.contains("https://github.com/RK0429/kalos/issues"));
    }

    #[test]
    fn command_failed_display_classifies_directory_not_exist() {
        let stderr = "database path does not exist".to_owned();
        let error = CodeQlAdapterError::CommandFailed {
            stage: "database create",
            language: "python".to_owned(),
            exit_code: 1,
            stderr: stderr.clone(),
            guidance: format_command_guidance(&stderr),
        };

        let display = error.to_string();
        assert!(display.contains("the CodeQL database output directory does not exist"));
    }

    #[test]
    fn classify_codeql_error_detects_missing_query_file() {
        let stderr = "A fatal error occurred: /Users/test/.cache/kalos/codeql/2.25.1/queries/extract-rust.ql does not exist.".to_owned();
        let error = CodeQlAdapterError::CommandFailed {
            stage: "query run",
            language: "rust".to_owned(),
            exit_code: 2,
            stderr: stderr.clone(),
            guidance: format_command_guidance(&stderr),
        };

        let display = error.to_string();
        assert!(display.contains("bundle may be incomplete"));
        assert!(display.contains("rm -rf ~/.cache/kalos/codeql/"));
        assert!(display.contains("kalos check"));
        assert!(display.contains("https://github.com/RK0429/kalos/issues"));
    }

    #[test]
    fn classify_codeql_error_distinguishes_query_file_from_directory_missing() {
        let directory_missing_stderr = "database path does not exist".to_owned();
        let directory_missing_error = CodeQlAdapterError::CommandFailed {
            stage: "database create",
            language: "python".to_owned(),
            exit_code: 1,
            stderr: directory_missing_stderr.clone(),
            guidance: format_command_guidance(&directory_missing_stderr),
        };

        let directory_missing_display = directory_missing_error.to_string();
        assert!(
            directory_missing_display
                .contains("the CodeQL database output directory does not exist")
        );
        assert!(!directory_missing_display.contains("bundle may be incomplete"));

        let query_missing_stderr = "/path/to/extract-python.ql does not exist".to_owned();
        let query_missing_error = CodeQlAdapterError::CommandFailed {
            stage: "query run",
            language: "python".to_owned(),
            exit_code: 1,
            stderr: query_missing_stderr.clone(),
            guidance: format_command_guidance(&query_missing_stderr),
        };

        let query_missing_display = query_missing_error.to_string();
        assert!(query_missing_display.contains("a required CodeQL query file is missing"));
        assert!(query_missing_display.contains("bundle may be incomplete"));
    }

    #[test]
    fn command_failed_display_shows_generic_cause_for_unknown_errors() {
        let stderr = "unexpected extractor failure".to_owned();
        let error = CodeQlAdapterError::CommandFailed {
            stage: "bqrs decode",
            language: "go".to_owned(),
            exit_code: 3,
            stderr: stderr.clone(),
            guidance: format_command_guidance(&stderr),
        };

        let display = error.to_string();
        assert!(display.contains("CodeQL encountered an error during extraction"));
    }

    #[test]
    fn parse_resolved_languages_reads_codeql_json_keys() {
        let languages =
            parse_resolved_languages(br#"{"javascript":["/codeql/javascript"],"rust":[]}"#)
                .unwrap();

        assert_eq!(
            languages,
            BTreeSet::from(["javascript".to_owned(), "rust".to_owned()])
        );
    }

    #[test]
    fn codeql_adapter_fails_preflight_when_required_extractor_is_missing() {
        let mut file_system = InMemoryFileSystem::new();
        file_system.insert("/workspace/src/lib.rs", "fn main() {}\n");
        let command_runner = MockCommandRunner::new();
        push_language_resolution_result(&command_runner, &["go", "javascript", "python"]);
        let adapter = CodeQlAdapter::new(
            file_system,
            command_runner,
            MockToolCachePort {
                bundle: mock_bundle(),
            },
            "2.0.0",
            Vec::new(),
        );

        let error = adapter
            .extract(&ExtractionRequest {
                workspace_root: PathBuf::from("/workspace"),
                analysis_targets: vec![FilePath::from(".")],
            })
            .unwrap_err();

        match error {
            CodeQlAdapterError::ExtractorUnavailable {
                language,
                available_languages,
            } => {
                assert_eq!(language, "rust");
                assert_eq!(
                    available_languages,
                    format_available_languages(&BTreeSet::from([
                        "go".to_owned(),
                        "javascript".to_owned(),
                        "python".to_owned()
                    ]))
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    fn load_fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codeql")
            .join(name);
        fs::read_to_string(path).unwrap()
    }

    fn mixed_language_file_system(rust_count: usize, python_count: usize) -> InMemoryFileSystem {
        let mut file_system = InMemoryFileSystem::new();
        for index in 0..rust_count {
            file_system.insert(
                &format!("/workspace/src/module_{index}.rs"),
                "pub fn placeholder() -> i32 { 1 }\n",
            );
        }
        for index in 0..python_count {
            file_system.insert(
                &format!("/workspace/scripts/task_{index}.py"),
                "def task():\n    return 1\n",
            );
        }
        file_system
    }

    fn source_files_from_counts(
        rust_count: usize,
        python_count: usize,
    ) -> BTreeMap<FilePath, SourceFile> {
        let mut source_files = BTreeMap::new();
        for index in 0..rust_count {
            let path = FilePath::from(format!("src/module_{index}.rs"));
            source_files.insert(
                path.clone(),
                SourceFile {
                    path,
                    language: Language::Rust,
                },
            );
        }
        for index in 0..python_count {
            let path = FilePath::from(format!("scripts/task_{index}.py"));
            source_files.insert(
                path.clone(),
                SourceFile {
                    path,
                    language: Language::Python,
                },
            );
        }
        source_files
    }

    fn single_source_file(path: &str, language: Language) -> BTreeMap<FilePath, SourceFile> {
        let path = FilePath::from(path);
        BTreeMap::from([(path.clone(), SourceFile { path, language })])
    }

    fn push_successful_language_results(command_runner: &MockCommandRunner, language_count: usize) {
        for _ in 0..language_count {
            push_language_resolution_result(
                command_runner,
                &["go", "javascript", "python", "rust"],
            );
            command_runner
                .push_result(Ok(ProcessOutput {
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    exit_code: 0,
                }))
                .unwrap();
            command_runner
                .push_result(Ok(ProcessOutput {
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    exit_code: 0,
                }))
                .unwrap();
            command_runner
                .push_result(Ok(ProcessOutput {
                    stdout: b"{}".to_vec(),
                    stderr: Vec::new(),
                    exit_code: 0,
                }))
                .unwrap();
        }
    }

    fn push_language_resolution_result(command_runner: &MockCommandRunner, languages: &[&str]) {
        let body = languages
            .iter()
            .map(|language| format!(r#""{language}":["/cache/codeql/2.0.0/{language}"]"#))
            .collect::<Vec<_>>()
            .join(",");
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: format!("{{{body}}}").into_bytes(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();
    }

    fn mock_bundle() -> ResolvedToolBundle {
        ResolvedToolBundle {
            tool_name: "codeql".to_owned(),
            version: "2.0.0".to_owned(),
            cache_path: PathBuf::from("/cache/codeql/2.0.0"),
            checksum: "a".repeat(64),
        }
    }
}
