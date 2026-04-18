use std::collections::BTreeMap;

use crate::domains::cpg::CpgSubgraph;
use crate::domains::{AnalysisLevel, MetricId, RuleId};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MetricValue {
    pub metric_id: MetricId,
    pub raw_value: f64,
    pub normalized_risk: f64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetricOrigin {
    BuiltIn,
    Plugin,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetricParticipation {
    ScoredAndDiagnosable,
    ReportOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetricConfig {
    pub entries: BTreeMap<String, String>,
}

pub fn round_half_up(value: f64, decimal_places: u32) -> f64 {
    let factor = 10_f64.powi(decimal_places as i32);
    let scaled = value * factor;
    let epsilon = f64::EPSILON * (scaled.abs() + 1.0) * 4.0;
    normalize_zero((scaled + 0.5 + epsilon).floor() / factor)
}

pub trait MetricDefinition: Send + Sync {
    fn id(&self) -> &MetricId;
    fn name(&self) -> &str;
    fn level(&self) -> AnalysisLevel;
    fn origin(&self) -> MetricOrigin;
    fn participation(&self) -> MetricParticipation;
    fn rule_binding(&self) -> Option<&RuleId>;
    fn description(&self) -> &str;
    fn compute(&self, subgraph: &CpgSubgraph, config: &MetricConfig) -> Option<MetricValue>;
}

fn normalize_zero(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}
