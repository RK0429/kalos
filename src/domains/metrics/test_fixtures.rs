use std::collections::BTreeMap;

use crate::domains::cpg::{
    CpgEdge, CpgNode, CpgSubgraph, EdgeKind, NodeId, NodeKind, SourceLocation,
};
use crate::domains::{FilePath, ScopeId};

#[derive(Clone, Debug)]
pub struct CpgBuilder {
    aliases: BTreeMap<String, NodeId>,
    nodes: Vec<CpgNode>,
    edges: Vec<CpgEdge>,
    next_node_id: u64,
    default_file_path: FilePath,
}

impl Default for CpgBuilder {
    fn default() -> Self {
        Self {
            aliases: BTreeMap::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
            next_node_id: 1,
            default_file_path: FilePath::from("src/lib.rs"),
        }
    }
}

impl CpgBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn function(self, alias: &str, name: &str) -> Self {
        let file_path = self.default_file_path.clone();
        self.node(alias, NodeKind::Function, name, file_path, None, None)
    }

    pub fn function_at(
        self,
        alias: &str,
        name: &str,
        file_path: &str,
        start_line: u32,
        end_line: u32,
    ) -> Self {
        self.node(
            alias,
            NodeKind::Function,
            name,
            FilePath::from(file_path),
            Some(start_line),
            Some(end_line),
        )
    }

    pub fn variable(self, alias: &str, name: &str) -> Self {
        let file_path = self.default_file_path.clone();
        self.node(alias, NodeKind::Variable, name, file_path, None, None)
    }

    pub fn parameter(self, alias: &str, name: &str) -> Self {
        let file_path = self.default_file_path.clone();
        self.node(alias, NodeKind::Parameter, name, file_path, None, None)
    }

    pub fn module(self, alias: &str, name: &str) -> Self {
        let file_path = self.default_file_path.clone();
        self.node(alias, NodeKind::Module, name, file_path, None, None)
    }

    pub fn module_at(
        self,
        alias: &str,
        name: &str,
        file_path: &str,
        start_line: u32,
        end_line: u32,
    ) -> Self {
        self.node(
            alias,
            NodeKind::Module,
            name,
            FilePath::from(file_path),
            Some(start_line),
            Some(end_line),
        )
    }

    pub fn edge(mut self, source_alias: &str, target_alias: &str, kind: EdgeKind) -> Self {
        let source = self
            .aliases
            .get(source_alias)
            .copied()
            .unwrap_or_else(|| panic!("unknown source alias `{source_alias}`"));
        let target = self
            .aliases
            .get(target_alias)
            .copied()
            .unwrap_or_else(|| panic!("unknown target alias `{target_alias}`"));

        self.edges.push(CpgEdge {
            source,
            target,
            kind,
            extension: None,
        });
        self
    }

    pub fn build(self, scope_id: ScopeId) -> CpgSubgraph {
        CpgSubgraph {
            scope_id,
            nodes: self.nodes,
            edges: self.edges,
        }
    }

    fn node(
        mut self,
        alias: &str,
        kind: NodeKind,
        name: &str,
        file_path: FilePath,
        start_line: Option<u32>,
        end_line: Option<u32>,
    ) -> Self {
        assert!(
            !self.aliases.contains_key(alias),
            "duplicate node alias `{alias}`"
        );

        let node_id = NodeId::new(self.next_node_id);
        self.next_node_id += 1;

        let resolved_start = start_line.unwrap_or(node_id.0 as u32);
        let resolved_end = end_line.unwrap_or(resolved_start);

        self.aliases.insert(alias.to_owned(), node_id);
        self.nodes.push(CpgNode {
            id: node_id,
            kind,
            name: name.to_owned(),
            location: SourceLocation {
                file_path,
                start_line: resolved_start,
                end_line: resolved_end,
            },
            extension: None,
        });
        self
    }
}
