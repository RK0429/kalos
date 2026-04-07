use std::collections::{BTreeMap, BTreeSet};

use petgraph::algo::kosaraju_scc;
use petgraph::graph::DiGraph;

use crate::domains::config::{DEFAULT_METRIC_RULE_THRESHOLDS, DEFAULT_PATTERN_RULE_SEVERITIES};
use crate::domains::cpg::{CpgNode, CpgSubgraph, EdgeKind, Language, NodeId, NodeKind, UnifiedCpg};
use crate::domains::metrics::{AnalysisMetrics, round_half_up};

use super::{AnalysisLevel, DiagnosticId, FilePath, MetricId, RuleId, ScopeId, Severity};

#[derive(Clone, Debug, PartialEq)]
pub struct DiagnosticReport {
    pub diagnostics: Vec<Diagnostic>,
    pub summary: DiagnosticSummary,
    pub diagnostics_scope: DiagnosticsScope,
    pub summary_scope: SummaryScope,
}

impl DiagnosticReport {
    pub fn determine_exit_code(&self, strict: bool) -> ExitCode {
        if self.summary.error_count > 0 || (strict && self.summary.warning_count > 0) {
            ExitCode::DiagnosticFailure
        } else {
            ExitCode::Success
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub id: DiagnosticId,
    pub primary_scope_id: ScopeId,
    pub rule_id: RuleId,
    pub kind: DiagnosticKind,
    pub severity: Severity,
    pub location: FileLocation,
    pub message: String,
    pub metric: Option<MetricObservation>,
    pub pattern: Option<PatternEvidence>,
    pub template_suggestion: TemplateSuggestion,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MetricRule {
    pub id: RuleId,
    pub metric_id: MetricId,
    pub default_threshold: f64,
    pub description: String,
    pub suggestion_template: String,
}

impl MetricRule {
    pub fn evaluate(
        &self,
        scope_id: &ScopeId,
        location: &FileLocation,
        obs: &MetricObservation,
        config: &RuleConfig,
    ) -> Option<Diagnostic> {
        if config.enabled == Some(false) {
            return None;
        }

        let threshold = round_half_up(config.threshold.unwrap_or(self.default_threshold), 6);
        if obs.normalized_risk <= threshold {
            return None;
        }

        let overflow_ratio = round_half_up(
            (obs.normalized_risk - threshold) / (1.0 - threshold).max(1e-9),
            6,
        );
        let severity = config.severity.unwrap_or({
            if overflow_ratio < 0.25 {
                Severity::Info
            } else if overflow_ratio < 0.60 {
                Severity::Warning
            } else {
                Severity::Error
            }
        });

        Some(Diagnostic {
            id: DiagnosticId::from(format!("{}-{}", self.id, scope_id.qualified_name)),
            primary_scope_id: scope_id.clone(),
            rule_id: self.id.clone(),
            kind: DiagnosticKind::Metric,
            severity,
            location: FileLocation {
                file_path: scope_id.file_path.clone(),
                start_line: location.start_line,
                end_line: location.end_line,
                column: location.column,
            },
            message: format!(
                "{} normalized risk {:.6} exceeded threshold {:.6}",
                obs.metric_id.as_str(),
                obs.normalized_risk,
                threshold
            ),
            metric: Some(MetricObservation {
                metric_id: obs.metric_id.clone(),
                raw_value: obs.raw_value,
                normalized_risk: obs.normalized_risk,
                threshold,
                overflow_ratio,
            }),
            pattern: None,
            template_suggestion: template_suggestion(&self.suggestion_template),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatternRule {
    pub id: RuleId,
    pub pattern_type: PatternType,
    pub evaluation_scope: AnalysisLevel,
    pub default_severity: Severity,
    pub description: String,
    pub suggestion_template: String,
}

impl PatternRule {
    pub fn detect(
        &self,
        cpg: &CpgSubgraph,
        metrics: &AnalysisMetrics,
        config: &RuleConfig,
    ) -> Vec<Diagnostic> {
        if config.enabled == Some(false) {
            return Vec::new();
        }

        let severity = config.severity.unwrap_or(self.default_severity);
        let graph = PatternModuleGraph::build(cpg);

        match self.pattern_type {
            PatternType::GodUnit => self.detect_god_units(cpg, metrics, &graph, severity),
            PatternType::FeatureEnvy => self.detect_feature_envy(cpg, &graph, severity),
            PatternType::CircularDependency => {
                self.detect_circular_dependencies(cpg, &graph, severity)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MetricObservation {
    pub metric_id: MetricId,
    pub raw_value: f64,
    pub normalized_risk: f64,
    pub threshold: f64,
    pub overflow_ratio: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatternEvidence {
    pub pattern_type: PatternType,
    pub evidence_scopes: Vec<ScopeId>,
    pub evidence_message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateSuggestion {
    pub explanation: String,
    pub code_example: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LlmSuggestionBundle {
    pub enrichments: BTreeMap<DiagnosticId, LlmSuggestion>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LlmSuggestion {
    pub explanation: String,
    pub code_example: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceExcerpt {
    pub file_path: FilePath,
    pub start_line: u32,
    pub end_line: u32,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpgSubgraphExcerpt {
    pub scopes: Vec<ScopeId>,
    pub representation: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiagnosticKind {
    Metric,
    Pattern,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileLocation {
    pub file_path: FilePath,
    pub start_line: u32,
    pub end_line: u32,
    pub column: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiagnosticSummary {
    pub error_count: u32,
    pub warning_count: u32,
    pub info_count: u32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiagnosticsScope {
    WholeProject,
    AffectedOnly,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SummaryScope {
    ListedDiagnostics,
    WholeProject,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExitCode {
    Success,
    DiagnosticFailure,
    ToolError,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PatternType {
    GodUnit,
    FeatureEnvy,
    CircularDependency,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuleConfig {
    pub enabled: Option<bool>,
    pub threshold: Option<f64>,
    pub severity: Option<Severity>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineSuppression {
    pub location: FileLocation,
    pub rule_id: Option<RuleId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LlmContext {
    pub rule_id: RuleId,
    pub severity: Severity,
    pub language: Language,
    pub workspace_relative_path: FilePath,
    pub source_excerpt: Option<SourceExcerpt>,
    pub cpg_excerpt: Option<CpgSubgraphExcerpt>,
}

pub fn project_subgraph(cpg: &UnifiedCpg) -> CpgSubgraph {
    cpg.subgraph(&ScopeId::new(
        AnalysisLevel::Project,
        "<project>",
        FilePath::from("."),
    ))
}

pub fn builtin_metric_rules() -> Vec<MetricRule> {
    const RULES: [(&str, &str, &str, &str); 10] = [
        (
            "KAL-F001",
            "M-F001",
            "Flags functions whose branching entropy suggests too many competing control-flow paths.",
            "Reduce branching entropy by extracting condition-heavy branches into helper functions or replacing nested conditionals with table-driven dispatch.",
        ),
        (
            "KAL-F002",
            "M-F002",
            "Flags functions whose cyclomatic complexity has drifted beyond the Wave 3 threshold.",
            "Lower cyclomatic complexity by splitting independent branches into smaller functions and simplifying guard logic.",
        ),
        (
            "KAL-F003",
            "M-F003",
            "Flags functions whose dense internal data flow makes behavior harder to follow.",
            "Reduce data-flow density by narrowing variable lifetimes, introducing clearer intermediate abstractions, and isolating transformation steps.",
        ),
        (
            "KAL-F004",
            "M-F004",
            "Flags functions whose identifier vocabulary is repetitive enough to hide intent.",
            "Rename repeated identifiers to reflect distinct roles and extract repeated sub-concepts into named helper types or functions.",
        ),
        (
            "KAL-M001",
            "M-M001",
            "Flags modules that depend on too many other modules.",
            "Trim fan-out by moving unstable integrations behind a façade and separating unrelated responsibilities into smaller modules.",
        ),
        (
            "KAL-M002",
            "M-M002",
            "Flags modules that participate in dependency cycles.",
            "Break module cycles by introducing an interface boundary, inversion point, or shared lower-level abstraction.",
        ),
        (
            "KAL-M003",
            "M-M003",
            "Flags modules whose outgoing dependencies dominate their stability profile.",
            "Stabilize the module by reducing outward dependencies or moving orchestration code to a higher-level coordinator.",
        ),
        (
            "KAL-P001",
            "M-P001",
            "Flags projects whose module dependency graph contains too many cyclic edges.",
            "Reduce cyclic coupling by untangling dependency directions and introducing clearer layering between modules.",
        ),
        (
            "KAL-P002",
            "M-P002",
            "Flags projects whose code volume is overly concentrated in a small set of modules.",
            "Rebalance module size by carving oversized modules into cohesive slices with explicit ownership boundaries.",
        ),
        (
            "KAL-P003",
            "M-P003",
            "Flags projects whose inbound dependencies are concentrated around a small number of hubs.",
            "Reduce hub dependency concentration by spreading responsibilities across narrower modules and adding stable extension seams.",
        ),
    ];

    RULES
        .into_iter()
        .map(
            |(rule_id, metric_id, description, suggestion_template)| MetricRule {
                id: RuleId::from(rule_id),
                metric_id: MetricId::from(metric_id),
                default_threshold: default_metric_threshold(rule_id),
                description: description.to_owned(),
                suggestion_template: suggestion_template.to_owned(),
            },
        )
        .collect()
}

pub fn builtin_pattern_rules() -> Vec<PatternRule> {
    vec![
        PatternRule {
            id: RuleId::from("KAL-PAT001"),
            pattern_type: PatternType::GodUnit,
            evaluation_scope: AnalysisLevel::Module,
            default_severity: default_pattern_severity("KAL-PAT001"),
            description:
                "Detects owner scopes that simultaneously expose many members, depend broadly on peers, and contain complex functions."
                    .to_owned(),
            suggestion_template:
                "Split the god unit along cohesive responsibilities, reduce its outward dependencies, and move complex routines behind narrower interfaces."
                    .to_owned(),
        },
        PatternRule {
            id: RuleId::from("KAL-PAT002"),
            pattern_type: PatternType::FeatureEnvy,
            evaluation_scope: AnalysisLevel::Function,
            default_severity: default_pattern_severity("KAL-PAT002"),
            description:
                "Detects functions that spend most of their calls or type references reaching into other modules."
                    .to_owned(),
            suggestion_template:
                "Move the behavior closer to the data it depends on, or introduce a local façade so the function stops reaching across module boundaries."
                    .to_owned(),
        },
        PatternRule {
            id: RuleId::from("KAL-PAT003"),
            pattern_type: PatternType::CircularDependency,
            evaluation_scope: AnalysisLevel::Project,
            default_severity: default_pattern_severity("KAL-PAT003"),
            description:
                "Detects strongly connected components in the module dependency graph."
                    .to_owned(),
            suggestion_template:
                "Break the cycle by inverting one dependency edge or extracting the shared contract into a lower-level module."
                    .to_owned(),
        },
    ]
}

pub fn apply_suppressions(
    diagnostics: Vec<Diagnostic>,
    suppressions: &[InlineSuppression],
) -> Vec<Diagnostic> {
    diagnostics
        .into_iter()
        .filter(|diagnostic| !is_suppressed(diagnostic, suppressions))
        .collect()
}

const RUST_DERIVE_METHOD_NAMES: &[&str] =
    &["clone", "eq", "ne", "fmt", "hash", "partial_cmp", "cmp"];

impl PatternRule {
    fn detect_god_units(
        &self,
        cpg: &CpgSubgraph,
        metrics: &AnalysisMetrics,
        graph: &PatternModuleGraph<'_>,
        severity: Severity,
    ) -> Vec<Diagnostic> {
        let complexity_by_function = function_metric_lookup(metrics, "M-F002");
        let candidates = if cpg.scope_id.level == AnalysisLevel::Module {
            graph
                .module_for_scope(&cpg.scope_id)
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            graph.modules.clone()
        };

        candidates
            .into_iter()
            .filter_map(|module| {
                let public_member_count = graph
                    .direct_function_children(module.id)
                    .into_iter()
                    .filter(|function| !function.name.starts_with('_'))
                    .count();
                let fan_out = graph.fan_out(module.id);
                let function_scores = graph
                    .owned_functions(module.id)
                    .into_iter()
                    .filter_map(|function| {
                        complexity_by_function
                            .get(&(function.name.clone(), function.location.file_path.clone()))
                            .copied()
                    })
                    .collect::<Vec<_>>();
                let complexity_average = if function_scores.is_empty() {
                    0.0
                } else {
                    round_half_up(
                        function_scores.iter().sum::<f64>() / function_scores.len() as f64,
                        6,
                    )
                };

                if public_member_count < 20 || fan_out < 8 || complexity_average < 0.50 {
                    return None;
                }

                let scope_id = scope_id_for_node(module, AnalysisLevel::Module);
                Some(Diagnostic {
                    id: DiagnosticId::from(format!("{}-{}", self.id, scope_id.qualified_name)),
                    primary_scope_id: scope_id.clone(),
                    rule_id: self.id.clone(),
                    kind: DiagnosticKind::Pattern,
                    severity,
                    location: file_location_from_node(module),
                    message: format!(
                        "God Unit detected in `{}`: public members={}, fan-out={}, average M-F002={:.6}",
                        scope_id.qualified_name, public_member_count, fan_out, complexity_average
                    ),
                    metric: None,
                    pattern: Some(PatternEvidence {
                        pattern_type: self.pattern_type,
                        evidence_scopes: vec![scope_id.clone()],
                        evidence_message: format!(
                            "public_member_count={}, fan_out={}, average M-F002={:.6}",
                            public_member_count, fan_out, complexity_average
                        ),
                    }),
                    template_suggestion: template_suggestion(&self.suggestion_template),
                })
            })
            .collect()
    }

    fn detect_feature_envy(
        &self,
        cpg: &CpgSubgraph,
        graph: &PatternModuleGraph<'_>,
        severity: Severity,
    ) -> Vec<Diagnostic> {
        let candidates = if cpg.scope_id.level == AnalysisLevel::Function {
            graph
                .function_for_scope(&cpg.scope_id)
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            graph.functions.clone()
        };

        candidates
            .into_iter()
            .filter_map(|function| {
                // Skip Rust derive-generated methods (e.g. Clone::clone, PartialEq::eq).
                if function.extension.as_ref().is_some_and(|ext| {
                    ext.language == Language::Rust
                        && RUST_DERIVE_METHOD_NAMES.contains(&function.name.as_str())
                }) {
                    return None;
                }

                let owner_modules = graph.ownership.get(&function.id)?;
                let mut foreign_accesses = 0_u32;
                let mut local_accesses = 0_u32;

                for edge in cpg.edges.iter().filter(|edge| {
                    edge.source == function.id
                        && matches!(edge.kind, EdgeKind::Call | EdgeKind::TypeReference)
                }) {
                    let Some(target_modules) = graph.ownership.get(&edge.target) else {
                        continue;
                    };

                    if owner_modules.iter().any(|module_id| target_modules.contains(module_id)) {
                        local_accesses += 1;
                    } else {
                        foreign_accesses += 1;
                    }
                }

                let total_accesses = foreign_accesses + local_accesses;
                if total_accesses == 0 {
                    return None;
                }

                let foreign_ratio =
                    round_half_up(f64::from(foreign_accesses) / f64::from(total_accesses), 6);
                if foreign_accesses < 5 || foreign_ratio < 0.70 {
                    return None;
                }

                let scope_id = scope_id_for_node(function, AnalysisLevel::Function);
                Some(Diagnostic {
                    id: DiagnosticId::from(format!("{}-{}", self.id, scope_id.qualified_name)),
                    primary_scope_id: scope_id.clone(),
                    rule_id: self.id.clone(),
                    kind: DiagnosticKind::Pattern,
                    severity,
                    location: file_location_from_node(function),
                    message: format!(
                        "Feature Envy detected in `{}`: foreign accesses={}, local accesses={}, foreign ratio={:.6}",
                        scope_id.qualified_name, foreign_accesses, local_accesses, foreign_ratio
                    ),
                    metric: None,
                    pattern: Some(PatternEvidence {
                        pattern_type: self.pattern_type,
                        evidence_scopes: vec![scope_id.clone()],
                        evidence_message: format!(
                            "foreign_accesses={}, local_accesses={}, foreign_ratio={:.6}",
                            foreign_accesses, local_accesses, foreign_ratio
                        ),
                    }),
                    template_suggestion: template_suggestion(&self.suggestion_template),
                })
            })
            .collect()
    }

    fn detect_circular_dependencies(
        &self,
        cpg: &CpgSubgraph,
        graph: &PatternModuleGraph<'_>,
        severity: Severity,
    ) -> Vec<Diagnostic> {
        if cpg.scope_id.level != AnalysisLevel::Project {
            return Vec::new();
        }

        graph
            .non_trivial_sccs()
            .into_iter()
            .map(|component| {
                let evidence_scopes = component
                    .into_iter()
                    .map(|module| scope_id_for_node(module, AnalysisLevel::Module))
                    .collect::<Vec<_>>();
                build_cross_scope_pattern_diagnostic(
                    self,
                    severity,
                    evidence_scopes,
                    "Circular dependency detected among modules".to_owned(),
                )
            })
            .collect()
    }
}

fn default_metric_threshold(rule_id: &str) -> f64 {
    DEFAULT_METRIC_RULE_THRESHOLDS
        .iter()
        .find_map(|(candidate, threshold)| (*candidate == rule_id).then_some(*threshold))
        .unwrap_or_else(|| panic!("missing default threshold for rule `{rule_id}`"))
}

fn default_pattern_severity(rule_id: &str) -> Severity {
    DEFAULT_PATTERN_RULE_SEVERITIES
        .iter()
        .find_map(|(candidate, severity)| (*candidate == rule_id).then_some(*severity))
        .unwrap_or_else(|| panic!("missing default severity for rule `{rule_id}`"))
}

fn template_suggestion(template: &str) -> TemplateSuggestion {
    TemplateSuggestion {
        explanation: template.to_owned(),
        code_example: None,
    }
}

fn function_metric_lookup(
    metrics: &AnalysisMetrics,
    metric_id: &str,
) -> BTreeMap<(String, FilePath), f64> {
    metrics
        .function_metrics
        .iter()
        .filter_map(|scope_metrics| {
            scope_metrics
                .values
                .iter()
                .find(|value| value.metric_id.as_str() == metric_id)
                .map(|value| {
                    (
                        (
                            scope_metrics.scope_id.qualified_name.clone(),
                            scope_metrics.scope_id.file_path.clone(),
                        ),
                        value.normalized_risk,
                    )
                })
        })
        .collect()
}

fn scope_id_for_node(node: &CpgNode, level: AnalysisLevel) -> ScopeId {
    ScopeId::new(level, node.name.clone(), node.location.file_path.clone())
}

fn file_location_from_node(node: &CpgNode) -> FileLocation {
    FileLocation {
        file_path: node.location.file_path.clone(),
        start_line: node.location.start_line,
        end_line: node.location.end_line,
        column: None,
    }
}

fn build_cross_scope_pattern_diagnostic(
    rule: &PatternRule,
    severity: Severity,
    mut evidence_scopes: Vec<ScopeId>,
    summary: String,
) -> Diagnostic {
    evidence_scopes.sort();
    let primary_scope_id = evidence_scopes
        .first()
        .cloned()
        .expect("cross-scope pattern requires at least one scope");
    let representative_file_path = evidence_scopes
        .iter()
        .map(|scope| scope.file_path.clone())
        .min()
        .expect("cross-scope pattern requires at least one scope");
    let scope_names = evidence_scopes
        .iter()
        .map(|scope| scope.qualified_name.clone())
        .collect::<Vec<_>>();
    let cycle_description = scope_names.join(" -> ");

    Diagnostic {
        id: DiagnosticId::from(format!("{}-{}", rule.id, primary_scope_id.qualified_name)),
        primary_scope_id,
        rule_id: rule.id.clone(),
        kind: DiagnosticKind::Pattern,
        severity,
        location: FileLocation {
            file_path: representative_file_path,
            start_line: 1,
            end_line: 1,
            column: None,
        },
        message: format!("{summary}: {cycle_description}"),
        metric: None,
        pattern: Some(PatternEvidence {
            pattern_type: rule.pattern_type,
            evidence_scopes,
            evidence_message: cycle_description,
        }),
        template_suggestion: template_suggestion(&rule.suggestion_template),
    }
}

fn is_suppressed(diagnostic: &Diagnostic, suppressions: &[InlineSuppression]) -> bool {
    if diagnostic
        .pattern
        .as_ref()
        .is_some_and(|pattern| pattern.evidence_scopes.len() > 1)
    {
        return false;
    }

    suppressions.iter().any(|suppression| {
        suppression.location.file_path == diagnostic.location.file_path
            && suppression.location.start_line == diagnostic.location.start_line
            && suppression
                .rule_id
                .as_ref()
                .is_none_or(|rule_id| *rule_id == diagnostic.rule_id)
    })
}

struct PatternModuleGraph<'a> {
    modules: Vec<&'a CpgNode>,
    functions: Vec<&'a CpgNode>,
    node_by_id: BTreeMap<NodeId, &'a CpgNode>,
    contains_graph: BTreeMap<NodeId, Vec<NodeId>>,
    ownership: BTreeMap<NodeId, BTreeSet<NodeId>>,
    outgoing: BTreeMap<NodeId, BTreeSet<NodeId>>,
    sccs: Vec<Vec<&'a CpgNode>>,
}

impl<'a> PatternModuleGraph<'a> {
    fn build(cpg: &'a CpgSubgraph) -> Self {
        let modules = cpg
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Module)
            .collect::<Vec<_>>();
        let functions = cpg
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Function)
            .collect::<Vec<_>>();
        let node_by_id = cpg
            .nodes
            .iter()
            .map(|node| (node.id, node))
            .collect::<BTreeMap<_, _>>();
        let contains_graph = cpg
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Contains)
            .fold(BTreeMap::<NodeId, Vec<NodeId>>::new(), |mut acc, edge| {
                acc.entry(edge.source).or_default().push(edge.target);
                acc
            });
        let ownership = build_module_ownership(&modules, &contains_graph);
        let mut outgoing = BTreeMap::<NodeId, BTreeSet<NodeId>>::new();
        for module in &modules {
            outgoing.entry(module.id).or_default();
        }

        for edge in cpg.edges.iter().filter(|edge| {
            matches!(
                edge.kind,
                EdgeKind::Call | EdgeKind::Contains | EdgeKind::TypeReference
            )
        }) {
            let Some(source_modules) = ownership.get(&edge.source) else {
                continue;
            };
            let Some(target_modules) = ownership.get(&edge.target) else {
                continue;
            };

            for source_module in source_modules {
                for target_module in target_modules {
                    if source_module != target_module {
                        outgoing
                            .entry(*source_module)
                            .or_default()
                            .insert(*target_module);
                    }
                }
            }
        }

        let sccs = build_non_trivial_sccs(&modules, &outgoing);

        Self {
            modules,
            functions,
            node_by_id,
            contains_graph,
            ownership,
            outgoing,
            sccs,
        }
    }

    fn module_for_scope(&self, scope_id: &ScopeId) -> Option<&'a CpgNode> {
        match_node_for_scope(&self.modules, scope_id)
    }

    fn function_for_scope(&self, scope_id: &ScopeId) -> Option<&'a CpgNode> {
        match_node_for_scope(&self.functions, scope_id)
    }

    fn direct_function_children(&self, module_id: NodeId) -> Vec<&'a CpgNode> {
        self.contains_graph
            .get(&module_id)
            .into_iter()
            .flatten()
            .filter_map(|node_id| self.node_by_id.get(node_id).copied())
            .filter(|node| node.kind == NodeKind::Function)
            .collect()
    }

    fn owned_functions(&self, module_id: NodeId) -> Vec<&'a CpgNode> {
        self.functions
            .iter()
            .copied()
            .filter(|function| {
                self.ownership
                    .get(&function.id)
                    .is_some_and(|owners| owners.contains(&module_id))
            })
            .collect()
    }

    fn fan_out(&self, module_id: NodeId) -> usize {
        self.outgoing.get(&module_id).map_or(0, BTreeSet::len)
    }

    fn non_trivial_sccs(&self) -> Vec<Vec<&'a CpgNode>> {
        self.sccs.clone()
    }
}

fn match_node_for_scope<'a>(nodes: &[&'a CpgNode], scope_id: &ScopeId) -> Option<&'a CpgNode> {
    nodes
        .iter()
        .copied()
        .find(|node| {
            node.name == scope_id.qualified_name && node.location.file_path == scope_id.file_path
        })
        .or_else(|| {
            nodes
                .iter()
                .copied()
                .find(|node| node.name == scope_id.qualified_name)
        })
        .or_else(|| {
            nodes
                .iter()
                .copied()
                .find(|node| node.location.file_path == scope_id.file_path)
        })
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

fn build_non_trivial_sccs<'a>(
    modules: &[&'a CpgNode],
    outgoing: &BTreeMap<NodeId, BTreeSet<NodeId>>,
) -> Vec<Vec<&'a CpgNode>> {
    let mut graph = DiGraph::<NodeId, ()>::new();
    let mut graph_nodes = BTreeMap::<NodeId, _>::new();
    let module_by_id = modules
        .iter()
        .copied()
        .map(|module| (module.id, module))
        .collect::<BTreeMap<_, _>>();

    for module in modules {
        graph_nodes.insert(module.id, graph.add_node(module.id));
    }
    for (source, targets) in outgoing {
        for target in targets {
            if let (Some(source_index), Some(target_index)) =
                (graph_nodes.get(source), graph_nodes.get(target))
            {
                graph.add_edge(*source_index, *target_index, ());
            }
        }
    }

    let mut components = kosaraju_scc(&graph)
        .into_iter()
        .filter(|component| component.len() >= 2)
        .map(|component| {
            let mut scopes = component
                .into_iter()
                .filter_map(|node_index| module_by_id.get(&graph[node_index]).copied())
                .collect::<Vec<_>>();
            scopes.sort_by_key(|module| {
                (
                    module.name.clone(),
                    module.location.file_path.clone(),
                    module.location.start_line,
                )
            });
            scopes
        })
        .collect::<Vec<_>>();

    components.sort_by_key(|component| {
        component
            .first()
            .map(|module| {
                (
                    module.name.clone(),
                    module.location.file_path.clone(),
                    module.location.start_line,
                )
            })
            .expect("non-trivial SCC must contain at least one module")
    });
    components
}

#[cfg(test)]
mod tests {
    use crate::domains::metrics::test_fixtures::CpgBuilder;
    use crate::domains::metrics::{MetricValue, OverallScore, ScopeMetrics};

    use super::{
        AnalysisLevel, AnalysisMetrics, Diagnostic, DiagnosticKind, DiagnosticReport,
        DiagnosticSummary, DiagnosticsScope, ExitCode, FileLocation, InlineSuppression,
        MetricObservation, MetricRule, PatternEvidence, PatternType, RuleConfig, Severity,
        SummaryScope, apply_suppressions, builtin_metric_rules, builtin_pattern_rules,
        project_subgraph,
    };
    use crate::domains::cpg::{CpgId, EdgeKind, Language, LanguageExtension, UnifiedCpg};
    use crate::domains::{DiagnosticId, FilePath, MetricId, RuleId, ScopeId};

    #[test]
    fn determine_exit_code_uses_summary_and_strict_mode() {
        let report = DiagnosticReport {
            diagnostics: Vec::new(),
            summary: DiagnosticSummary {
                error_count: 0,
                warning_count: 1,
                info_count: 0,
            },
            diagnostics_scope: DiagnosticsScope::WholeProject,
            summary_scope: SummaryScope::ListedDiagnostics,
        };

        assert_eq!(report.determine_exit_code(false), ExitCode::Success);
        assert_eq!(
            report.determine_exit_code(true),
            ExitCode::DiagnosticFailure
        );
    }

    #[test]
    fn metric_rule_evaluate_builds_diagnostic_with_default_severity_and_rounding() {
        let rule = MetricRule {
            id: RuleId::from("KAL-F002"),
            metric_id: MetricId::from("M-F002"),
            default_threshold: 0.60,
            description: "Cyclomatic complexity risk".to_owned(),
            suggestion_template: "Split complex branches.".to_owned(),
        };
        let scope_id = ScopeId::new(AnalysisLevel::Function, "crate::f", "src/lib.rs");
        let diagnostic = rule
            .evaluate(
                &scope_id,
                &FileLocation {
                    file_path: FilePath::from("ignored.rs"),
                    start_line: 12,
                    end_line: 18,
                    column: Some(3),
                },
                &MetricObservation {
                    metric_id: MetricId::from("M-F002"),
                    raw_value: 11.0,
                    normalized_risk: 0.7000005,
                    threshold: 0.0,
                    overflow_ratio: 0.0,
                },
                &RuleConfig {
                    enabled: Some(true),
                    threshold: Some(0.55),
                    severity: None,
                },
            )
            .expect("threshold violation should emit a diagnostic");

        assert_eq!(diagnostic.id, DiagnosticId::from("KAL-F002-crate::f"));
        assert_eq!(diagnostic.kind, DiagnosticKind::Metric);
        assert_eq!(diagnostic.severity, Severity::Warning);
        assert_eq!(
            diagnostic.location,
            FileLocation {
                file_path: FilePath::from("src/lib.rs"),
                start_line: 12,
                end_line: 18,
                column: Some(3),
            }
        );
        assert!(diagnostic.message.contains("M-F002"));
        assert_eq!(diagnostic.metric.as_ref().unwrap().threshold, 0.55);
        assert_eq!(diagnostic.metric.as_ref().unwrap().overflow_ratio, 0.333334);
        assert_eq!(
            diagnostic.template_suggestion.explanation,
            "Split complex branches."
        );
    }

    #[test]
    fn metric_rule_evaluate_respects_disable_and_severity_override() {
        let rule = MetricRule {
            id: RuleId::from("KAL-F001"),
            metric_id: MetricId::from("M-F001"),
            default_threshold: 0.55,
            description: "Branch entropy risk".to_owned(),
            suggestion_template: "Extract branches.".to_owned(),
        };
        let scope_id = ScopeId::new(AnalysisLevel::Function, "crate::f", "src/lib.rs");
        let location = FileLocation {
            file_path: FilePath::from("src/lib.rs"),
            start_line: 1,
            end_line: 1,
            column: None,
        };
        let observation = MetricObservation {
            metric_id: MetricId::from("M-F001"),
            raw_value: 0.8,
            normalized_risk: 0.95,
            threshold: 0.0,
            overflow_ratio: 0.0,
        };

        assert!(
            rule.evaluate(
                &scope_id,
                &location,
                &observation,
                &RuleConfig {
                    enabled: Some(false),
                    threshold: None,
                    severity: None,
                },
            )
            .is_none()
        );

        let diagnostic = rule
            .evaluate(
                &scope_id,
                &location,
                &observation,
                &RuleConfig {
                    enabled: Some(true),
                    threshold: None,
                    severity: Some(Severity::Info),
                },
            )
            .expect("override severity should still emit a diagnostic");

        assert_eq!(diagnostic.severity, Severity::Info);
    }

    #[test]
    fn builtin_rule_catalogs_cover_wave3_rules() {
        let metric_rules = builtin_metric_rules();
        let pattern_rules = builtin_pattern_rules();

        assert_eq!(metric_rules.len(), 10);
        assert_eq!(pattern_rules.len(), 3);
        assert_eq!(
            metric_rules
                .iter()
                .find(|rule| rule.id == RuleId::from("KAL-M002"))
                .map(|rule| rule.default_threshold),
            Some(0.20)
        );
        assert_eq!(
            pattern_rules
                .iter()
                .find(|rule| rule.id == RuleId::from("KAL-PAT003"))
                .map(|rule| rule.default_severity),
            Some(Severity::Error)
        );
    }

    #[test]
    fn god_unit_detection_uses_members_fan_out_and_function_complexity() {
        let cpg = god_unit_cpg();
        let metrics = AnalysisMetrics {
            function_metrics: (1..=20)
                .map(|index| {
                    ScopeMetrics::new(
                        ScopeId::new(
                            AnalysisLevel::Function,
                            format!("crate::god::member_{index}"),
                            "src/god.rs",
                        ),
                        vec![MetricValue {
                            metric_id: MetricId::from("M-F002"),
                            raw_value: 12.0,
                            normalized_risk: 0.6,
                        }],
                    )
                })
                .collect(),
            module_metrics: Vec::new(),
            project_metrics: None,
            overall_score: zero_overall_score(),
        };

        let diagnostic = builtin_pattern_rules()
            .into_iter()
            .find(|rule| rule.id == RuleId::from("KAL-PAT001"))
            .unwrap()
            .detect(
                &project_subgraph(&cpg),
                &metrics,
                &RuleConfig {
                    enabled: Some(true),
                    threshold: None,
                    severity: None,
                },
            )
            .pop()
            .expect("god unit should be detected");

        assert_eq!(diagnostic.rule_id, RuleId::from("KAL-PAT001"));
        assert_eq!(diagnostic.severity, Severity::Warning);
        assert_eq!(
            diagnostic.primary_scope_id,
            ScopeId::new(AnalysisLevel::Module, "crate::god", "src/god.rs")
        );
        assert!(diagnostic.message.contains("public members=20"));
        assert!(diagnostic.message.contains("fan-out=8"));
        assert!(
            diagnostic
                .template_suggestion
                .explanation
                .contains("Split the god unit")
        );
    }

    #[test]
    fn feature_envy_detection_reports_only_high_foreign_ratio_functions() {
        let subgraph = CpgBuilder::new()
            .module_at("module_a", "crate::a", "src/a.rs", 1, 40)
            .module_at("module_b", "crate::b", "src/b.rs", 1, 20)
            .function_at("envy", "crate::a::envy", "src/a.rs", 3, 12)
            .function_at("local", "crate::a::local", "src/a.rs", 14, 18)
            .function_at("balanced", "crate::a::balanced", "src/a.rs", 20, 30)
            .function_at("b1", "crate::b::f1", "src/b.rs", 3, 3)
            .function_at("b2", "crate::b::f2", "src/b.rs", 4, 4)
            .function_at("b3", "crate::b::f3", "src/b.rs", 5, 5)
            .function_at("b4", "crate::b::f4", "src/b.rs", 6, 6)
            .function_at("b5", "crate::b::f5", "src/b.rs", 7, 7)
            .edge("module_a", "envy", EdgeKind::Contains)
            .edge("module_a", "local", EdgeKind::Contains)
            .edge("module_a", "balanced", EdgeKind::Contains)
            .edge("module_b", "b1", EdgeKind::Contains)
            .edge("module_b", "b2", EdgeKind::Contains)
            .edge("module_b", "b3", EdgeKind::Contains)
            .edge("module_b", "b4", EdgeKind::Contains)
            .edge("module_b", "b5", EdgeKind::Contains)
            .edge("envy", "b1", EdgeKind::Call)
            .edge("envy", "b2", EdgeKind::Call)
            .edge("envy", "b3", EdgeKind::TypeReference)
            .edge("envy", "b4", EdgeKind::Call)
            .edge("envy", "b5", EdgeKind::TypeReference)
            .edge("envy", "local", EdgeKind::Call)
            .edge("balanced", "b1", EdgeKind::Call)
            .edge("balanced", "b2", EdgeKind::Call)
            .edge("balanced", "b3", EdgeKind::Call)
            .edge("balanced", "local", EdgeKind::Call)
            .edge("balanced", "envy", EdgeKind::Call)
            .build(ScopeId::new(AnalysisLevel::Project, "<project>", "."));
        let diagnostics = builtin_pattern_rules()
            .into_iter()
            .find(|rule| rule.id == RuleId::from("KAL-PAT002"))
            .unwrap()
            .detect(
                &subgraph,
                &empty_metrics(),
                &RuleConfig {
                    enabled: Some(true),
                    threshold: None,
                    severity: None,
                },
            );

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].primary_scope_id.qualified_name,
            "crate::a::envy"
        );
        assert!(diagnostics[0].message.contains("foreign accesses=5"));
        assert!(diagnostics[0].message.contains("foreign ratio=0.833333"));
        assert!(
            diagnostics[0]
                .template_suggestion
                .explanation
                .contains("Move the behavior")
        );
    }

    #[test]
    fn feature_envy_skips_rust_derive_methods() {
        let mut subgraph = CpgBuilder::new()
            .module_at("module_a", "crate::a", "src/a.rs", 1, 40)
            .module_at("module_b", "crate::b", "src/b.rs", 1, 20)
            .function_at("clone", "clone", "src/a.rs", 3, 12)
            .function_at("local", "local", "src/a.rs", 14, 18)
            .function_at("b1", "crate::b::f1", "src/b.rs", 3, 3)
            .function_at("b2", "crate::b::f2", "src/b.rs", 4, 4)
            .function_at("b3", "crate::b::f3", "src/b.rs", 5, 5)
            .function_at("b4", "crate::b::f4", "src/b.rs", 6, 6)
            .function_at("b5", "crate::b::f5", "src/b.rs", 7, 7)
            .edge("module_a", "clone", EdgeKind::Contains)
            .edge("module_a", "local", EdgeKind::Contains)
            .edge("module_b", "b1", EdgeKind::Contains)
            .edge("module_b", "b2", EdgeKind::Contains)
            .edge("module_b", "b3", EdgeKind::Contains)
            .edge("module_b", "b4", EdgeKind::Contains)
            .edge("module_b", "b5", EdgeKind::Contains)
            .edge("clone", "b1", EdgeKind::Call)
            .edge("clone", "b2", EdgeKind::Call)
            .edge("clone", "b3", EdgeKind::TypeReference)
            .edge("clone", "b4", EdgeKind::Call)
            .edge("clone", "b5", EdgeKind::TypeReference)
            .edge("clone", "local", EdgeKind::Call)
            .build(ScopeId::new(AnalysisLevel::Project, "<project>", "."));

        let clone = subgraph
            .nodes
            .iter_mut()
            .find(|node| node.name == "clone")
            .expect("clone function should exist");
        clone.extension = Some(LanguageExtension {
            language: Language::Rust,
            properties: std::collections::BTreeMap::new(),
        });

        let diagnostics = builtin_pattern_rules()
            .into_iter()
            .find(|rule| rule.id == RuleId::from("KAL-PAT002"))
            .unwrap()
            .detect(
                &subgraph,
                &empty_metrics(),
                &RuleConfig {
                    enabled: Some(true),
                    threshold: None,
                    severity: None,
                },
            );

        assert!(diagnostics.is_empty());
    }

    #[test]
    fn circular_dependency_detection_returns_one_diagnostic_per_scc() {
        let cpg = circular_dependency_cpg();
        let diagnostics = builtin_pattern_rules()
            .into_iter()
            .find(|rule| rule.id == RuleId::from("KAL-PAT003"))
            .unwrap()
            .detect(
                &project_subgraph(&cpg),
                &empty_metrics(),
                &RuleConfig {
                    enabled: Some(true),
                    threshold: None,
                    severity: None,
                },
            );

        assert_eq!(diagnostics.len(), 2);
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.severity == Severity::Error)
        );
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.location.start_line == 1)
        );
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.pattern.is_some())
        );
        assert_eq!(
            diagnostics[0]
                .pattern
                .as_ref()
                .unwrap()
                .evidence_scopes
                .len(),
            2
        );
    }

    #[test]
    fn apply_suppressions_handles_targeted_blanket_and_cross_scope_cases() {
        let diagnostics = vec![
            Diagnostic {
                id: DiagnosticId::from("metric"),
                primary_scope_id: ScopeId::new(AnalysisLevel::Function, "crate::f", "src/lib.rs"),
                rule_id: RuleId::from("KAL-F001"),
                kind: DiagnosticKind::Metric,
                severity: Severity::Warning,
                location: FileLocation {
                    file_path: FilePath::from("src/lib.rs"),
                    start_line: 10,
                    end_line: 10,
                    column: None,
                },
                message: "metric".to_owned(),
                metric: None,
                pattern: None,
                template_suggestion: super::template_suggestion("metric"),
            },
            Diagnostic {
                id: DiagnosticId::from("pattern"),
                primary_scope_id: ScopeId::new(AnalysisLevel::Module, "crate::m", "src/lib.rs"),
                rule_id: RuleId::from("KAL-PAT001"),
                kind: DiagnosticKind::Pattern,
                severity: Severity::Warning,
                location: FileLocation {
                    file_path: FilePath::from("src/lib.rs"),
                    start_line: 20,
                    end_line: 20,
                    column: None,
                },
                message: "pattern".to_owned(),
                metric: None,
                pattern: Some(PatternEvidence {
                    pattern_type: PatternType::GodUnit,
                    evidence_scopes: vec![ScopeId::new(
                        AnalysisLevel::Module,
                        "crate::m",
                        "src/lib.rs",
                    )],
                    evidence_message: "evidence".to_owned(),
                }),
                template_suggestion: super::template_suggestion("pattern"),
            },
            Diagnostic {
                id: DiagnosticId::from("cycle"),
                primary_scope_id: ScopeId::new(AnalysisLevel::Module, "crate::a", "src/a.rs"),
                rule_id: RuleId::from("KAL-PAT003"),
                kind: DiagnosticKind::Pattern,
                severity: Severity::Error,
                location: FileLocation {
                    file_path: FilePath::from("src/a.rs"),
                    start_line: 1,
                    end_line: 1,
                    column: None,
                },
                message: "cycle".to_owned(),
                metric: None,
                pattern: Some(PatternEvidence {
                    pattern_type: PatternType::CircularDependency,
                    evidence_scopes: vec![
                        ScopeId::new(AnalysisLevel::Module, "crate::a", "src/a.rs"),
                        ScopeId::new(AnalysisLevel::Module, "crate::b", "src/b.rs"),
                    ],
                    evidence_message: "a -> b".to_owned(),
                }),
                template_suggestion: super::template_suggestion("cycle"),
            },
        ];
        let suppressions = vec![
            InlineSuppression {
                location: FileLocation {
                    file_path: FilePath::from("src/lib.rs"),
                    start_line: 10,
                    end_line: 10,
                    column: None,
                },
                rule_id: Some(RuleId::from("KAL-F001")),
            },
            InlineSuppression {
                location: FileLocation {
                    file_path: FilePath::from("src/lib.rs"),
                    start_line: 20,
                    end_line: 20,
                    column: None,
                },
                rule_id: None,
            },
            InlineSuppression {
                location: FileLocation {
                    file_path: FilePath::from("src/a.rs"),
                    start_line: 1,
                    end_line: 1,
                    column: None,
                },
                rule_id: None,
            },
        ];

        let remaining = apply_suppressions(diagnostics, &suppressions);

        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].rule_id, RuleId::from("KAL-PAT003"));
    }

    fn empty_metrics() -> AnalysisMetrics {
        AnalysisMetrics {
            function_metrics: Vec::new(),
            module_metrics: Vec::new(),
            project_metrics: None,
            overall_score: zero_overall_score(),
        }
    }

    fn zero_overall_score() -> OverallScore {
        OverallScore {
            function_risk: None,
            module_risk: None,
            project_risk: None,
            overall_risk: 0.0,
            overall_score: 0,
            function_score: None,
            module_score: None,
            project_score: None,
        }
    }

    fn god_unit_cpg() -> UnifiedCpg {
        let mut builder = CpgBuilder::new()
            .module_at("god", "crate::god", "src/god.rs", 1, 200)
            .module_at("dep1", "crate::dep1", "src/dep1.rs", 1, 20)
            .module_at("dep2", "crate::dep2", "src/dep2.rs", 1, 20)
            .module_at("dep3", "crate::dep3", "src/dep3.rs", 1, 20)
            .module_at("dep4", "crate::dep4", "src/dep4.rs", 1, 20)
            .module_at("dep5", "crate::dep5", "src/dep5.rs", 1, 20)
            .module_at("dep6", "crate::dep6", "src/dep6.rs", 1, 20)
            .module_at("dep7", "crate::dep7", "src/dep7.rs", 1, 20)
            .module_at("dep8", "crate::dep8", "src/dep8.rs", 1, 20);

        for index in 1..=20 {
            let member_alias = format!("member_{index}");
            let member_name = format!("crate::god::member_{index}");
            builder = builder
                .function_at(
                    &member_alias,
                    &member_name,
                    "src/god.rs",
                    index + 1,
                    index + 1,
                )
                .edge("god", &member_alias, EdgeKind::Contains);
        }

        for index in 1..=8 {
            let dependency_alias = format!("dep{index}");
            let function_alias = format!("dep_fn_{index}");
            let function_name = format!("crate::dep{index}::f");
            let member_alias = format!("member_{index}");
            builder = builder
                .function_at(
                    &function_alias,
                    &function_name,
                    &format!("src/dep{index}.rs"),
                    2,
                    2,
                )
                .edge(&dependency_alias, &function_alias, EdgeKind::Contains)
                .edge(&member_alias, &function_alias, EdgeKind::Call);
        }

        let project = builder.build(ScopeId::new(AnalysisLevel::Project, "<project>", "."));
        UnifiedCpg {
            id: CpgId::from("god-unit"),
            nodes: project.nodes,
            edges: project.edges,
        }
    }

    fn circular_dependency_cpg() -> UnifiedCpg {
        let subgraph = CpgBuilder::new()
            .module_at("a", "crate::a", "src/a.rs", 1, 20)
            .module_at("b", "crate::b", "src/b.rs", 1, 20)
            .module_at("c", "crate::c", "src/c.rs", 1, 20)
            .module_at("d", "crate::d", "src/d.rs", 1, 20)
            .function_at("a_fn", "crate::a::f", "src/a.rs", 2, 2)
            .function_at("b_fn", "crate::b::f", "src/b.rs", 2, 2)
            .function_at("c_fn", "crate::c::f", "src/c.rs", 2, 2)
            .function_at("d_fn", "crate::d::f", "src/d.rs", 2, 2)
            .edge("a", "a_fn", EdgeKind::Contains)
            .edge("b", "b_fn", EdgeKind::Contains)
            .edge("c", "c_fn", EdgeKind::Contains)
            .edge("d", "d_fn", EdgeKind::Contains)
            .edge("a_fn", "b_fn", EdgeKind::Call)
            .edge("b_fn", "a_fn", EdgeKind::Call)
            .edge("c_fn", "d_fn", EdgeKind::Call)
            .edge("d_fn", "c_fn", EdgeKind::Call)
            .build(ScopeId::new(AnalysisLevel::Project, "<project>", "."));

        UnifiedCpg {
            id: CpgId::from("cycles"),
            nodes: subgraph.nodes,
            edges: subgraph.edges,
        }
    }
}
