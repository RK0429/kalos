use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::domains::cpg::{CpgNode, EdgeKind, NodeId, NodeKind, UnifiedCpg};
use crate::domains::diagnostics::DiagnosticSummary;
use crate::domains::metrics::{OverallScore, ScopeMetrics};

use super::{AnalysisLevel, DiagnosticId, FilePath, ScopeId};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiffBaseline {
    pub fingerprint: BaselineFingerprint,
    pub dependency_index: DependencyIndexManifest,
    pub scope_metrics: BTreeMap<ScopeId, ScopeMetrics>,
    pub diagnostic_snapshots: BTreeMap<ScopeId, ScopeDiagnosticSnapshot>,
    pub overall_score: OverallScore,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AffectedScopeSet {
    pub scopes: BTreeSet<ScopeId>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InvalidationPlan {
    pub recompute_scopes: BTreeSet<ScopeId>,
    pub reuse_scopes: BTreeSet<ScopeId>,
    pub fallback_to_full: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DependencyIndexManifest {
    pub reverse_dependencies: BTreeMap<ScopeId, BTreeSet<ScopeId>>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScopeDiagnosticSnapshot {
    pub scope_id: ScopeId,
    pub diagnostic_ids: Vec<DiagnosticId>,
    pub summary: DiagnosticSummary,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BaselineFingerprint {
    pub workspace_root_hash: String,
    pub base_snapshot_hash: String,
    pub config_hash: String,
    pub analysis_targets_hash: String,
    pub rule_catalog_version: String,
    pub extractor_version: String,
    pub kalos_version: String,
}

#[derive(Debug)]
pub struct ImpactAnalysisInput<'a> {
    pub diff_cpg: &'a UnifiedCpg,
    pub changed_files: &'a BTreeSet<FilePath>,
    pub baseline: Option<&'a DiffBaseline>,
    pub base_snapshot_hash: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImpactAnalysisOutput {
    pub affected_scopes: AffectedScopeSet,
    pub invalidation_plan: InvalidationPlan,
    pub merged_dependency_index: DependencyIndexManifest,
}

pub fn analyze_impact(input: &ImpactAnalysisInput) -> ImpactAnalysisOutput {
    let diff_scopes = known_scopes_from_cpg(input.diff_cpg);
    let diff_dependency_index = build_dependency_index_from_cpg(input.diff_cpg);
    let input_scopes = collect_input_scopes(input.baseline, &diff_scopes);
    let changed_scopes = input_scopes
        .into_iter()
        .filter(|scope| input.changed_files.contains(&scope.file_path))
        .collect::<BTreeSet<_>>();

    let Some(baseline) = input.baseline else {
        return fallback_output(diff_dependency_index);
    };

    if baseline.fingerprint.base_snapshot_hash != input.base_snapshot_hash
        || baseline.dependency_index.reverse_dependencies.is_empty()
    {
        return fallback_output(diff_dependency_index);
    }

    let merged_dependency_index = merge_dependency_indexes(
        &baseline.dependency_index,
        &diff_dependency_index,
        &changed_scopes,
    );
    let affected_scopes = AffectedScopeSet {
        scopes: reverse_transitive_closure(
            &merged_dependency_index.reverse_dependencies,
            &changed_scopes,
        ),
    };

    let mut all_known_scopes = collect_input_scopes(Some(baseline), &diff_scopes);
    all_known_scopes.extend(collect_manifest_scopes(&merged_dependency_index));
    all_known_scopes.insert(project_scope_id());

    let mut recompute_scopes = affected_scopes.scopes.clone();
    recompute_scopes.insert(project_scope_id());
    let reuse_scopes = all_known_scopes
        .difference(&recompute_scopes)
        .cloned()
        .collect::<BTreeSet<_>>();
    let invalidation_plan = InvalidationPlan {
        recompute_scopes,
        reuse_scopes,
        fallback_to_full: false,
    };

    if invalidation_plan_is_valid(&invalidation_plan, &affected_scopes, &all_known_scopes) {
        ImpactAnalysisOutput {
            affected_scopes,
            invalidation_plan,
            merged_dependency_index,
        }
    } else {
        ImpactAnalysisOutput {
            affected_scopes: AffectedScopeSet {
                scopes: BTreeSet::new(),
            },
            invalidation_plan: InvalidationPlan {
                recompute_scopes: BTreeSet::new(),
                reuse_scopes: BTreeSet::new(),
                fallback_to_full: true,
            },
            merged_dependency_index,
        }
    }
}

pub fn extract_scope_id_for_node(cpg: &UnifiedCpg, node_id: NodeId) -> Option<ScopeId> {
    let nodes = cpg
        .nodes
        .iter()
        .map(|node| (node.id, node))
        .collect::<BTreeMap<_, _>>();
    let parents = contains_parent_map(cpg);
    let mut queue = VecDeque::from([node_id]);
    let mut visited = BTreeSet::new();

    while let Some(current) = queue.pop_front() {
        if !visited.insert(current) {
            continue;
        }

        let node = nodes.get(&current)?;
        if let Some(scope_id) = scope_id_for_node(node) {
            return Some(scope_id);
        }

        if let Some(parent_ids) = parents.get(&current) {
            for parent_id in parent_ids {
                queue.push_back(*parent_id);
            }
        }
    }

    None
}

pub fn build_dependency_index_from_cpg(cpg: &UnifiedCpg) -> DependencyIndexManifest {
    let mut reverse_dependencies = known_scopes_from_cpg(cpg)
        .into_iter()
        .map(|scope_id| (scope_id, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();

    for edge in cpg
        .edges
        .iter()
        .filter(|edge| matches!(edge.kind, EdgeKind::Call | EdgeKind::Contains))
    {
        let Some(source_scope) = extract_scope_id_for_node(cpg, edge.source) else {
            continue;
        };
        let Some(target_scope) = extract_scope_id_for_node(cpg, edge.target) else {
            continue;
        };

        reverse_dependencies
            .entry(source_scope.clone())
            .or_default();
        reverse_dependencies
            .entry(target_scope.clone())
            .or_default();

        if source_scope != target_scope {
            reverse_dependencies
                .entry(target_scope)
                .or_default()
                .insert(source_scope);
        }
    }

    DependencyIndexManifest {
        reverse_dependencies,
    }
}

pub fn reverse_transitive_closure(
    reverse_deps: &BTreeMap<ScopeId, BTreeSet<ScopeId>>,
    seeds: &BTreeSet<ScopeId>,
) -> BTreeSet<ScopeId> {
    let mut visited = BTreeSet::new();
    let mut queue = seeds.iter().cloned().collect::<VecDeque<_>>();

    while let Some(scope_id) = queue.pop_front() {
        if !visited.insert(scope_id.clone()) {
            continue;
        }

        if let Some(dependents) = reverse_deps.get(&scope_id) {
            for dependent in dependents {
                if !visited.contains(dependent) {
                    queue.push_back(dependent.clone());
                }
            }
        }
    }

    visited
}

fn fallback_output(merged_dependency_index: DependencyIndexManifest) -> ImpactAnalysisOutput {
    ImpactAnalysisOutput {
        affected_scopes: AffectedScopeSet {
            scopes: BTreeSet::new(),
        },
        invalidation_plan: InvalidationPlan {
            recompute_scopes: BTreeSet::new(),
            reuse_scopes: BTreeSet::new(),
            fallback_to_full: true,
        },
        merged_dependency_index,
    }
}

fn merge_dependency_indexes(
    baseline: &DependencyIndexManifest,
    diff_index: &DependencyIndexManifest,
    changed_scopes: &BTreeSet<ScopeId>,
) -> DependencyIndexManifest {
    let mut reverse_dependencies = baseline.reverse_dependencies.clone();

    for dependents in reverse_dependencies.values_mut() {
        dependents.retain(|dependent| !changed_scopes.contains(dependent));
    }

    for scope_id in changed_scopes {
        reverse_dependencies.entry(scope_id.clone()).or_default();
    }

    for (dependency, dependents) in &diff_index.reverse_dependencies {
        let entry = reverse_dependencies.entry(dependency.clone()).or_default();
        for dependent in dependents {
            if changed_scopes.contains(dependent) {
                entry.insert(dependent.clone());
            }
        }
    }

    DependencyIndexManifest {
        reverse_dependencies,
    }
}

fn invalidation_plan_is_valid(
    invalidation_plan: &InvalidationPlan,
    affected_scopes: &AffectedScopeSet,
    all_known_scopes: &BTreeSet<ScopeId>,
) -> bool {
    invalidation_plan
        .recompute_scopes
        .intersection(&invalidation_plan.reuse_scopes)
        .next()
        .is_none()
        && invalidation_plan
            .recompute_scopes
            .union(&invalidation_plan.reuse_scopes)
            .cloned()
            .collect::<BTreeSet<_>>()
            == *all_known_scopes
        && affected_scopes
            .scopes
            .is_subset(&invalidation_plan.recompute_scopes)
}

fn collect_input_scopes(
    baseline: Option<&DiffBaseline>,
    diff_scopes: &BTreeSet<ScopeId>,
) -> BTreeSet<ScopeId> {
    let mut scopes = diff_scopes.clone();
    scopes.insert(project_scope_id());

    if let Some(baseline) = baseline {
        scopes.extend(collect_baseline_scopes(baseline));
    }

    scopes
}

fn collect_baseline_scopes(baseline: &DiffBaseline) -> BTreeSet<ScopeId> {
    let mut scopes = baseline
        .scope_metrics
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    scopes.extend(baseline.diagnostic_snapshots.keys().cloned());
    scopes.extend(collect_manifest_scopes(&baseline.dependency_index));
    scopes.insert(project_scope_id());
    scopes
}

fn collect_manifest_scopes(manifest: &DependencyIndexManifest) -> BTreeSet<ScopeId> {
    let mut scopes = manifest
        .reverse_dependencies
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    scopes.extend(
        manifest
            .reverse_dependencies
            .values()
            .flat_map(|dependents| dependents.iter().cloned()),
    );
    scopes
}

fn known_scopes_from_cpg(cpg: &UnifiedCpg) -> BTreeSet<ScopeId> {
    let mut scopes = cpg
        .nodes
        .iter()
        .filter_map(scope_id_for_node)
        .collect::<BTreeSet<_>>();
    scopes.insert(project_scope_id());
    scopes
}

fn contains_parent_map(cpg: &UnifiedCpg) -> BTreeMap<NodeId, Vec<NodeId>> {
    let mut parents = BTreeMap::<NodeId, Vec<NodeId>>::new();

    for edge in cpg
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Contains)
    {
        parents.entry(edge.target).or_default().push(edge.source);
    }

    for parent_ids in parents.values_mut() {
        parent_ids.sort();
        parent_ids.dedup();
    }

    parents
}

fn scope_id_for_node(node: &CpgNode) -> Option<ScopeId> {
    match node.kind {
        NodeKind::Function => Some(ScopeId::new(
            AnalysisLevel::Function,
            node.name.clone(),
            node.location.file_path.clone(),
        )),
        NodeKind::Module => Some(ScopeId::new(
            AnalysisLevel::Module,
            node.name.clone(),
            node.location.file_path.clone(),
        )),
        _ => None,
    }
}

fn project_scope_id() -> ScopeId {
    ScopeId::new(AnalysisLevel::Project, "<project>", ".")
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{
        BaselineFingerprint, DependencyIndexManifest, DiffBaseline, ImpactAnalysisInput,
        OverallScore, ScopeMetrics, analyze_impact, build_dependency_index_from_cpg,
        project_scope_id,
    };
    use crate::domains::cpg::{
        CpgEdge, CpgId, CpgNode, EdgeKind, NodeId, NodeKind, SourceLocation, UnifiedCpg,
    };
    use crate::domains::{AnalysisLevel, FilePath, ScopeId};

    #[test]
    fn analyze_impact_falls_back_without_baseline() {
        let scope_a = module_scope("crate::a", "src/a.rs");
        let diff_cpg = module_cpg(vec![scope_a.clone()], Vec::new());
        let changed_files = BTreeSet::from([FilePath::from("src/a.rs")]);

        let output = analyze_impact(&ImpactAnalysisInput {
            diff_cpg: &diff_cpg,
            changed_files: &changed_files,
            baseline: None,
            base_snapshot_hash: "base-tree",
        });

        assert!(output.invalidation_plan.fallback_to_full);
        assert!(output.affected_scopes.scopes.is_empty());
        assert!(output.invalidation_plan.recompute_scopes.is_empty());
        assert!(output.invalidation_plan.reuse_scopes.is_empty());
        assert!(
            output
                .merged_dependency_index
                .reverse_dependencies
                .contains_key(&scope_a)
        );
    }

    #[test]
    fn analyze_impact_recomputes_project_scope_and_reverse_dependents() {
        let scope_a = module_scope("crate::a", "src/a.rs");
        let scope_b = module_scope("crate::b", "src/b.rs");
        let scope_c = module_scope("crate::c", "src/c.rs");
        let baseline = baseline_with_scopes(
            vec![
                (scope_a.clone(), vec![scope_b.clone()]),
                (scope_b.clone(), vec![scope_c.clone()]),
            ],
            vec![scope_a.clone(), scope_b.clone(), scope_c.clone()],
            "base-tree",
        );
        let diff_cpg = module_cpg(vec![scope_a.clone()], Vec::new());
        let changed_files = BTreeSet::from([FilePath::from("src/a.rs")]);

        let output = analyze_impact(&ImpactAnalysisInput {
            diff_cpg: &diff_cpg,
            changed_files: &changed_files,
            baseline: Some(&baseline),
            base_snapshot_hash: "base-tree",
        });

        assert!(!output.invalidation_plan.fallback_to_full);
        assert!(output.affected_scopes.scopes.contains(&scope_a));
        assert!(output.affected_scopes.scopes.contains(&scope_b));
        assert!(output.affected_scopes.scopes.contains(&scope_c));
        assert!(
            output
                .invalidation_plan
                .recompute_scopes
                .contains(&project_scope_id())
        );
    }

    #[test]
    fn analyze_impact_preserves_invalidation_invariants() {
        let scope_a = module_scope("crate::a", "src/a.rs");
        let scope_b = module_scope("crate::b", "src/b.rs");
        let scope_c = module_scope("crate::c", "src/c.rs");
        let baseline = baseline_with_scopes(
            vec![
                (scope_a.clone(), vec![scope_b.clone()]),
                (scope_b.clone(), vec![scope_c.clone()]),
            ],
            vec![scope_a.clone(), scope_b.clone(), scope_c.clone()],
            "base-tree",
        );
        let diff_cpg = module_cpg(vec![scope_a.clone()], Vec::new());
        let changed_files = BTreeSet::from([FilePath::from("src/a.rs")]);

        let output = analyze_impact(&ImpactAnalysisInput {
            diff_cpg: &diff_cpg,
            changed_files: &changed_files,
            baseline: Some(&baseline),
            base_snapshot_hash: "base-tree",
        });

        let all_scopes = output
            .invalidation_plan
            .recompute_scopes
            .union(&output.invalidation_plan.reuse_scopes)
            .cloned()
            .collect::<BTreeSet<_>>();

        assert!(
            output
                .invalidation_plan
                .recompute_scopes
                .intersection(&output.invalidation_plan.reuse_scopes)
                .next()
                .is_none()
        );
        assert_eq!(
            all_scopes,
            BTreeSet::from([
                scope_a.clone(),
                scope_b.clone(),
                scope_c.clone(),
                project_scope_id()
            ])
        );
        assert!(
            output
                .affected_scopes
                .scopes
                .is_subset(&output.invalidation_plan.recompute_scopes)
        );
    }

    #[test]
    fn analyze_impact_falls_back_when_baseline_dependency_index_is_empty() {
        let scope_a = module_scope("crate::a", "src/a.rs");
        let baseline = baseline_with_scopes(Vec::new(), vec![scope_a.clone()], "base-tree");
        let diff_cpg = module_cpg(vec![scope_a], Vec::new());
        let changed_files = BTreeSet::from([FilePath::from("src/a.rs")]);

        let output = analyze_impact(&ImpactAnalysisInput {
            diff_cpg: &diff_cpg,
            changed_files: &changed_files,
            baseline: Some(&baseline),
            base_snapshot_hash: "base-tree",
        });

        assert!(output.invalidation_plan.fallback_to_full);
    }

    #[test]
    fn analyze_impact_is_deterministic() {
        let scope_a = module_scope("crate::a", "src/a.rs");
        let scope_b = module_scope("crate::b", "src/b.rs");
        let baseline = baseline_with_scopes(
            vec![(scope_a.clone(), vec![scope_b.clone()])],
            vec![scope_a.clone(), scope_b.clone()],
            "base-tree",
        );
        let diff_cpg = module_cpg(vec![scope_a], Vec::new());
        let changed_files = BTreeSet::from([FilePath::from("src/a.rs")]);
        let input = ImpactAnalysisInput {
            diff_cpg: &diff_cpg,
            changed_files: &changed_files,
            baseline: Some(&baseline),
            base_snapshot_hash: "base-tree",
        };

        let first = analyze_impact(&input);
        let second = analyze_impact(&input);

        assert_eq!(first, second);
    }

    #[test]
    fn dependency_index_builder_tracks_reverse_edges_for_calls() {
        let scope_a = module_scope("crate::a", "src/a.rs");
        let scope_b = module_scope("crate::b", "src/b.rs");
        let cpg = module_cpg(
            vec![scope_a.clone(), scope_b.clone()],
            vec![(scope_b.clone(), scope_a.clone(), EdgeKind::Call)],
        );

        let manifest = build_dependency_index_from_cpg(&cpg);

        assert_eq!(
            manifest.reverse_dependencies.get(&scope_a),
            Some(&BTreeSet::from([scope_b]))
        );
        assert!(
            manifest
                .reverse_dependencies
                .contains_key(&project_scope_id())
        );
    }

    fn baseline_with_scopes(
        reverse_edges: Vec<(ScopeId, Vec<ScopeId>)>,
        scopes: Vec<ScopeId>,
        base_snapshot_hash: &str,
    ) -> DiffBaseline {
        let mut reverse_dependencies = BTreeMap::new();
        for (scope_id, dependents) in reverse_edges {
            reverse_dependencies.insert(scope_id, dependents.into_iter().collect());
        }

        let mut scope_metrics = scopes
            .iter()
            .cloned()
            .map(|scope_id| {
                (
                    scope_id.clone(),
                    ScopeMetrics {
                        scope_id,
                        values: Vec::new(),
                        scope_risk: 0.0,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        scope_metrics
            .entry(project_scope_id())
            .or_insert_with(|| ScopeMetrics {
                scope_id: project_scope_id(),
                values: Vec::new(),
                scope_risk: 0.0,
            });

        DiffBaseline {
            fingerprint: BaselineFingerprint {
                workspace_root_hash: "workspace".to_owned(),
                base_snapshot_hash: base_snapshot_hash.to_owned(),
                config_hash: "config".to_owned(),
                analysis_targets_hash: "targets".to_owned(),
                rule_catalog_version: "rules".to_owned(),
                extractor_version: "extractor".to_owned(),
                kalos_version: "kalos".to_owned(),
            },
            dependency_index: DependencyIndexManifest {
                reverse_dependencies,
            },
            scope_metrics,
            diagnostic_snapshots: BTreeMap::new(),
            overall_score: OverallScore {
                function_risk: None,
                module_risk: Some(0.0),
                project_risk: Some(0.0),
                overall_risk: 0.0,
                overall_score: 100,
                function_score: None,
                module_score: Some(100),
                project_score: Some(100),
            },
        }
    }

    fn module_scope(name: &str, file_path: &str) -> ScopeId {
        ScopeId::new(AnalysisLevel::Module, name, file_path)
    }

    fn module_cpg(scopes: Vec<ScopeId>, edges: Vec<(ScopeId, ScopeId, EdgeKind)>) -> UnifiedCpg {
        let nodes = scopes
            .iter()
            .enumerate()
            .map(|(index, scope)| CpgNode {
                id: NodeId::from((index + 1) as u64),
                kind: NodeKind::Module,
                name: scope.qualified_name.clone(),
                location: SourceLocation {
                    file_path: scope.file_path.clone(),
                    start_line: 1,
                    end_line: 1,
                },
                extension: None,
            })
            .collect::<Vec<_>>();
        let node_ids = nodes
            .iter()
            .map(|node| {
                (
                    ScopeId::new(
                        AnalysisLevel::Module,
                        node.name.clone(),
                        node.location.file_path.clone(),
                    ),
                    node.id,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let cpg_edges = edges
            .into_iter()
            .map(|(source, target, kind)| CpgEdge {
                source: *node_ids.get(&source).expect("source scope should exist"),
                target: *node_ids.get(&target).expect("target scope should exist"),
                kind,
                extension: None,
            })
            .collect::<Vec<_>>();

        UnifiedCpg {
            id: CpgId::from("diff"),
            nodes,
            edges: cpg_edges,
        }
    }
}
