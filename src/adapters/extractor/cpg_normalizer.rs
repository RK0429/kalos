use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value};
use thiserror::Error;

use crate::domains::FilePath;
use crate::domains::cpg::{
    AnalysisWarning, CpgEdge, CpgId, CpgNode, EdgeKind, Language, LanguageExtension, NodeId,
    NodeKind, SourceAnalysis, SourceFile, SourceLocation, UnifiedCpg,
};
use crate::platform::fs::path_to_forward_slashes;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct CodeQlQueryOutput {
    #[serde(default)]
    pub functions: Vec<FixtureNode>,
    #[serde(default)]
    pub classes: Vec<FixtureNode>,
    #[serde(default)]
    pub modules: Vec<FixtureNode>,
    #[serde(default)]
    pub variables: Vec<FixtureNode>,
    #[serde(default)]
    pub parameters: Vec<FixtureNode>,
    #[serde(default)]
    pub external_symbols: Vec<FixtureNode>,
    #[serde(default)]
    pub calls: Vec<FixtureEdge>,
    #[serde(default)]
    pub data_flows: Vec<FixtureEdge>,
    #[serde(default)]
    pub contains: Vec<FixtureEdge>,
    #[serde(default)]
    pub type_references: Vec<FixtureEdge>,
    #[serde(default)]
    pub semantic_edges: Vec<FixtureEdge>,
    #[serde(default)]
    pub warnings: Vec<FixtureWarning>,
}

impl CodeQlQueryOutput {
    pub fn extend_from(&mut self, other: Self) {
        self.functions.extend(other.functions);
        self.classes.extend(other.classes);
        self.modules.extend(other.modules);
        self.variables.extend(other.variables);
        self.parameters.extend(other.parameters);
        self.external_symbols.extend(other.external_symbols);
        self.calls.extend(other.calls);
        self.data_flows.extend(other.data_flows);
        self.contains.extend(other.contains);
        self.type_references.extend(other.type_references);
        self.semantic_edges.extend(other.semantic_edges);
        self.warnings.extend(other.warnings);
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct FixtureNode {
    pub id: String,
    pub name: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    #[serde(default)]
    pub language: Option<FixtureLanguage>,
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FixtureEdge {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub language: Option<FixtureLanguage>,
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FixtureWarning {
    pub file: String,
    pub message: String,
}

#[derive(Copy, Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FixtureLanguage {
    Python,
    Typescript,
    Rust,
    Go,
}

impl FixtureLanguage {
    fn into_domain(self) -> Language {
        match self {
            Self::Python => Language::Python,
            Self::Typescript => Language::TypeScript,
            Self::Rust => Language::Rust,
            Self::Go => Language::Go,
        }
    }
}

#[derive(Debug, Error)]
pub enum NormalizationError {
    #[error("failed to parse CodeQL output JSON: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("fixture path `{path}` is outside workspace root `{workspace_root}`")]
    PathOutsideWorkspace {
        path: PathBuf,
        workspace_root: PathBuf,
    },
}

#[derive(Clone, Debug, Default)]
pub struct CpgNormalizer;

impl CpgNormalizer {
    pub fn parse_output(bytes: &[u8]) -> Result<CodeQlQueryOutput, NormalizationError> {
        let value: Value = serde_json::from_slice(bytes)?;
        let normalized = if is_bqrs_output(&value) {
            transform_bqrs_output(value)?
        } else {
            value
        };
        Ok(serde_json::from_value(normalized)?)
    }

    pub fn normalize_fixture_bytes(
        &self,
        workspace_root: &Path,
        source_files: BTreeMap<FilePath, SourceFile>,
        bytes: &[u8],
    ) -> Result<SourceAnalysis, NormalizationError> {
        let output = Self::parse_output(bytes)?;
        self.normalize(workspace_root, source_files, output)
    }

    pub fn normalize(
        &self,
        workspace_root: &Path,
        source_files: BTreeMap<FilePath, SourceFile>,
        output: CodeQlQueryOutput,
    ) -> Result<SourceAnalysis, NormalizationError> {
        let mut raw_nodes = Vec::new();
        raw_nodes.extend(self.build_nodes(
            workspace_root,
            &source_files,
            output.modules,
            NodeKind::Module,
        )?);
        raw_nodes.extend(self.build_nodes(
            workspace_root,
            &source_files,
            output.classes,
            NodeKind::Class,
        )?);
        raw_nodes.extend(self.build_nodes(
            workspace_root,
            &source_files,
            output.functions,
            NodeKind::Function,
        )?);
        raw_nodes.extend(self.build_nodes(
            workspace_root,
            &source_files,
            output.variables,
            NodeKind::Variable,
        )?);
        raw_nodes.extend(self.build_nodes(
            workspace_root,
            &source_files,
            output.parameters,
            NodeKind::Parameter,
        )?);
        raw_nodes.extend(self.build_nodes(
            workspace_root,
            &source_files,
            output.external_symbols,
            NodeKind::ExternalSymbol,
        )?);

        raw_nodes.sort_by(|left, right| {
            (
                left.location.file_path.clone(),
                node_kind_rank(left.kind),
                left.location.start_line,
                left.location.end_line,
                left.name.clone(),
                left.source_id.clone(),
            )
                .cmp(&(
                    right.location.file_path.clone(),
                    node_kind_rank(right.kind),
                    right.location.start_line,
                    right.location.end_line,
                    right.name.clone(),
                    right.source_id.clone(),
                ))
        });

        let mut node_lookup = HashMap::new();
        let nodes = raw_nodes
            .into_iter()
            .enumerate()
            .map(|(index, node)| {
                let node_id = NodeId::new((index + 1) as u64);
                node_lookup.insert(node.source_id, node_id);
                CpgNode {
                    id: node_id,
                    kind: node.kind,
                    name: node.name,
                    location: node.location,
                    extension: node.extension,
                }
            })
            .collect::<Vec<_>>();

        let mut warnings = output
            .warnings
            .into_iter()
            .map(|warning| {
                Ok(AnalysisWarning {
                    file_path: FilePath::from(normalize_fixture_path(
                        workspace_root,
                        &warning.file,
                    )?),
                    message: warning.message,
                    user_facing: false,
                })
            })
            .collect::<Result<Vec<_>, NormalizationError>>()?;
        let mut edges = Vec::new();

        append_edges(
            &mut edges,
            &mut warnings,
            &node_lookup,
            output.calls,
            EdgeKind::Call,
        );
        append_edges(
            &mut edges,
            &mut warnings,
            &node_lookup,
            output.data_flows,
            EdgeKind::DataFlow,
        );
        append_edges(
            &mut edges,
            &mut warnings,
            &node_lookup,
            output.contains,
            EdgeKind::Contains,
        );
        append_edges(
            &mut edges,
            &mut warnings,
            &node_lookup,
            output.type_references,
            EdgeKind::TypeReference,
        );
        append_edges(
            &mut edges,
            &mut warnings,
            &node_lookup,
            output.semantic_edges,
            EdgeKind::Semantic,
        );

        edges.sort_by_key(|edge| (edge.source, edge.target, edge.kind));
        warnings.sort_by(|left, right| {
            (left.file_path.clone(), left.message.clone())
                .cmp(&(right.file_path.clone(), right.message.clone()))
        });

        Ok(SourceAnalysis {
            cpg: UnifiedCpg {
                id: CpgId::from("codeql"),
                nodes,
                edges,
            },
            source_files,
            suppressions: Vec::new(),
            warnings,
        })
    }

    fn build_nodes(
        &self,
        workspace_root: &Path,
        source_files: &BTreeMap<FilePath, SourceFile>,
        fixture_nodes: Vec<FixtureNode>,
        kind: NodeKind,
    ) -> Result<Vec<RawNode>, NormalizationError> {
        let mut raw_nodes = Vec::new();

        for node in fixture_nodes {
            let file_path = FilePath::from(normalize_fixture_path(workspace_root, &node.file)?);
            if !source_files.contains_key(&file_path) {
                continue;
            }

            let language = node
                .language
                .map(FixtureLanguage::into_domain)
                .or_else(|| source_files.get(&file_path).map(|file| file.language))
                .or_else(|| infer_language_from_path(file_path.as_str()));
            let extension = language.map(|language| LanguageExtension {
                language,
                properties: node.properties.clone(),
            });

            raw_nodes.push(RawNode {
                source_id: node.id,
                kind,
                name: node.name,
                location: SourceLocation {
                    file_path,
                    start_line: node.start_line,
                    end_line: node.end_line,
                },
                extension,
            });
        }

        Ok(raw_nodes)
    }
}

#[derive(Debug, Error)]
enum BqrsTransformError {
    #[error(transparent)]
    Parse(#[from] serde_json::Error),
    #[error(
        "CodeQL BQRS output did not contain any named predicates; queries must use `query predicate` instead of `select`"
    )]
    MissingNamedPredicates,
}

impl From<BqrsTransformError> for NormalizationError {
    fn from(error: BqrsTransformError) -> Self {
        match error {
            BqrsTransformError::Parse(error) => Self::Parse(error),
            BqrsTransformError::MissingNamedPredicates => Self::Parse(serde_json::Error::io(
                std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()),
            )),
        }
    }
}

#[derive(Debug, Deserialize)]
struct BqrsPredicate {
    columns: Vec<BqrsColumn>,
    tuples: Vec<Vec<Value>>,
}

#[derive(Debug, Deserialize)]
struct BqrsColumn {
    name: String,
}

fn is_bqrs_output(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        object.values().any(|entry| {
            entry
                .as_object()
                .is_some_and(|entry| entry.contains_key("columns") && entry.contains_key("tuples"))
        })
    })
}

fn transform_bqrs_output(value: Value) -> Result<Value, BqrsTransformError> {
    let Value::Object(object) = value else {
        return Ok(value);
    };

    let mut transformed = Map::new();
    for (key, value) in object {
        if key == "#select" {
            continue;
        }
        if value
            .as_object()
            .is_some_and(|entry| entry.contains_key("columns") && entry.contains_key("tuples"))
        {
            transformed.insert(key, transform_bqrs_predicate(value)?);
        } else {
            transformed.insert(key, value);
        }
    }

    if !transformed.keys().any(|key| is_known_output_field(key)) {
        return Err(BqrsTransformError::MissingNamedPredicates);
    }

    Ok(Value::Object(transformed))
}

fn transform_bqrs_predicate(value: Value) -> Result<Value, BqrsTransformError> {
    let predicate: BqrsPredicate = serde_json::from_value(value)?;
    let rows = predicate
        .tuples
        .into_iter()
        .map(|tuple| {
            let mut row = Map::new();
            for (column, value) in predicate.columns.iter().zip(tuple.into_iter()) {
                row.insert(column.name.clone(), value);
            }
            Value::Object(row)
        })
        .collect();
    Ok(Value::Array(rows))
}

fn is_known_output_field(key: &str) -> bool {
    matches!(
        key,
        "functions"
            | "classes"
            | "modules"
            | "variables"
            | "parameters"
            | "external_symbols"
            | "calls"
            | "data_flows"
            | "contains"
            | "type_references"
            | "semantic_edges"
            | "warnings"
    )
}

#[derive(Debug)]
struct RawNode {
    source_id: String,
    kind: NodeKind,
    name: String,
    location: SourceLocation,
    extension: Option<LanguageExtension>,
}

fn append_edges(
    edges: &mut Vec<CpgEdge>,
    warnings: &mut Vec<AnalysisWarning>,
    node_lookup: &HashMap<String, NodeId>,
    fixture_edges: Vec<FixtureEdge>,
    kind: EdgeKind,
) {
    for edge in fixture_edges {
        let Some(source) = node_lookup.get(&edge.source).copied() else {
            warnings.push(AnalysisWarning {
                file_path: FilePath::from("."),
                message: format!(
                    "CodeQL output referenced unknown source node `{}`",
                    edge.source
                ),
                user_facing: false,
            });
            continue;
        };
        let Some(target) = node_lookup.get(&edge.target).copied() else {
            warnings.push(AnalysisWarning {
                file_path: FilePath::from("."),
                message: format!(
                    "CodeQL output referenced unknown target node `{}`",
                    edge.target
                ),
                user_facing: false,
            });
            continue;
        };
        let extension = edge.language.map(|language| LanguageExtension {
            language: language.into_domain(),
            properties: edge.properties,
        });

        edges.push(CpgEdge {
            source,
            target,
            kind,
            extension,
        });
    }
}

fn normalize_fixture_path(workspace_root: &Path, path: &str) -> Result<String, NormalizationError> {
    let path = Path::new(path);
    let normalized_path = normalize_path(path);
    let relative = if normalized_path.is_absolute() {
        normalized_path
            .strip_prefix(workspace_root)
            .map_err(|_| NormalizationError::PathOutsideWorkspace {
                path: normalized_path.clone(),
                workspace_root: workspace_root.to_path_buf(),
            })?
            .to_path_buf()
    } else {
        normalized_path
    };

    Ok(path_to_forward_slashes(&relative))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}
fn infer_language_from_path(path: &str) -> Option<Language> {
    match Path::new(path).extension()?.to_str()? {
        "py" => Some(Language::Python),
        "ts" => Some(Language::TypeScript),
        "tsx" => Some(Language::TypeScript),
        "rs" => Some(Language::Rust),
        "go" => Some(Language::Go),
        _ => None,
    }
}

fn node_kind_rank(kind: NodeKind) -> u8 {
    match kind {
        NodeKind::Module => 0,
        NodeKind::Class => 1,
        NodeKind::Function => 2,
        NodeKind::Variable => 3,
        NodeKind::Parameter => 4,
        NodeKind::ExternalSymbol => 5,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use super::{
        CodeQlQueryOutput, CpgNormalizer, FixtureLanguage, FixtureNode, NormalizationError,
    };
    use crate::domains::FilePath;
    use crate::domains::cpg::{EdgeKind, Language, NodeKind, SourceFile};

    #[test]
    fn normalizes_fixture_output_into_unified_cpg() {
        let workspace_root = std::path::Path::new("/workspace");
        let source_files = BTreeMap::from([(
            FilePath::from("src/app.py"),
            SourceFile {
                path: FilePath::from("src/app.py"),
                language: Language::Python,
            },
        )]);
        let fixture = load_fixture("python.json");

        let analysis = CpgNormalizer
            .normalize_fixture_bytes(workspace_root, source_files.clone(), fixture.as_bytes())
            .unwrap();

        assert_eq!(analysis.source_files, source_files);
        assert_eq!(analysis.cpg.nodes.len(), 3);
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
    }

    #[test]
    fn normalizes_python_module_nodes_with_file_path_names() {
        let workspace_root = std::path::Path::new("/workspace");
        let source_files = BTreeMap::from([(
            FilePath::from("src/app.py"),
            SourceFile {
                path: FilePath::from("src/app.py"),
                language: Language::Python,
            },
        )]);
        let fixture = r#"{
            "modules": [
                {
                    "id":"mod_src/app.py",
                    "name":"src/app.py",
                    "file":"src/app.py",
                    "start_line":1,
                    "end_line":1,
                    "language":"python"
                }
            ],
            "functions": [
                {
                    "id":"fn_src/app.py:6:greet",
                    "name":"greet",
                    "file":"src/app.py",
                    "start_line":6,
                    "end_line":8,
                    "language":"python"
                }
            ],
            "contains": [
                {
                    "source":"mod_src/app.py",
                    "target":"fn_src/app.py:6:greet",
                    "language":"python"
                }
            ]
        }"#;

        let analysis = CpgNormalizer
            .normalize_fixture_bytes(workspace_root, source_files, fixture.as_bytes())
            .unwrap();

        assert!(analysis.cpg.nodes.iter().any(|node| {
            node.kind == NodeKind::Module
                && node.name == "src/app.py"
                && node.location.file_path == FilePath::from("src/app.py")
                && node.location.start_line == 1
                && node.location.end_line == 1
        }));
    }

    #[test]
    fn normalizes_python_package_modules_into_module_nodes() {
        let workspace_root = std::path::Path::new("/workspace");
        let source_files = BTreeMap::from([(
            FilePath::from("pkg/app.py"),
            SourceFile {
                path: FilePath::from("pkg/app.py"),
                language: Language::Python,
            },
        )]);
        let fixture = r#"{
            "modules": [
                {
                    "id":"mod_pkg/app.py",
                    "name":"pkg/app.py",
                    "file":"pkg/app.py",
                    "start_line":1,
                    "end_line":1,
                    "language":"python"
                }
            ]
        }"#;

        let analysis = CpgNormalizer
            .normalize_fixture_bytes(workspace_root, source_files, fixture.as_bytes())
            .unwrap();

        assert!(analysis.cpg.nodes.iter().any(|node| {
            node.kind == NodeKind::Module
                && node.name == "pkg/app.py"
                && node.location.file_path == FilePath::from("pkg/app.py")
                && node.location.start_line == 1
                && node.location.end_line == 1
        }));
    }

    #[test]
    fn normalizes_workspace_relative_paths_from_absolute_fixture_paths() {
        let workspace_root = fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap();
        let fixture = format!(
            r#"{{
                "modules": [{{"id":"m1","name":"crate","file":"{}/tests/fixtures/codeql/tmp.rs","start_line":1,"end_line":1,"language":"rust"}}]
            }}"#,
            workspace_root.display()
        );
        let source_files = BTreeMap::from([(
            FilePath::from("tests/fixtures/codeql/tmp.rs"),
            SourceFile {
                path: FilePath::from("tests/fixtures/codeql/tmp.rs"),
                language: Language::Rust,
            },
        )]);

        let analysis = CpgNormalizer
            .normalize_fixture_bytes(&workspace_root, source_files, fixture.as_bytes())
            .unwrap();

        assert_eq!(
            analysis.cpg.nodes[0].location.file_path,
            FilePath::from("tests/fixtures/codeql/tmp.rs")
        );
    }

    #[test]
    fn normalize_filters_out_nodes_for_files_missing_from_source_files() {
        let workspace_root = std::path::Path::new("/workspace");
        let source_files = BTreeMap::from([(
            FilePath::from("src/main.rs"),
            SourceFile {
                path: FilePath::from("src/main.rs"),
                language: Language::Rust,
            },
        )]);
        let output = CodeQlQueryOutput {
            functions: vec![
                FixtureNode {
                    id: "fn_src/main.rs:1:main".to_string(),
                    name: "main".to_string(),
                    file: "src/main.rs".to_string(),
                    start_line: 1,
                    end_line: 3,
                    language: Some(FixtureLanguage::Rust),
                    properties: BTreeMap::new(),
                },
                FixtureNode {
                    id: "fn_tmp/scratch.rs:1:scratch".to_string(),
                    name: "scratch".to_string(),
                    file: "tmp/scratch.rs".to_string(),
                    start_line: 1,
                    end_line: 1,
                    language: Some(FixtureLanguage::Rust),
                    properties: BTreeMap::new(),
                },
            ],
            ..CodeQlQueryOutput::default()
        };

        let analysis = CpgNormalizer
            .normalize(workspace_root, source_files, output)
            .unwrap();

        assert_eq!(analysis.cpg.nodes.len(), 1);
        assert!(
            analysis
                .cpg
                .nodes
                .iter()
                .all(|node| node.location.file_path == FilePath::from("src/main.rs"))
        );
    }

    #[test]
    fn parses_all_language_fixtures() {
        let workspace_root = std::path::Path::new("/workspace");
        let cases = [
            ("python.json", "src/app.py", Language::Python),
            ("typescript.json", "web/app.ts", Language::TypeScript),
            ("rust.json", "src/lib.rs", Language::Rust),
            ("go.json", "cmd/app/main.go", Language::Go),
        ];

        for (fixture_name, path, language) in cases {
            let source_files = BTreeMap::from([(
                FilePath::from(path),
                SourceFile {
                    path: FilePath::from(path),
                    language,
                },
            )]);
            let analysis = CpgNormalizer
                .normalize_fixture_bytes(
                    workspace_root,
                    source_files,
                    load_fixture(fixture_name).as_bytes(),
                )
                .unwrap();
            assert!(
                !analysis.cpg.nodes.is_empty(),
                "{fixture_name} should produce nodes"
            );
        }
    }

    #[test]
    fn parse_output_transforms_bqrs_tabular_json() {
        let output = CpgNormalizer::parse_output(
            br##"{
                "functions": {
                    "columns": [
                        {"name": "id", "kind": "String"},
                        {"name": "name", "kind": "String"},
                        {"name": "file", "kind": "String"},
                        {"name": "start_line", "kind": "Int"},
                        {"name": "end_line", "kind": "Int"}
                    ],
                    "tuples": [["fn_src/app.py:3:main", "main", "src/app.py", 3, 7]]
                },
                "modules": {
                    "columns": [
                        {"name": "id", "kind": "String"},
                        {"name": "name", "kind": "String"},
                        {"name": "file", "kind": "String"},
                        {"name": "start_line", "kind": "Int"},
                        {"name": "end_line", "kind": "Int"}
                    ],
                    "tuples": [["mod_src/app.py", "src/app.py", "src/app.py", 1, 1]]
                },
                "contains": {
                    "columns": [
                        {"name": "source", "kind": "String"},
                        {"name": "target", "kind": "String"}
                    ],
                    "tuples": [["mod_src/app.py", "fn_src/app.py:3:main"]]
                },
                "#select": {
                    "columns": [{"name": "result", "kind": "Int"}],
                    "tuples": [[1]]
                }
            }"##,
        )
        .unwrap();

        assert_eq!(output.modules.len(), 1);
        assert_eq!(output.modules[0].id, "mod_src/app.py");
        assert_eq!(output.modules[0].name, "src/app.py");
        assert_eq!(output.functions.len(), 1);
        assert_eq!(output.functions[0].name, "main");
        assert_eq!(output.contains.len(), 1);
        assert_eq!(output.contains[0].source, "mod_src/app.py");
    }

    #[test]
    fn parse_output_rejects_select_only_bqrs_output() {
        let error = CpgNormalizer::parse_output(
            br##"{
                "#select": {
                    "columns": [{"name": "result", "kind": "Int"}],
                    "tuples": [[1]]
                }
            }"##,
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("queries must use `query predicate` instead of `select`"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn normalize_rejects_path_outside_workspace() {
        let workspace_root = std::path::Path::new("/workspace");
        let error = CpgNormalizer
            .normalize_fixture_bytes(
                workspace_root,
                BTreeMap::new(),
                br#"{
                "modules": [{
                    "id":"m1",
                    "name":"evil",
                    "file":"/other/project/evil.py",
                    "start_line":1,
                    "end_line":1,
                    "language":"python"
                }]
            }"#,
            )
            .unwrap_err();

        match error {
            NormalizationError::PathOutsideWorkspace {
                path,
                workspace_root,
            } => {
                assert_eq!(path, std::path::PathBuf::from("/other/project/evil.py"));
                assert_eq!(workspace_root, std::path::PathBuf::from("/workspace"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn filters_out_nodes_from_files_not_in_source_files() {
        let workspace_root = std::path::Path::new("/workspace");
        let source_files = BTreeMap::from([(
            FilePath::from("src/lib.rs"),
            SourceFile {
                path: FilePath::from("src/lib.rs"),
                language: Language::Rust,
            },
        )]);
        let fixture = r#"{
            "functions": [
                {"id":"fn1","name":"real_fn","file":"src/lib.rs","start_line":1,"end_line":5,"language":"rust"},
                {"id":"fn2","name":"generated_fn","file":"target/debug/build/out/generated.rs","start_line":1,"end_line":10,"language":"rust"}
            ],
            "modules": [
                {"id":"mod1","name":"lib","file":"src/lib.rs","start_line":1,"end_line":10,"language":"rust"},
                {"id":"mod2","name":"generated","file":"target/debug/build/out/generated.rs","start_line":1,"end_line":20,"language":"rust"}
            ]
        }"#;

        let analysis = CpgNormalizer
            .normalize_fixture_bytes(workspace_root, source_files, fixture.as_bytes())
            .unwrap();

        assert_eq!(analysis.cpg.nodes.len(), 2);
        assert!(
            analysis
                .cpg
                .nodes
                .iter()
                .all(|node| node.location.file_path == FilePath::from("src/lib.rs"))
        );
    }

    fn load_fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codeql")
            .join(name);
        fs::read_to_string(path).unwrap()
    }
}
