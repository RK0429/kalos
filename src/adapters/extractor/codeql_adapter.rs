use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::cpg_normalizer::{CodeQlQueryOutput, CpgNormalizer, NormalizationError};
use super::file_collector::FileCollector;
use crate::domains::cpg::{Language, SourceAnalysis, UnifiedCpg};
use crate::platform::fs::FileSystem;
use crate::platform::process::{CommandRunner, ProcessError, ProcessOutput};
use crate::ports::extractor::{ExtractionRequest, ExtractorPort};
use crate::ports::tool_cache::{ToolCachePort, ToolCacheRequest};

const DEFAULT_EXTENSIONS: [&str; 5] = [".py", ".ts", ".tsx", ".rs", ".go"];

#[derive(Clone, Debug)]
pub struct CodeQlAdapter<F, R, T> {
    file_system: F,
    command_runner: R,
    tool_cache: T,
    bundle_version: String,
    exclude_patterns: Vec<String>,
    normalizer: CpgNormalizer,
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
        }
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
    #[error("failed to collect source files under `{workspace_root}`: {source}")]
    CollectFiles {
        workspace_root: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to resolve CodeQL bundle: {message}")]
    ResolveBundle { message: String },
    #[error("failed to execute `{stage}` for `{language}`: {source}")]
    Process {
        stage: &'static str,
        language: String,
        #[source]
        source: ProcessError,
    },
    #[error("CodeQL `{stage}` failed for `{language}` with exit code {exit_code}: {stderr}")]
    CommandFailed {
        stage: &'static str,
        language: String,
        exit_code: i32,
        stderr: String,
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
                warnings: Vec::new(),
            });
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
        let codeql_program = bundle.cache_path.join("codeql");
        let mut combined_output = CodeQlQueryOutput::default();
        let languages = source_files
            .values()
            .map(|source_file| source_file.language)
            .collect::<BTreeSet<_>>();

        for language in languages {
            let database_path = request
                .workspace_root
                .join(".kalos")
                .join("codeql")
                .join(Self::language_pack(language).replace('/', "-"));
            let query_path = bundle.cache_path.join("queries").join(format!(
                "extract-{}.ql",
                Self::language_pack(language).replace('/', "-")
            ));

            self.run_checked(
                &codeql_program,
                build_database_create_args(&database_path, &request.workspace_root, language),
                &request.workspace_root,
                "database create",
                language,
            )?;

            let query_output = self.run_checked(
                &codeql_program,
                build_query_run_args(&database_path, &query_path),
                &request.workspace_root,
                "query run",
                language,
            )?;
            combined_output.extend_from(CpgNormalizer::parse_output(&query_output.stdout)?);
        }

        Ok(self
            .normalizer
            .normalize(&request.workspace_root, source_files, combined_output)?)
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
            return Err(CodeQlAdapterError::CommandFailed {
                stage,
                language: language_name(language).to_owned(),
                exit_code: output.exit_code,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }

        Ok(output)
    }
}

fn build_database_create_args(
    database_path: &Path,
    workspace_root: &Path,
    language: Language,
) -> Vec<String> {
    vec![
        "database".to_owned(),
        "create".to_owned(),
        database_path.to_string_lossy().into_owned(),
        "--language".to_owned(),
        CodeQlAdapter::<(), (), ()>::language_pack(language).to_owned(),
        "--source-root".to_owned(),
        workspace_root.to_string_lossy().into_owned(),
    ]
}

fn build_query_run_args(database_path: &Path, query_path: &Path) -> Vec<String> {
    vec![
        "query".to_owned(),
        "run".to_owned(),
        query_path.to_string_lossy().into_owned(),
        "--database".to_owned(),
        database_path.to_string_lossy().into_owned(),
        "--format=json".to_owned(),
        "--output=-".to_owned(),
    ]
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
    use std::convert::Infallible;
    use std::fs;
    use std::io;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::{CodeQlAdapter, CodeQlAdapterError};
    use crate::domains::FilePath;
    use crate::domains::cpg::{EdgeKind, Language, NodeKind};
    use crate::platform::fs::InMemoryFileSystem;
    use crate::platform::process::{MockCommandRunner, ProcessError, ProcessOutput};
    use crate::ports::extractor::{ExtractionRequest, ExtractorPort};
    use crate::ports::tool_cache::{ResolvedToolBundle, ToolCachePort, ToolCacheRequest};

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
    fn codeql_adapter_extracts_with_mocks() {
        let mut file_system = InMemoryFileSystem::new();
        file_system.insert("/workspace/src/app.py", "def main():\n    return 1\n");
        let command_runner = MockCommandRunner::new();
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
            vec![NodeKind::Module, NodeKind::Class, NodeKind::Function]
        );
        assert_eq!(
            analysis
                .cpg
                .edges
                .iter()
                .map(|edge| edge.kind)
                .collect::<Vec<_>>(),
            vec![EdgeKind::Contains, EdgeKind::Contains, EdgeKind::Call]
        );

        let invocations = command_runner.invocations().unwrap();
        assert_eq!(invocations.len(), 2);
        assert_eq!(invocations[0].program, "/cache/codeql/2.0.0/codeql");
        assert_eq!(invocations[0].args[0], "database");
        assert_eq!(invocations[0].args[1], "create");
        assert_eq!(invocations[0].args[3], "--language");
        assert_eq!(invocations[0].args[4], "python");
        assert_eq!(invocations[1].args[0], "query");
        assert_eq!(invocations[1].args[1], "run");
        assert!(invocations[1].args.iter().any(|arg| arg == "--format=json"));
        assert!(invocations[1].args.iter().any(|arg| arg == "--output=-"));
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
    fn codeql_adapter_returns_error_on_command_failure() {
        let mut file_system = InMemoryFileSystem::new();
        file_system.insert("/workspace/web/app.tsx", "export const App = () => null;\n");
        let command_runner = MockCommandRunner::new();
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
        command_runner
            .push_result(Err(ProcessError::Io {
                program: "/cache/codeql/2.0.0/codeql".to_owned(),
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
                assert_eq!(stage, "database create");
                assert_eq!(language, "typescript");
                assert_eq!(program, "/cache/codeql/2.0.0/codeql");
                assert_eq!(cwd, PathBuf::from("/workspace"));
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
}
