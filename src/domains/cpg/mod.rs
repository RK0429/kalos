use std::collections::BTreeMap;
use std::fmt;

use super::{FilePath, RuleId, ScopeId};

macro_rules! string_newtype {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_newtype!(CpgId);

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u64);

impl NodeId {
    pub fn new(value: u64) -> Self {
        Self(value)
    }
}

impl From<u64> for NodeId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceAnalysis {
    pub cpg: UnifiedCpg,
    pub source_files: BTreeMap<FilePath, SourceFile>,
    pub suppressions: Vec<SuppressionComment>,
    pub warnings: Vec<AnalysisWarning>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnifiedCpg {
    pub id: CpgId,
    pub nodes: Vec<CpgNode>,
    pub edges: Vec<CpgEdge>,
}

impl UnifiedCpg {
    pub fn subgraph(&self, scope_id: &ScopeId) -> CpgSubgraph {
        CpgSubgraph {
            scope_id: scope_id.clone(),
            nodes: self.nodes.clone(),
            edges: self.edges.clone(),
        }
    }

    pub fn functions(&self) -> Vec<&CpgNode> {
        self.nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Function)
            .collect()
    }

    pub fn modules(&self) -> Vec<&CpgNode> {
        self.nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Module)
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpgSubgraph {
    pub scope_id: ScopeId,
    pub nodes: Vec<CpgNode>,
    pub edges: Vec<CpgEdge>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpgNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub name: String,
    pub location: SourceLocation,
    pub extension: Option<LanguageExtension>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpgEdge {
    pub source: NodeId,
    pub target: NodeId,
    pub kind: EdgeKind,
    pub extension: Option<LanguageExtension>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    pub file_path: FilePath,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageExtension {
    pub language: Language,
    pub properties: BTreeMap<String, String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeKind {
    Function,
    Class,
    Module,
    Variable,
    Parameter,
    ExternalSymbol,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeKind {
    Call,
    DataFlow,
    ControlFlow,
    Contains,
    TypeReference,
    Semantic,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Language {
    Python,
    TypeScript,
    Rust,
    Go,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFile {
    pub path: FilePath,
    pub language: Language,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuppressionComment {
    pub location: SourceLocation,
    pub rule_id: Option<RuleId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisWarning {
    pub file_path: FilePath,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::{CpgEdge, CpgId, CpgNode, EdgeKind, NodeId, NodeKind, SourceLocation, UnifiedCpg};
    use crate::domains::{AnalysisLevel, FilePath, ScopeId};

    #[test]
    fn unified_cpg_filters_functions_and_modules() {
        let location = SourceLocation {
            file_path: FilePath::from("src/lib.rs"),
            start_line: 1,
            end_line: 3,
        };
        let graph = UnifiedCpg {
            id: CpgId::from("graph"),
            nodes: vec![
                CpgNode {
                    id: NodeId::from(1),
                    kind: NodeKind::Function,
                    name: "f".to_owned(),
                    location: location.clone(),
                    extension: None,
                },
                CpgNode {
                    id: NodeId::from(2),
                    kind: NodeKind::Module,
                    name: "m".to_owned(),
                    location: location.clone(),
                    extension: None,
                },
                CpgNode {
                    id: NodeId::from(3),
                    kind: NodeKind::Class,
                    name: "C".to_owned(),
                    location,
                    extension: None,
                },
            ],
            edges: vec![CpgEdge {
                source: NodeId::from(1),
                target: NodeId::from(2),
                kind: EdgeKind::Contains,
                extension: None,
            }],
        };

        assert_eq!(graph.functions().len(), 1);
        assert_eq!(graph.modules().len(), 1);
    }

    #[test]
    fn subgraph_stub_preserves_scope_and_content() {
        let location = SourceLocation {
            file_path: FilePath::from("src/lib.rs"),
            start_line: 1,
            end_line: 1,
        };
        let graph = UnifiedCpg {
            id: CpgId::from("graph"),
            nodes: vec![CpgNode {
                id: NodeId::from(1),
                kind: NodeKind::Function,
                name: "f".to_owned(),
                location,
                extension: None,
            }],
            edges: Vec::new(),
        };
        let scope = ScopeId::new(AnalysisLevel::Function, "crate::f", "src/lib.rs");
        let subgraph = graph.subgraph(&scope);

        assert_eq!(subgraph.scope_id, scope);
        assert_eq!(subgraph.nodes.len(), 1);
        assert!(subgraph.edges.is_empty());
    }
}
