use std::collections::{BTreeMap, BTreeSet};

use crate::domains::diagnostics::DiagnosticSummary;
use crate::domains::metrics::{OverallScore, ScopeMetrics};

use super::{DiagnosticId, ScopeId};

#[derive(Clone, Debug, PartialEq)]
pub struct DiffBaseline {
    pub fingerprint: BaselineFingerprint,
    pub dependency_index: DependencyIndexManifest,
    pub scope_metrics: BTreeMap<ScopeId, ScopeMetrics>,
    pub diagnostic_snapshots: BTreeMap<ScopeId, ScopeDiagnosticSnapshot>,
    pub overall_score: OverallScore,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AffectedScopeSet {
    pub scopes: BTreeSet<ScopeId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidationPlan {
    pub recompute_scopes: BTreeSet<ScopeId>,
    pub reuse_scopes: BTreeSet<ScopeId>,
    pub fallback_to_full: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependencyIndexManifest {
    pub reverse_dependencies: BTreeMap<ScopeId, BTreeSet<ScopeId>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeDiagnosticSnapshot {
    pub scope_id: ScopeId,
    pub diagnostic_ids: Vec<DiagnosticId>,
    pub summary: DiagnosticSummary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineFingerprint {
    pub workspace_root_hash: String,
    pub base_snapshot_hash: String,
    pub config_hash: String,
    pub analysis_targets_hash: String,
    pub rule_catalog_version: String,
    pub extractor_version: String,
    pub kalos_version: String,
}
