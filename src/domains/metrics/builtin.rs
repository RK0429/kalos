use std::collections::{BTreeMap, BTreeSet};

use petgraph::algo::kosaraju_scc;
use petgraph::graph::DiGraph;

use crate::domains::cpg::{CpgNode, CpgSubgraph, EdgeKind, NodeId, NodeKind, UnifiedCpg};
use crate::domains::{AnalysisLevel, FilePath, MetricId, RuleId, ScopeId};

use super::types::{
    MetricConfig, MetricDefinition, MetricOrigin, MetricParticipation, MetricValue, round_half_up,
};

/// Minimum number of module-level dependency edges required for M-P003
/// (hub dependency concentration) to produce a meaningful normalized risk.
///
/// Below this support level `max_in_degree / total_in_degree` is dominated by
/// structural inevitability (e.g. a single test → source edge yields 1.0) and
/// cannot be distinguished from a genuinely concentrated hub, so the
/// normalized risk is clamped to 0 to avoid spurious KAL-P003 diagnostics on
/// freshly scaffolded projects. See GitHub issue #59.
pub const MIN_HUB_CONCENTRATION_IN_DEGREE: u32 = 3;

/// Minimum number of module-level fan-in/fan-out dependency edges required for
/// M-M003 (module instability) to produce a meaningful normalized risk.
///
/// Below this support level `fan_out / (fan_in + fan_out)` is dominated by
/// structural inevitability (e.g. a thin leaf adapter with one outward
/// dependency yields 1.0) and cannot be distinguished from genuinely unstable
/// architecture, so the normalized risk is clamped to 0 to avoid spurious
/// KAL-M003 diagnostics on tiny modules. See GitHub issue #116.
pub const MIN_MODULE_INSTABILITY_DEPENDENCY_SUPPORT: u32 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CfgBranchEntropyRisk {
    id: MetricId,
    rule_id: RuleId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CyclomaticComplexityRisk {
    id: MetricId,
    rule_id: RuleId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataFlowDensityRisk {
    id: MetricId,
    rule_id: RuleId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentifierRepetitionRisk {
    id: MetricId,
    rule_id: RuleId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleFanOutRisk {
    id: MetricId,
    rule_id: RuleId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CircularDependencyParticipationRisk {
    id: MetricId,
    rule_id: RuleId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstabilityRisk {
    id: MetricId,
    rule_id: RuleId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CyclicCouplingRisk {
    id: MetricId,
    rule_id: RuleId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleSizeEntropyImbalanceRisk {
    id: MetricId,
    rule_id: RuleId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HubDependencyConcentrationRisk {
    id: MetricId,
    rule_id: RuleId,
}

macro_rules! impl_builtin_metric {
    ($name:ident, $metric_id:literal, $metric_name:literal, $level:expr, $rule_id:literal, $description:literal, $compute_fn:ident) => {
        impl Default for $name {
            fn default() -> Self {
                Self {
                    id: MetricId::from($metric_id),
                    rule_id: RuleId::from($rule_id),
                }
            }
        }

        impl $name {
            pub fn new() -> Self {
                Self::default()
            }
        }

        impl MetricDefinition for $name {
            fn id(&self) -> &MetricId {
                &self.id
            }

            fn name(&self) -> &str {
                $metric_name
            }

            fn level(&self) -> AnalysisLevel {
                $level
            }

            fn origin(&self) -> MetricOrigin {
                MetricOrigin::BuiltIn
            }

            fn participation(&self) -> MetricParticipation {
                MetricParticipation::ScoredAndDiagnosable
            }

            fn rule_binding(&self) -> Option<&RuleId> {
                Some(&self.rule_id)
            }

            fn description(&self) -> &str {
                $description
            }

            fn compute(
                &self,
                subgraph: &CpgSubgraph,
                _config: &MetricConfig,
            ) -> Option<MetricValue> {
                $compute_fn(subgraph, self.id())
            }
        }
    };
}

impl_builtin_metric!(
    CfgBranchEntropyRisk,
    "M-F001",
    "CFG Branch Entropy Risk",
    AnalysisLevel::Function,
    "KAL-F001",
    "Average normalized branching entropy across CFG branch nodes.",
    compute_cfg_branch_entropy_risk
);
impl_builtin_metric!(
    CyclomaticComplexityRisk,
    "M-F002",
    "Cyclomatic Complexity Risk",
    AnalysisLevel::Function,
    "KAL-F002",
    "McCabe cyclomatic complexity normalized to a 0–1 risk scale.",
    compute_cyclomatic_complexity_risk
);
impl_builtin_metric!(
    DataFlowDensityRisk,
    "M-F003",
    "Data Flow Density Risk",
    AnalysisLevel::Function,
    "KAL-F003",
    "Unique variable-to-variable data-flow density within the function.",
    compute_data_flow_density_risk
);
impl_builtin_metric!(
    IdentifierRepetitionRisk,
    "M-F004",
    "Identifier Repetition Risk",
    AnalysisLevel::Function,
    "KAL-F004",
    "Entropy imbalance across local identifier tokens.",
    compute_identifier_repetition_risk
);
impl_builtin_metric!(
    ModuleFanOutRisk,
    "M-M001",
    "Module Fan-Out Risk",
    AnalysisLevel::Module,
    "KAL-M001",
    "Unique outgoing dependencies from the current module.",
    compute_module_fan_out_risk
);
impl_builtin_metric!(
    CircularDependencyParticipationRisk,
    "M-M002",
    "Circular Dependency Participation Risk",
    AnalysisLevel::Module,
    "KAL-M002",
    "Participation of the current module in dependency cycles.",
    compute_circular_dependency_participation_risk
);
impl_builtin_metric!(
    InstabilityRisk,
    "M-M003",
    "Instability Risk",
    AnalysisLevel::Module,
    "KAL-M003",
    "Outgoing dependency ratio for the current module.",
    compute_instability_risk
);
impl_builtin_metric!(
    CyclicCouplingRisk,
    "M-P001",
    "Cyclic Coupling Risk",
    AnalysisLevel::Project,
    "KAL-P001",
    "Fraction of module dependency edges that participate in cycles.",
    compute_cyclic_coupling_risk
);
impl_builtin_metric!(
    ModuleSizeEntropyImbalanceRisk,
    "M-P002",
    "Module Size Entropy Imbalance Risk",
    AnalysisLevel::Project,
    "KAL-P002",
    "Entropy imbalance of module LOC share across the project.",
    compute_module_size_entropy_imbalance_risk
);
impl_builtin_metric!(
    HubDependencyConcentrationRisk,
    "M-P003",
    "Hub Dependency Concentration Risk",
    AnalysisLevel::Project,
    "KAL-P003",
    "Inbound dependency concentration around the largest module hub.",
    compute_hub_dependency_concentration_risk
);

pub fn builtin_metric_definitions() -> Vec<Box<dyn MetricDefinition>> {
    vec![
        Box::new(CfgBranchEntropyRisk::new()),
        Box::new(CyclomaticComplexityRisk::new()),
        Box::new(DataFlowDensityRisk::new()),
        Box::new(IdentifierRepetitionRisk::new()),
        Box::new(ModuleFanOutRisk::new()),
        Box::new(CircularDependencyParticipationRisk::new()),
        Box::new(InstabilityRisk::new()),
        Box::new(CyclicCouplingRisk::new()),
        Box::new(ModuleSizeEntropyImbalanceRisk::new()),
        Box::new(HubDependencyConcentrationRisk::new()),
    ]
}

fn compute_cfg_branch_entropy_risk(
    subgraph: &CpgSubgraph,
    metric_id: &MetricId,
) -> Option<MetricValue> {
    let control_flow_edges = subgraph
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::ControlFlow)
        .collect::<Vec<_>>();
    let mut out_degree = BTreeMap::<NodeId, u32>::new();
    for edge in control_flow_edges {
        *out_degree.entry(edge.source).or_insert(0) += 1;
    }

    let branch_scores = out_degree
        .into_values()
        .filter(|degree| *degree > 1)
        .map(|degree| f64::from(degree).log2() / 4_f64.log2())
        .collect::<Vec<_>>();

    let raw_value = average(&branch_scores);
    finalize_metric_value(metric_id, raw_value, raw_value)
}

fn compute_cyclomatic_complexity_risk(
    subgraph: &CpgSubgraph,
    metric_id: &MetricId,
) -> Option<MetricValue> {
    let control_flow_edges = subgraph
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::ControlFlow)
        .collect::<Vec<_>>();
    let mut cfg_nodes = subgraph
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Function)
        .map(|node| node.id)
        .collect::<BTreeSet<_>>();

    for edge in &control_flow_edges {
        cfg_nodes.insert(edge.source);
        cfg_nodes.insert(edge.target);
    }

    let raw_value = if control_flow_edges.is_empty() {
        1.0
    } else if cfg_nodes.is_empty() {
        0.0
    } else {
        control_flow_edges.len() as f64 - cfg_nodes.len() as f64 + 2.0
    };
    let normalized_risk = ((raw_value - 1.0) / 15.0).clamp(0.0, 1.0);

    finalize_metric_value(metric_id, raw_value, normalized_risk)
}

fn compute_data_flow_density_risk(
    subgraph: &CpgSubgraph,
    metric_id: &MetricId,
) -> Option<MetricValue> {
    let variable_nodes = subgraph
        .nodes
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Variable | NodeKind::Parameter))
        .map(|node| node.id)
        .collect::<BTreeSet<_>>();

    let raw_value = if variable_nodes.len() < 2 {
        0.0
    } else {
        let unique_edges = subgraph
            .edges
            .iter()
            .filter(|edge| {
                edge.kind == EdgeKind::DataFlow
                    && variable_nodes.contains(&edge.source)
                    && variable_nodes.contains(&edge.target)
            })
            .map(|edge| (edge.source, edge.target))
            .collect::<BTreeSet<_>>();

        unique_edges.len() as f64
            / (variable_nodes.len() as f64 * (variable_nodes.len() as f64 - 1.0))
    };

    finalize_metric_value(metric_id, raw_value, raw_value)
}

fn compute_identifier_repetition_risk(
    subgraph: &CpgSubgraph,
    metric_id: &MetricId,
) -> Option<MetricValue> {
    let identifiers = subgraph
        .nodes
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Variable | NodeKind::Parameter))
        .collect::<Vec<_>>();
    let tokens = identifiers
        .into_iter()
        .flat_map(|node| split_identifier_tokens(&node.name))
        .collect::<Vec<_>>();

    let unique_tokens = tokens.iter().cloned().collect::<BTreeSet<_>>();
    let raw_value = if unique_tokens.len() < 2 {
        0.0
    } else {
        let mut frequencies = BTreeMap::<String, usize>::new();
        for token in tokens {
            *frequencies.entry(token).or_insert(0) += 1;
        }

        let entropy = shannon_entropy(frequencies.values().copied().collect::<Vec<_>>().as_slice());
        1.0 - entropy / (unique_tokens.len() as f64).log2()
    };

    finalize_metric_value(metric_id, raw_value, raw_value)
}

fn compute_module_fan_out_risk(
    subgraph: &CpgSubgraph,
    metric_id: &MetricId,
) -> Option<MetricValue> {
    let module_graph = ModuleDependencyGraph::build(subgraph);
    let raw_value = module_graph
        .module_for_scope(&subgraph.scope_id)
        .map(|module_id| module_graph.fan_out(module_id) as f64)
        .unwrap_or(0.0);
    let normalized_risk = (raw_value / 12.0).clamp(0.0, 1.0);

    finalize_metric_value(metric_id, raw_value, normalized_risk)
}

fn compute_circular_dependency_participation_risk(
    subgraph: &CpgSubgraph,
    metric_id: &MetricId,
) -> Option<MetricValue> {
    let module_graph = ModuleDependencyGraph::build(subgraph);
    let raw_value = module_graph
        .module_for_scope(&subgraph.scope_id)
        .map(|module_id| {
            let scc_size = module_graph.scc_size(module_id);
            if scc_size < 2 {
                0.0
            } else {
                (scc_size as f64 - 1.0) / 5.0
            }
        })
        .unwrap_or(0.0);

    finalize_metric_value(metric_id, raw_value, raw_value)
}

fn compute_instability_risk(subgraph: &CpgSubgraph, metric_id: &MetricId) -> Option<MetricValue> {
    let module_graph = ModuleDependencyGraph::build(subgraph);
    let (raw_value, normalized_risk) = module_graph
        .module_for_scope(&subgraph.scope_id)
        .map(|module_id| {
            let fan_in = module_graph.fan_in(module_id) as f64;
            let fan_out = module_graph.fan_out(module_id) as f64;
            let denominator = fan_in + fan_out;

            if denominator == 0.0 {
                (0.0, 0.0)
            } else {
                let raw_ratio = fan_out / denominator;
                let normalized =
                    if denominator < f64::from(MIN_MODULE_INSTABILITY_DEPENDENCY_SUPPORT) {
                        0.0
                    } else {
                        raw_ratio
                    };
                (raw_ratio, normalized)
            }
        })
        .unwrap_or((0.0, 0.0));

    finalize_metric_value(metric_id, raw_value, normalized_risk)
}

fn compute_cyclic_coupling_risk(
    subgraph: &CpgSubgraph,
    metric_id: &MetricId,
) -> Option<MetricValue> {
    let module_graph = ModuleDependencyGraph::build(subgraph);
    let total_edges = module_graph.total_dependency_edges() as f64;
    let raw_value = if total_edges == 0.0 {
        0.0
    } else {
        module_graph.cyclic_dependency_edges() as f64 / total_edges
    };

    finalize_metric_value(metric_id, raw_value, raw_value)
}

fn compute_module_size_entropy_imbalance_risk(
    subgraph: &CpgSubgraph,
    metric_id: &MetricId,
) -> Option<MetricValue> {
    let module_graph = ModuleDependencyGraph::build(subgraph);
    let module_sizes = module_graph
        .modules
        .iter()
        .map(|module| module_line_count(module))
        .filter(|loc| *loc > 0)
        .collect::<Vec<_>>();

    let raw_value = if module_sizes.len() < 2 {
        0.0
    } else {
        let total_loc = module_sizes.iter().sum::<u32>() as f64;
        let entropy = -module_sizes
            .iter()
            .map(|loc| *loc as f64 / total_loc)
            .map(|share| share * share.log2())
            .sum::<f64>();
        1.0 - entropy / (module_sizes.len() as f64).log2()
    };

    finalize_metric_value(metric_id, raw_value, raw_value)
}

fn compute_hub_dependency_concentration_risk(
    subgraph: &CpgSubgraph,
    metric_id: &MetricId,
) -> Option<MetricValue> {
    let module_graph = ModuleDependencyGraph::build(subgraph);
    let total_in_degree = module_graph.total_dependency_edges() as u32;
    let (raw_value, normalized_risk) = if total_in_degree == 0 {
        (0.0, 0.0)
    } else {
        let max_in_degree = module_graph
            .modules
            .iter()
            .map(|module| module_graph.fan_in(module.id))
            .max()
            .unwrap_or(0) as f64;
        let raw_ratio = max_in_degree / f64::from(total_in_degree);
        let normalized = if total_in_degree < MIN_HUB_CONCENTRATION_IN_DEGREE {
            0.0
        } else {
            raw_ratio
        };
        (raw_ratio, normalized)
    };

    finalize_metric_value(metric_id, raw_value, normalized_risk)
}

/// Count the module-to-module dependency edges the M-P003 rule sees for the
/// given project-level CPG. Exposed so the analysis pipeline can surface a
/// user-visible warning when the signal is suppressed on very small projects.
pub fn project_hub_concentration_support(cpg: &UnifiedCpg) -> u32 {
    let project_subgraph = cpg.subgraph(&ScopeId::new(
        AnalysisLevel::Project,
        "<project>",
        FilePath::from("."),
    ));
    ModuleDependencyGraph::build(&project_subgraph).total_dependency_edges() as u32
}

fn finalize_metric_value(
    metric_id: &MetricId,
    raw_value: f64,
    normalized_risk: f64,
) -> Option<MetricValue> {
    if !raw_value.is_finite() || !normalized_risk.is_finite() {
        return None;
    }

    Some(MetricValue {
        metric_id: metric_id.clone(),
        raw_value: round_half_up(raw_value, 6),
        normalized_risk: round_half_up(normalized_risk.clamp(0.0, 1.0), 6),
    })
}

fn average(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn shannon_entropy(counts: &[usize]) -> f64 {
    let total = counts.iter().sum::<usize>() as f64;
    if total == 0.0 {
        return 0.0;
    }

    -counts
        .iter()
        .map(|count| *count as f64 / total)
        .filter(|probability| *probability > 0.0)
        .map(|probability| probability * probability.log2())
        .sum::<f64>()
}

fn split_identifier_tokens(identifier: &str) -> Vec<String> {
    identifier
        .split('_')
        .filter(|part| !part.is_empty())
        .flat_map(split_camel_case_part)
        .filter(|token| !token.is_empty())
        .collect()
}

fn split_camel_case_part(part: &str) -> Vec<String> {
    let chars = part.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return Vec::new();
    }

    let mut tokens = Vec::new();
    let mut current = String::new();

    for (index, ch) in chars.iter().copied().enumerate() {
        if index > 0 && should_split_token(chars[index - 1], ch, chars.get(index + 1).copied()) {
            tokens.push(current.to_lowercase());
            current.clear();
        }
        current.push(ch);
    }

    if !current.is_empty() {
        tokens.push(current.to_lowercase());
    }

    tokens
}

fn should_split_token(previous: char, current: char, next: Option<char>) -> bool {
    (previous.is_ascii_lowercase() && current.is_ascii_uppercase())
        || (previous.is_ascii_uppercase()
            && current.is_ascii_uppercase()
            && next.is_some_and(|ch| ch.is_ascii_lowercase()))
        || (previous.is_ascii_alphabetic() && current.is_ascii_digit())
        || (previous.is_ascii_digit() && current.is_ascii_alphabetic())
}

fn module_line_count(module: &CpgNode) -> u32 {
    let start = module.location.start_line;
    let end = module.location.end_line;
    if end < start { 0 } else { end - start + 1 }
}

struct ModuleDependencyGraph<'a> {
    modules: Vec<&'a CpgNode>,
    outgoing: BTreeMap<NodeId, BTreeSet<NodeId>>,
    incoming: BTreeMap<NodeId, BTreeSet<NodeId>>,
    scc_id_by_module: BTreeMap<NodeId, usize>,
    scc_size_by_module: BTreeMap<NodeId, usize>,
}

impl<'a> ModuleDependencyGraph<'a> {
    fn build(subgraph: &'a CpgSubgraph) -> Self {
        let modules = subgraph
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Module)
            .collect::<Vec<_>>();
        let contains_graph = subgraph
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Contains)
            .fold(BTreeMap::<NodeId, Vec<NodeId>>::new(), |mut acc, edge| {
                acc.entry(edge.source).or_default().push(edge.target);
                acc
            });
        let ownership = build_module_ownership(&modules, &contains_graph);
        let mut outgoing = BTreeMap::<NodeId, BTreeSet<NodeId>>::new();
        let mut incoming = BTreeMap::<NodeId, BTreeSet<NodeId>>::new();

        for module in &modules {
            outgoing.entry(module.id).or_default();
            incoming.entry(module.id).or_default();
        }

        for edge in subgraph
            .edges
            .iter()
            .filter(|edge| matches!(edge.kind, EdgeKind::Call | EdgeKind::TypeReference))
        {
            let Some(source_modules) = ownership.get(&edge.source) else {
                continue;
            };
            let Some(target_modules) = ownership.get(&edge.target) else {
                continue;
            };

            for source_module in source_modules {
                for target_module in target_modules {
                    if source_module == target_module {
                        continue;
                    }

                    outgoing
                        .entry(*source_module)
                        .or_default()
                        .insert(*target_module);
                    incoming
                        .entry(*target_module)
                        .or_default()
                        .insert(*source_module);
                }
            }
        }

        let mut graph = DiGraph::<NodeId, ()>::new();
        let mut graph_nodes = BTreeMap::<NodeId, _>::new();
        for module in &modules {
            graph_nodes.insert(module.id, graph.add_node(module.id));
        }
        for (source, targets) in &outgoing {
            for target in targets {
                graph.add_edge(graph_nodes[source], graph_nodes[target], ());
            }
        }

        let mut scc_id_by_module = BTreeMap::new();
        let mut scc_size_by_module = BTreeMap::new();
        for (component_id, component) in kosaraju_scc(&graph).into_iter().enumerate() {
            let component_size = component.len();
            for node_index in component {
                let module_id = graph[node_index];
                scc_id_by_module.insert(module_id, component_id);
                scc_size_by_module.insert(module_id, component_size);
            }
        }

        Self {
            modules,
            outgoing,
            incoming,
            scc_id_by_module,
            scc_size_by_module,
        }
    }

    fn module_for_scope(&self, scope_id: &ScopeId) -> Option<NodeId> {
        self.modules
            .iter()
            .find(|module| {
                module.name == scope_id.qualified_name
                    && module.location.file_path == scope_id.file_path
            })
            .or_else(|| {
                self.modules
                    .iter()
                    .find(|module| module.name == scope_id.qualified_name)
            })
            .or_else(|| {
                self.modules
                    .iter()
                    .find(|module| module.location.file_path == scope_id.file_path)
            })
            .map(|module| module.id)
    }

    fn fan_out(&self, module_id: NodeId) -> usize {
        self.outgoing.get(&module_id).map_or(0, BTreeSet::len)
    }

    fn fan_in(&self, module_id: NodeId) -> usize {
        self.incoming.get(&module_id).map_or(0, BTreeSet::len)
    }

    fn scc_size(&self, module_id: NodeId) -> usize {
        self.scc_size_by_module
            .get(&module_id)
            .copied()
            .unwrap_or(1)
    }

    fn total_dependency_edges(&self) -> usize {
        self.outgoing.values().map(BTreeSet::len).sum()
    }

    fn cyclic_dependency_edges(&self) -> usize {
        self.outgoing
            .iter()
            .map(|(source, targets)| {
                targets
                    .iter()
                    .filter(|target| self.shares_non_trivial_scc(*source, **target))
                    .count()
            })
            .sum()
    }

    fn shares_non_trivial_scc(&self, source: NodeId, target: NodeId) -> bool {
        self.scc_id_by_module.get(&source) == self.scc_id_by_module.get(&target)
            && self.scc_size(source) >= 2
    }
}

fn build_module_ownership(
    modules: &[&CpgNode],
    contains_graph: &BTreeMap<NodeId, Vec<NodeId>>,
) -> BTreeMap<NodeId, BTreeSet<NodeId>> {
    let mut ownership = BTreeMap::<NodeId, BTreeSet<NodeId>>::new();

    for module in modules {
        let mut stack = vec![module.id];
        let mut visited = BTreeSet::new();

        while let Some(node_id) = stack.pop() {
            if !visited.insert(node_id) {
                continue;
            }

            if let Some(children) = contains_graph.get(&node_id) {
                stack.extend(children.iter().copied());
            }
        }

        for node_id in visited {
            ownership.entry(node_id).or_default().insert(module.id);
        }
    }

    ownership
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        CfgBranchEntropyRisk, CircularDependencyParticipationRisk, CyclicCouplingRisk,
        CyclomaticComplexityRisk, DataFlowDensityRisk, HubDependencyConcentrationRisk,
        IdentifierRepetitionRisk, InstabilityRisk, MetricConfig, MetricDefinition,
        ModuleFanOutRisk, ModuleSizeEntropyImbalanceRisk, builtin_metric_definitions,
        project_hub_concentration_support,
    };
    use crate::domains::cpg::{
        CpgEdge, CpgId, CpgNode, EdgeKind, NodeId, NodeKind, SourceLocation, UnifiedCpg,
    };
    use crate::domains::metrics::test_fixtures::CpgBuilder;
    use crate::domains::{AnalysisLevel, FilePath, ScopeId};

    fn config() -> MetricConfig {
        MetricConfig {
            entries: BTreeMap::new(),
        }
    }

    #[test]
    fn builtin_metric_definitions_return_all_wave2_metrics() {
        assert_eq!(builtin_metric_definitions().len(), 10);
    }

    #[test]
    fn cfg_branch_entropy_risk_handles_branching_and_empty_cfg() {
        let metric = CfgBranchEntropyRisk::new();
        let branching = CpgBuilder::new()
            .function("f", "crate::f")
            .variable("branch", "branch")
            .variable("left", "left")
            .variable("right", "right")
            .variable("exit", "exit")
            .edge("f", "branch", EdgeKind::ControlFlow)
            .edge("branch", "left", EdgeKind::ControlFlow)
            .edge("branch", "right", EdgeKind::ControlFlow)
            .edge("left", "exit", EdgeKind::ControlFlow)
            .edge("right", "exit", EdgeKind::ControlFlow)
            .build(ScopeId::new(
                AnalysisLevel::Function,
                "crate::f",
                "src/lib.rs",
            ));
        let linear = CpgBuilder::new()
            .function("f", "crate::f")
            .variable("exit", "exit")
            .edge("f", "exit", EdgeKind::ControlFlow)
            .build(ScopeId::new(
                AnalysisLevel::Function,
                "crate::f",
                "src/lib.rs",
            ));

        let first = metric.compute(&branching, &config()).unwrap();
        let second = metric.compute(&branching, &config()).unwrap();
        let empty = metric.compute(&linear, &config()).unwrap();

        assert_eq!(first.raw_value, 0.5);
        assert_eq!(first.normalized_risk, 0.5);
        assert_eq!(first, second);
        assert_eq!(empty.raw_value, 0.0);
        assert_eq!(empty.normalized_risk, 0.0);
    }

    #[test]
    fn cyclomatic_complexity_risk_handles_basic_and_degenerate_cfgs() {
        let metric = CyclomaticComplexityRisk::new();
        let branching = CpgBuilder::new()
            .function("f", "crate::f")
            .variable("branch", "branch")
            .variable("left", "left")
            .variable("right", "right")
            .variable("exit", "exit")
            .edge("f", "branch", EdgeKind::ControlFlow)
            .edge("branch", "left", EdgeKind::ControlFlow)
            .edge("branch", "right", EdgeKind::ControlFlow)
            .edge("left", "exit", EdgeKind::ControlFlow)
            .edge("right", "exit", EdgeKind::ControlFlow)
            .build(ScopeId::new(
                AnalysisLevel::Function,
                "crate::f",
                "src/lib.rs",
            ));
        let empty = CpgBuilder::new()
            .function("f", "crate::f")
            .build(ScopeId::new(
                AnalysisLevel::Function,
                "crate::f",
                "src/lib.rs",
            ));

        let complex = metric.compute(&branching, &config()).unwrap();
        let simple = metric.compute(&empty, &config()).unwrap();

        assert_eq!(complex.raw_value, 2.0);
        assert_eq!(complex.normalized_risk, 0.066667);
        assert_eq!(simple.raw_value, 1.0);
        assert_eq!(simple.normalized_risk, 0.0);
    }

    #[test]
    fn subgraph_filters_by_function_scope_for_cyclomatic_complexity() {
        let metric = CyclomaticComplexityRisk::new();
        let graph = UnifiedCpg {
            id: CpgId::from("graph"),
            nodes: vec![
                CpgNode {
                    id: NodeId::from(1),
                    kind: NodeKind::Function,
                    name: "crate::a".to_owned(),
                    location: SourceLocation {
                        file_path: FilePath::from("src/a.rs"),
                        start_line: 1,
                        end_line: 6,
                    },
                    extension: None,
                },
                CpgNode {
                    id: NodeId::from(2),
                    kind: NodeKind::Variable,
                    name: "cond".to_owned(),
                    location: SourceLocation {
                        file_path: FilePath::from("src/a.rs"),
                        start_line: 2,
                        end_line: 2,
                    },
                    extension: None,
                },
                CpgNode {
                    id: NodeId::from(3),
                    kind: NodeKind::Variable,
                    name: "then".to_owned(),
                    location: SourceLocation {
                        file_path: FilePath::from("src/a.rs"),
                        start_line: 3,
                        end_line: 3,
                    },
                    extension: None,
                },
                CpgNode {
                    id: NodeId::from(4),
                    kind: NodeKind::Variable,
                    name: "exit".to_owned(),
                    location: SourceLocation {
                        file_path: FilePath::from("src/a.rs"),
                        start_line: 4,
                        end_line: 4,
                    },
                    extension: None,
                },
                CpgNode {
                    id: NodeId::from(5),
                    kind: NodeKind::Function,
                    name: "crate::b".to_owned(),
                    location: SourceLocation {
                        file_path: FilePath::from("src/b.rs"),
                        start_line: 1,
                        end_line: 3,
                    },
                    extension: None,
                },
                CpgNode {
                    id: NodeId::from(6),
                    kind: NodeKind::Variable,
                    name: "exit".to_owned(),
                    location: SourceLocation {
                        file_path: FilePath::from("src/b.rs"),
                        start_line: 2,
                        end_line: 2,
                    },
                    extension: None,
                },
            ],
            edges: vec![
                CpgEdge {
                    source: NodeId::from(1),
                    target: NodeId::from(2),
                    kind: EdgeKind::Contains,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(1),
                    target: NodeId::from(3),
                    kind: EdgeKind::Contains,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(1),
                    target: NodeId::from(4),
                    kind: EdgeKind::Contains,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(5),
                    target: NodeId::from(6),
                    kind: EdgeKind::Contains,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(1),
                    target: NodeId::from(2),
                    kind: EdgeKind::ControlFlow,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(2),
                    target: NodeId::from(3),
                    kind: EdgeKind::ControlFlow,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(2),
                    target: NodeId::from(4),
                    kind: EdgeKind::ControlFlow,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(3),
                    target: NodeId::from(4),
                    kind: EdgeKind::ControlFlow,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(5),
                    target: NodeId::from(6),
                    kind: EdgeKind::ControlFlow,
                    extension: None,
                },
            ],
        };

        let function_a = graph.subgraph(&ScopeId::new(
            AnalysisLevel::Function,
            "crate::a",
            "src/a.rs",
        ));
        let function_b = graph.subgraph(&ScopeId::new(
            AnalysisLevel::Function,
            "crate::b",
            "src/b.rs",
        ));

        let value_a = metric.compute(&function_a, &config()).unwrap();
        let value_b = metric.compute(&function_b, &config()).unwrap();

        assert_eq!(value_a.raw_value, 2.0);
        assert_eq!(value_a.normalized_risk, 0.066667);
        assert_eq!(value_b.raw_value, 1.0);
        assert_eq!(value_b.normalized_risk, 0.0);
        assert!(value_a.raw_value >= 0.0);
        assert!(value_b.raw_value >= 0.0);
        assert_ne!(value_a.raw_value, value_b.raw_value);
    }

    #[test]
    fn data_flow_density_risk_uses_unique_variable_pairs() {
        let metric = DataFlowDensityRisk::new();
        let dense = CpgBuilder::new()
            .function("f", "crate::f")
            .parameter("p", "input")
            .variable("a", "alpha")
            .variable("b", "beta")
            .variable("c", "gamma")
            .edge("p", "a", EdgeKind::DataFlow)
            .edge("a", "b", EdgeKind::DataFlow)
            .edge("b", "c", EdgeKind::DataFlow)
            .edge("p", "a", EdgeKind::DataFlow)
            .build(ScopeId::new(
                AnalysisLevel::Function,
                "crate::f",
                "src/lib.rs",
            ));
        let sparse = CpgBuilder::new()
            .function("f", "crate::f")
            .variable("only", "value")
            .build(ScopeId::new(
                AnalysisLevel::Function,
                "crate::f",
                "src/lib.rs",
            ));

        let dense_value = metric.compute(&dense, &config()).unwrap();
        let sparse_value = metric.compute(&sparse, &config()).unwrap();

        assert_eq!(dense_value.raw_value, 0.25);
        assert_eq!(dense_value.normalized_risk, 0.25);
        assert_eq!(sparse_value.raw_value, 0.0);
        assert_eq!(sparse_value.normalized_risk, 0.0);
    }

    #[test]
    fn identifier_repetition_risk_handles_token_entropy_and_degenerate_input() {
        let metric = IdentifierRepetitionRisk::new();
        let repeated = CpgBuilder::new()
            .function("f", "crate::f")
            .parameter("p1", "foo_count")
            .parameter("p2", "fooTotal")
            .build(ScopeId::new(
                AnalysisLevel::Function,
                "crate::f",
                "src/lib.rs",
            ));
        let degenerate = CpgBuilder::new()
            .function("f", "crate::f")
            .parameter("p1", "value")
            .build(ScopeId::new(
                AnalysisLevel::Function,
                "crate::f",
                "src/lib.rs",
            ));

        let repeated_value = metric.compute(&repeated, &config()).unwrap();
        let degenerate_value = metric.compute(&degenerate, &config()).unwrap();

        assert_eq!(repeated_value.raw_value, 0.053605);
        assert_eq!(repeated_value.normalized_risk, 0.053605);
        assert_eq!(degenerate_value.raw_value, 0.0);
        assert_eq!(degenerate_value.normalized_risk, 0.0);
    }

    #[test]
    fn function_metrics_return_zero_risk_when_subgraph_lacks_supporting_data() {
        let cfg_entropy = CfgBranchEntropyRisk::new();
        let complexity = CyclomaticComplexityRisk::new();
        let data_flow_density = DataFlowDensityRisk::new();
        let identifier_repetition = IdentifierRepetitionRisk::new();
        let scope = ScopeId::new(AnalysisLevel::Function, "crate::f", "src/lib.rs");
        let graph = UnifiedCpg {
            id: CpgId::from("graph"),
            nodes: vec![
                CpgNode {
                    id: NodeId::from(1),
                    kind: NodeKind::Module,
                    name: "crate".to_owned(),
                    location: SourceLocation {
                        file_path: FilePath::from("src/lib.rs"),
                        start_line: 1,
                        end_line: 10,
                    },
                    extension: None,
                },
                CpgNode {
                    id: NodeId::from(2),
                    kind: NodeKind::Function,
                    name: "crate::f".to_owned(),
                    location: SourceLocation {
                        file_path: FilePath::from("src/lib.rs"),
                        start_line: 2,
                        end_line: 6,
                    },
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

        let subgraph = graph.subgraph(&scope);

        assert_eq!(subgraph.nodes.len(), 1);
        assert_eq!(subgraph.edges.len(), 0);
        let cfg_entropy_value = cfg_entropy.compute(&subgraph, &config()).unwrap();
        let complexity_value = complexity.compute(&subgraph, &config()).unwrap();
        let data_flow_density_value = data_flow_density.compute(&subgraph, &config()).unwrap();
        let identifier_repetition_value =
            identifier_repetition.compute(&subgraph, &config()).unwrap();

        assert_eq!(cfg_entropy_value.raw_value, 0.0);
        assert_eq!(cfg_entropy_value.normalized_risk, 0.0);
        assert_eq!(complexity_value.raw_value, 1.0);
        assert_eq!(complexity_value.normalized_risk, 0.0);
        assert_eq!(data_flow_density_value.raw_value, 0.0);
        assert_eq!(data_flow_density_value.normalized_risk, 0.0);
        assert_eq!(identifier_repetition_value.raw_value, 0.0);
        assert_eq!(identifier_repetition_value.normalized_risk, 0.0);
    }

    #[test]
    fn function_metrics_return_some_when_subgraph_has_supporting_data() {
        let cfg_entropy = CfgBranchEntropyRisk::new();
        let complexity = CyclomaticComplexityRisk::new();
        let data_flow_density = DataFlowDensityRisk::new();
        let identifier_repetition = IdentifierRepetitionRisk::new();
        let scope = ScopeId::new(AnalysisLevel::Function, "crate::f", "src/lib.rs");
        let graph = UnifiedCpg {
            id: CpgId::from("graph"),
            nodes: vec![
                CpgNode {
                    id: NodeId::from(1),
                    kind: NodeKind::Function,
                    name: "crate::f".to_owned(),
                    location: SourceLocation {
                        file_path: FilePath::from("src/lib.rs"),
                        start_line: 1,
                        end_line: 8,
                    },
                    extension: None,
                },
                CpgNode {
                    id: NodeId::from(2),
                    kind: NodeKind::Parameter,
                    name: "foo_count".to_owned(),
                    location: SourceLocation {
                        file_path: FilePath::from("src/lib.rs"),
                        start_line: 2,
                        end_line: 2,
                    },
                    extension: None,
                },
                CpgNode {
                    id: NodeId::from(3),
                    kind: NodeKind::Variable,
                    name: "fooTotal".to_owned(),
                    location: SourceLocation {
                        file_path: FilePath::from("src/lib.rs"),
                        start_line: 3,
                        end_line: 3,
                    },
                    extension: None,
                },
                CpgNode {
                    id: NodeId::from(4),
                    kind: NodeKind::Variable,
                    name: "exit".to_owned(),
                    location: SourceLocation {
                        file_path: FilePath::from("src/lib.rs"),
                        start_line: 4,
                        end_line: 4,
                    },
                    extension: None,
                },
            ],
            edges: vec![
                CpgEdge {
                    source: NodeId::from(1),
                    target: NodeId::from(2),
                    kind: EdgeKind::Contains,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(1),
                    target: NodeId::from(3),
                    kind: EdgeKind::Contains,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(1),
                    target: NodeId::from(4),
                    kind: EdgeKind::Contains,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(1),
                    target: NodeId::from(2),
                    kind: EdgeKind::ControlFlow,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(2),
                    target: NodeId::from(3),
                    kind: EdgeKind::ControlFlow,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(2),
                    target: NodeId::from(4),
                    kind: EdgeKind::ControlFlow,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(3),
                    target: NodeId::from(4),
                    kind: EdgeKind::ControlFlow,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(2),
                    target: NodeId::from(3),
                    kind: EdgeKind::DataFlow,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(3),
                    target: NodeId::from(4),
                    kind: EdgeKind::DataFlow,
                    extension: None,
                },
            ],
        };

        let subgraph = graph.subgraph(&scope);
        let cfg_entropy_value = cfg_entropy.compute(&subgraph, &config());
        let complexity_value = complexity.compute(&subgraph, &config());
        let data_flow_density_value = data_flow_density.compute(&subgraph, &config());
        let identifier_repetition_value = identifier_repetition.compute(&subgraph, &config());

        assert_eq!(subgraph.nodes.len(), 4);
        assert_eq!(subgraph.edges.len(), 9);
        assert!(matches!(
            cfg_entropy_value.as_ref().map(|value| value.raw_value),
            Some(raw_value) if raw_value > 0.0
        ));
        assert!(matches!(
            complexity_value.as_ref().map(|value| value.raw_value),
            Some(raw_value) if raw_value > 1.0
        ));
        assert!(matches!(
            data_flow_density_value.as_ref().map(|value| value.raw_value),
            Some(raw_value) if raw_value > 0.0
        ));
        assert!(matches!(
            identifier_repetition_value.as_ref().map(|value| value.raw_value),
            Some(raw_value) if raw_value > 0.0
        ));
    }

    #[test]
    fn module_metrics_compute_fan_out_cycle_participation_and_instability() {
        let fan_out_metric = ModuleFanOutRisk::new();
        let cycle_metric = CircularDependencyParticipationRisk::new();
        let instability_metric = InstabilityRisk::new();
        let graph = CpgBuilder::new()
            .module_at("module_a", "crate::A", "src/a.rs", 1, 10)
            .module_at("module_b", "crate::B", "src/b.rs", 1, 10)
            .module_at("module_c", "crate::C", "src/c.rs", 1, 20)
            .function_at("a_fn", "a_fn", "src/a.rs", 2, 4)
            .function_at("b_fn", "b_fn", "src/b.rs", 2, 4)
            .function_at("c_fn", "c_fn", "src/c.rs", 2, 4)
            .edge("module_a", "a_fn", EdgeKind::Contains)
            .edge("module_b", "b_fn", EdgeKind::Contains)
            .edge("module_c", "c_fn", EdgeKind::Contains)
            .edge("a_fn", "b_fn", EdgeKind::Call)
            .edge("a_fn", "c_fn", EdgeKind::Call)
            .edge("b_fn", "a_fn", EdgeKind::Call)
            .build(ScopeId::new(AnalysisLevel::Module, "crate::A", "src/a.rs"));
        let isolated = CpgBuilder::new()
            .module_at("module_a", "crate::A", "src/a.rs", 1, 10)
            .function_at("a_fn", "a_fn", "src/a.rs", 2, 4)
            .edge("module_a", "a_fn", EdgeKind::Contains)
            .build(ScopeId::new(AnalysisLevel::Module, "crate::A", "src/a.rs"));

        let fan_out = fan_out_metric.compute(&graph, &config()).unwrap();
        let cycle = cycle_metric.compute(&graph, &config()).unwrap();
        let instability = instability_metric.compute(&graph, &config()).unwrap();
        let isolated_cycle = cycle_metric.compute(&isolated, &config()).unwrap();
        let isolated_instability = instability_metric.compute(&isolated, &config()).unwrap();

        assert_eq!(fan_out.raw_value, 2.0);
        assert_eq!(fan_out.normalized_risk, 0.166667);
        assert_eq!(cycle.raw_value, 0.2);
        assert_eq!(cycle.normalized_risk, 0.2);
        assert_eq!(instability.raw_value, 0.666667);
        assert_eq!(instability.normalized_risk, 0.666667);
        assert_eq!(isolated_cycle.raw_value, 0.0);
        assert_eq!(isolated_instability.raw_value, 0.0);
    }

    #[test]
    fn circular_dependency_risk_ignores_parent_child_contains_edges() {
        let metric = CircularDependencyParticipationRisk::new();
        let parent_graph = CpgBuilder::new()
            .module_at("parent", "crate::parent", "src/parent.rs", 1, 10)
            .module_at(
                "child",
                "crate::parent::child",
                "src/parent/child.rs",
                1,
                10,
            )
            .function_at(
                "child_fn",
                "crate::parent::child::f",
                "src/parent/child.rs",
                2,
                2,
            )
            .edge("parent", "child", EdgeKind::Contains)
            .edge("child", "child_fn", EdgeKind::Contains)
            .build(ScopeId::new(
                AnalysisLevel::Module,
                "crate::parent",
                "src/parent.rs",
            ));
        let child_graph = CpgBuilder::new()
            .module_at("parent", "crate::parent", "src/parent.rs", 1, 10)
            .module_at(
                "child",
                "crate::parent::child",
                "src/parent/child.rs",
                1,
                10,
            )
            .function_at(
                "child_fn",
                "crate::parent::child::f",
                "src/parent/child.rs",
                2,
                2,
            )
            .edge("parent", "child", EdgeKind::Contains)
            .edge("child", "child_fn", EdgeKind::Contains)
            .build(ScopeId::new(
                AnalysisLevel::Module,
                "crate::parent::child",
                "src/parent/child.rs",
            ));

        let parent_value = metric.compute(&parent_graph, &config()).unwrap();
        let child_value = metric.compute(&child_graph, &config()).unwrap();

        assert_eq!(parent_value.raw_value, 0.0);
        assert_eq!(parent_value.normalized_risk, 0.0);
        assert_eq!(child_value.raw_value, 0.0);
        assert_eq!(child_value.normalized_risk, 0.0);
    }

    #[test]
    fn module_metrics_use_full_graph_for_module_scope_subgraphs() {
        let fan_out_metric = ModuleFanOutRisk::new();
        let cycle_metric = CircularDependencyParticipationRisk::new();
        let instability_metric = InstabilityRisk::new();
        let graph = UnifiedCpg {
            id: CpgId::from("graph"),
            nodes: vec![
                CpgNode {
                    id: NodeId::from(1),
                    kind: NodeKind::Module,
                    name: "crate::A".to_owned(),
                    location: SourceLocation {
                        file_path: FilePath::from("src/a.rs"),
                        start_line: 1,
                        end_line: 10,
                    },
                    extension: None,
                },
                CpgNode {
                    id: NodeId::from(2),
                    kind: NodeKind::Module,
                    name: "crate::B".to_owned(),
                    location: SourceLocation {
                        file_path: FilePath::from("src/b.rs"),
                        start_line: 1,
                        end_line: 10,
                    },
                    extension: None,
                },
                CpgNode {
                    id: NodeId::from(3),
                    kind: NodeKind::Module,
                    name: "crate::C".to_owned(),
                    location: SourceLocation {
                        file_path: FilePath::from("src/c.rs"),
                        start_line: 1,
                        end_line: 20,
                    },
                    extension: None,
                },
                CpgNode {
                    id: NodeId::from(4),
                    kind: NodeKind::Function,
                    name: "crate::A::a_fn".to_owned(),
                    location: SourceLocation {
                        file_path: FilePath::from("src/a.rs"),
                        start_line: 2,
                        end_line: 4,
                    },
                    extension: None,
                },
                CpgNode {
                    id: NodeId::from(5),
                    kind: NodeKind::Function,
                    name: "crate::B::b_fn".to_owned(),
                    location: SourceLocation {
                        file_path: FilePath::from("src/b.rs"),
                        start_line: 2,
                        end_line: 4,
                    },
                    extension: None,
                },
                CpgNode {
                    id: NodeId::from(6),
                    kind: NodeKind::Function,
                    name: "crate::C::c_fn".to_owned(),
                    location: SourceLocation {
                        file_path: FilePath::from("src/c.rs"),
                        start_line: 2,
                        end_line: 4,
                    },
                    extension: None,
                },
            ],
            edges: vec![
                CpgEdge {
                    source: NodeId::from(1),
                    target: NodeId::from(4),
                    kind: EdgeKind::Contains,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(2),
                    target: NodeId::from(5),
                    kind: EdgeKind::Contains,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(3),
                    target: NodeId::from(6),
                    kind: EdgeKind::Contains,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(4),
                    target: NodeId::from(5),
                    kind: EdgeKind::Call,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(4),
                    target: NodeId::from(6),
                    kind: EdgeKind::Call,
                    extension: None,
                },
                CpgEdge {
                    source: NodeId::from(5),
                    target: NodeId::from(4),
                    kind: EdgeKind::Call,
                    extension: None,
                },
            ],
        };
        let scope = ScopeId::new(AnalysisLevel::Module, "crate::A", "src/a.rs");
        let subgraph = graph.subgraph(&scope);

        let fan_out = fan_out_metric.compute(&subgraph, &config()).unwrap();
        let cycle = cycle_metric.compute(&subgraph, &config()).unwrap();
        let instability = instability_metric.compute(&subgraph, &config()).unwrap();

        assert_eq!(fan_out.raw_value, 2.0);
        assert_eq!(fan_out.normalized_risk, 0.166667);
        assert_eq!(cycle.raw_value, 0.2);
        assert_eq!(cycle.normalized_risk, 0.2);
        assert_eq!(instability.raw_value, 0.666667);
        assert_eq!(instability.normalized_risk, 0.666667);
    }

    #[test]
    fn project_metrics_handle_cycles_entropy_and_hubs() {
        let cyclic_metric = CyclicCouplingRisk::new();
        let entropy_metric = ModuleSizeEntropyImbalanceRisk::new();
        let hub_metric = HubDependencyConcentrationRisk::new();
        let cyclic_graph = CpgBuilder::new()
            .module_at("module_a", "crate::A", "src/a.rs", 1, 10)
            .module_at("module_b", "crate::B", "src/b.rs", 1, 10)
            .module_at("module_c", "crate::C", "src/c.rs", 1, 20)
            .function_at("a_fn", "a_fn", "src/a.rs", 2, 4)
            .function_at("b_fn", "b_fn", "src/b.rs", 2, 4)
            .function_at("c_fn", "c_fn", "src/c.rs", 2, 4)
            .edge("module_a", "a_fn", EdgeKind::Contains)
            .edge("module_b", "b_fn", EdgeKind::Contains)
            .edge("module_c", "c_fn", EdgeKind::Contains)
            .edge("a_fn", "b_fn", EdgeKind::Call)
            .edge("b_fn", "a_fn", EdgeKind::Call)
            .edge("c_fn", "b_fn", EdgeKind::Call)
            .build(ScopeId::new(AnalysisLevel::Project, "<project>", "."));
        let empty_graph = CpgBuilder::new()
            .module_at("module_a", "crate::A", "src/a.rs", 1, 10)
            .build(ScopeId::new(AnalysisLevel::Project, "<project>", "."));

        let cyclic = cyclic_metric.compute(&cyclic_graph, &config()).unwrap();
        let entropy = entropy_metric.compute(&cyclic_graph, &config()).unwrap();
        let hub = hub_metric.compute(&cyclic_graph, &config()).unwrap();
        let empty_cyclic = cyclic_metric.compute(&empty_graph, &config()).unwrap();
        let empty_entropy = entropy_metric.compute(&empty_graph, &config()).unwrap();
        let empty_hub = hub_metric.compute(&empty_graph, &config()).unwrap();
        let repeat = hub_metric.compute(&cyclic_graph, &config()).unwrap();

        assert_eq!(cyclic.raw_value, 0.666667);
        assert_eq!(cyclic.normalized_risk, 0.666667);
        assert_eq!(entropy.raw_value, 0.053605);
        assert_eq!(entropy.normalized_risk, 0.053605);
        assert_eq!(hub.raw_value, 0.666667);
        assert_eq!(hub.normalized_risk, 0.666667);
        assert_eq!(empty_cyclic.raw_value, 0.0);
        assert_eq!(empty_entropy.raw_value, 0.0);
        assert_eq!(empty_hub.raw_value, 0.0);
        assert_eq!(hub, repeat);
    }

    #[test]
    fn cyclic_coupling_risk_ignores_parent_child_contains_edges() {
        let metric = CyclicCouplingRisk::new();
        let graph = CpgBuilder::new()
            .module_at("parent", "crate::parent", "src/parent.rs", 1, 10)
            .module_at(
                "child",
                "crate::parent::child",
                "src/parent/child.rs",
                1,
                10,
            )
            .function_at(
                "child_fn",
                "crate::parent::child::f",
                "src/parent/child.rs",
                2,
                2,
            )
            .edge("parent", "child", EdgeKind::Contains)
            .edge("child", "child_fn", EdgeKind::Contains)
            .build(ScopeId::new(AnalysisLevel::Project, "<project>", "."));

        let value = metric.compute(&graph, &config()).unwrap();

        assert_eq!(value.raw_value, 0.0);
        assert_eq!(value.normalized_risk, 0.0);
    }

    // Regression for kalos #59: on tiny projects the M-P003 ratio is forced
    // toward 1.0 by structural necessity (a single dependency edge yields
    // max/total = 1/1). The compute function must report the raw ratio for
    // transparency while clamping normalized_risk to 0 so KAL-P003 does not
    // fire on freshly scaffolded two-module repositories.
    #[test]
    fn hub_dependency_concentration_suppresses_normalized_risk_on_tiny_projects() {
        let hub_metric = HubDependencyConcentrationRisk::new();
        let tiny_graph = CpgBuilder::new()
            .module_at("module_app", "src/app.py", "src/app.py", 1, 2)
            .module_at(
                "module_test",
                "tests/test_app.py",
                "tests/test_app.py",
                1,
                4,
            )
            .function_at("greet", "app.greet", "src/app.py", 1, 2)
            .function_at(
                "test_greet",
                "tests.test_app.test_greet",
                "tests/test_app.py",
                3,
                4,
            )
            .edge("module_app", "greet", EdgeKind::Contains)
            .edge("module_test", "test_greet", EdgeKind::Contains)
            .edge("test_greet", "greet", EdgeKind::Call)
            .build(ScopeId::new(AnalysisLevel::Project, "<project>", "."));

        let value = hub_metric.compute(&tiny_graph, &config()).unwrap();

        assert_eq!(value.raw_value, 1.0);
        assert_eq!(value.normalized_risk, 0.0);
    }

    #[test]
    fn instability_suppresses_normalized_risk_for_tiny_leaf_adapter() {
        let metric = InstabilityRisk::new();
        let graph = CpgBuilder::new()
            .module_at("adapter", "src/adapter.py", "src/adapter.py", 1, 2)
            .module_at("sdk", "src/sdk.py", "src/sdk.py", 1, 2)
            .function_at("call_sdk", "adapter.call_sdk", "src/adapter.py", 1, 2)
            .function_at("request", "sdk.request", "src/sdk.py", 1, 2)
            .edge("adapter", "call_sdk", EdgeKind::Contains)
            .edge("sdk", "request", EdgeKind::Contains)
            .edge("call_sdk", "request", EdgeKind::Call)
            .build(ScopeId::new(
                AnalysisLevel::Module,
                "src/adapter.py",
                "src/adapter.py",
            ));

        let value = metric.compute(&graph, &config()).unwrap();

        assert_eq!(value.raw_value, 1.0);
        assert_eq!(value.normalized_risk, 0.0);
    }

    #[test]
    fn instability_retains_normalized_risk_at_minimum_sample() {
        let metric = InstabilityRisk::new();
        let graph = CpgBuilder::new()
            .module_at("root", "crate::root", "src/root.rs", 1, 10)
            .module_at("a", "crate::a", "src/a.rs", 1, 2)
            .module_at("b", "crate::b", "src/b.rs", 1, 2)
            .module_at("c", "crate::c", "src/c.rs", 1, 2)
            .function_at("root_fn", "root_fn", "src/root.rs", 2, 4)
            .function_at("a_fn", "a_fn", "src/a.rs", 1, 2)
            .function_at("b_fn", "b_fn", "src/b.rs", 1, 2)
            .function_at("c_fn", "c_fn", "src/c.rs", 1, 2)
            .edge("root", "root_fn", EdgeKind::Contains)
            .edge("a", "a_fn", EdgeKind::Contains)
            .edge("b", "b_fn", EdgeKind::Contains)
            .edge("c", "c_fn", EdgeKind::Contains)
            .edge("root_fn", "a_fn", EdgeKind::Call)
            .edge("root_fn", "b_fn", EdgeKind::Call)
            .edge("root_fn", "c_fn", EdgeKind::Call)
            .build(ScopeId::new(
                AnalysisLevel::Module,
                "crate::root",
                "src/root.rs",
            ));

        let value = metric.compute(&graph, &config()).unwrap();

        assert_eq!(value.raw_value, 1.0);
        assert_eq!(value.normalized_risk, 1.0);
    }

    #[test]
    fn hub_dependency_concentration_suppresses_normalized_risk_with_two_edges() {
        let hub_metric = HubDependencyConcentrationRisk::new();
        let graph = CpgBuilder::new()
            .module_at("module_a", "crate::A", "src/a.rs", 1, 10)
            .module_at("module_b", "crate::B", "src/b.rs", 1, 10)
            .module_at("module_c", "crate::C", "src/c.rs", 1, 10)
            .function_at("a_fn", "a_fn", "src/a.rs", 2, 4)
            .function_at("b_fn", "b_fn", "src/b.rs", 2, 4)
            .function_at("c_fn", "c_fn", "src/c.rs", 2, 4)
            .edge("module_a", "a_fn", EdgeKind::Contains)
            .edge("module_b", "b_fn", EdgeKind::Contains)
            .edge("module_c", "c_fn", EdgeKind::Contains)
            .edge("a_fn", "b_fn", EdgeKind::Call)
            .edge("c_fn", "b_fn", EdgeKind::Call)
            .build(ScopeId::new(AnalysisLevel::Project, "<project>", "."));
        let cpg = UnifiedCpg {
            id: CpgId::from("two-edge-boundary"),
            nodes: graph.nodes.clone(),
            edges: graph.edges.clone(),
        };

        let value = hub_metric.compute(&graph, &config()).unwrap();

        assert_eq!(project_hub_concentration_support(&cpg), 2);
        assert_eq!(value.raw_value, 1.0);
        assert_eq!(value.normalized_risk, 0.0);
    }

    #[test]
    fn hub_dependency_concentration_retains_normalized_risk_at_minimum_sample() {
        let hub_metric = HubDependencyConcentrationRisk::new();
        let graph = CpgBuilder::new()
            .module_at("module_a", "crate::A", "src/a.rs", 1, 10)
            .module_at("module_b", "crate::B", "src/b.rs", 1, 10)
            .module_at("module_c", "crate::C", "src/c.rs", 1, 10)
            .function_at("a_fn", "a_fn", "src/a.rs", 2, 4)
            .function_at("b_fn", "b_fn", "src/b.rs", 2, 4)
            .function_at("c_fn", "c_fn", "src/c.rs", 2, 4)
            .edge("module_a", "a_fn", EdgeKind::Contains)
            .edge("module_b", "b_fn", EdgeKind::Contains)
            .edge("module_c", "c_fn", EdgeKind::Contains)
            .edge("a_fn", "b_fn", EdgeKind::Call)
            .edge("c_fn", "b_fn", EdgeKind::Call)
            .edge("b_fn", "a_fn", EdgeKind::Call)
            .build(ScopeId::new(AnalysisLevel::Project, "<project>", "."));

        let value = hub_metric.compute(&graph, &config()).unwrap();

        assert_eq!(value.raw_value, value.normalized_risk);
        assert!(value.normalized_risk > 0.0);
    }

    #[test]
    fn project_hub_concentration_support_counts_module_dependency_edges() {
        let tiny_subgraph = CpgBuilder::new()
            .module_at("module_app", "src/app.py", "src/app.py", 1, 2)
            .module_at(
                "module_test",
                "tests/test_app.py",
                "tests/test_app.py",
                1,
                4,
            )
            .function_at("greet", "app.greet", "src/app.py", 1, 2)
            .function_at(
                "test_greet",
                "tests.test_app.test_greet",
                "tests/test_app.py",
                3,
                4,
            )
            .edge("module_app", "greet", EdgeKind::Contains)
            .edge("module_test", "test_greet", EdgeKind::Contains)
            .edge("test_greet", "greet", EdgeKind::Call)
            .build(ScopeId::new(AnalysisLevel::Project, "<project>", "."));
        let tiny_cpg = UnifiedCpg {
            id: CpgId::from("tiny"),
            nodes: tiny_subgraph.nodes,
            edges: tiny_subgraph.edges,
        };

        assert_eq!(project_hub_concentration_support(&tiny_cpg), 1);
    }
}
