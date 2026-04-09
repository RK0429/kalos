use std::collections::BTreeMap;

use crate::domains::cpg::CpgSubgraph;
use crate::domains::diagnostics::RuleConfig;

use super::{AnalysisLevel, MetricId, RuleId, ScopeId};

pub mod builtin;
pub use builtin::{
    CfgBranchEntropyRisk, CircularDependencyParticipationRisk, CyclicCouplingRisk,
    CyclomaticComplexityRisk, DataFlowDensityRisk, HubDependencyConcentrationRisk,
    IdentifierRepetitionRisk, InstabilityRisk, ModuleFanOutRisk, ModuleSizeEntropyImbalanceRisk,
    builtin_metric_definitions,
};

#[cfg(test)]
pub mod test_fixtures;

#[derive(Clone, Debug, PartialEq)]
pub struct AnalysisMetrics {
    pub function_metrics: Vec<ScopeMetrics>,
    pub module_metrics: Vec<ScopeMetrics>,
    pub project_metrics: Option<ScopeMetrics>,
    pub overall_score: OverallScore,
}

impl AnalysisMetrics {
    pub fn assemble(
        mut function_metrics: Vec<ScopeMetrics>,
        mut module_metrics: Vec<ScopeMetrics>,
        mut project_metrics: Option<ScopeMetrics>,
        weights: &ScoreWeights,
        metric_metadata: &BTreeMap<MetricId, MetricMetadata>,
        rule_configs: &BTreeMap<RuleId, RuleConfig>,
    ) -> Self {
        for scope_metrics in &mut function_metrics {
            scope_metrics.refresh_scope_risk(metric_metadata, rule_configs);
        }
        for scope_metrics in &mut module_metrics {
            scope_metrics.refresh_scope_risk(metric_metadata, rule_configs);
        }
        if let Some(scope_metrics) = &mut project_metrics {
            scope_metrics.refresh_scope_risk(metric_metadata, rule_configs);
        }

        let function_risk = compute_level_risk(&function_metrics);
        let module_risk = compute_level_risk(&module_metrics);
        let project_risk = project_metrics
            .as_ref()
            .map(|scope_metrics| scope_metrics.scope_risk);

        let adjusted_weights = AdjustedWeights::from_available_levels(
            weights,
            function_risk.is_some(),
            module_risk.is_some(),
            project_risk.is_some(),
        );

        let overall_risk = round_half_up(
            adjusted_weights.function * function_risk.unwrap_or(0.0)
                + adjusted_weights.module * module_risk.unwrap_or(0.0)
                + adjusted_weights.project * project_risk.unwrap_or(0.0),
            6,
        );

        Self {
            function_metrics,
            module_metrics,
            project_metrics,
            overall_score: OverallScore {
                function_risk,
                module_risk,
                project_risk,
                overall_risk,
                overall_score: risk_to_score(overall_risk),
                function_score: function_risk.map(risk_to_score),
                module_score: module_risk.map(risk_to_score),
                project_score: project_risk.map(risk_to_score),
            },
        }
    }

    pub fn overall_score(&self) -> &OverallScore {
        &self.overall_score
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScopeMetrics {
    pub scope_id: ScopeId,
    pub values: Vec<MetricValue>,
    pub scope_risk: f64,
}

impl ScopeMetrics {
    pub fn new(scope_id: ScopeId, values: Vec<MetricValue>) -> Self {
        Self {
            scope_id,
            values,
            scope_risk: 0.0,
        }
    }

    pub fn refresh_scope_risk(
        &mut self,
        metric_metadata: &BTreeMap<MetricId, MetricMetadata>,
        rule_configs: &BTreeMap<RuleId, RuleConfig>,
    ) {
        self.scope_risk = Self::calculate_scope_risk(&self.values, metric_metadata, rule_configs);
    }

    pub fn calculate_scope_risk(
        values: &[MetricValue],
        metric_metadata: &BTreeMap<MetricId, MetricMetadata>,
        rule_configs: &BTreeMap<RuleId, RuleConfig>,
    ) -> f64 {
        let mut total = 0.0;
        let mut count = 0_u32;

        for value in values {
            if metric_metadata
                .get(&value.metric_id)
                .is_some_and(|metadata| metadata.participates_in_scoring(rule_configs))
            {
                total += value.normalized_risk;
                count += 1;
            }
        }

        if count == 0 {
            0.0
        } else {
            round_half_up(total / f64::from(count), 6)
        }
    }

    pub fn scope_risk(&self) -> f64 {
        self.scope_risk
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MetricValue {
    pub metric_id: MetricId,
    pub raw_value: f64,
    pub normalized_risk: f64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OverallScore {
    pub function_risk: Option<f64>,
    pub module_risk: Option<f64>,
    pub project_risk: Option<f64>,
    pub overall_risk: f64,
    pub overall_score: u8,
    pub function_score: Option<u8>,
    pub module_score: Option<u8>,
    pub project_score: Option<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScoreWeights {
    pub function: f64,
    pub module: f64,
    pub project: f64,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetricMetadata {
    pub name: Option<String>,
    pub participation: MetricParticipation,
    pub rule_binding: Option<RuleId>,
}

impl MetricMetadata {
    pub fn from_definition(definition: &dyn MetricDefinition) -> Self {
        Self {
            name: Some(definition.name().to_owned()),
            participation: definition.participation(),
            rule_binding: definition.rule_binding().cloned(),
        }
    }

    fn participates_in_scoring(&self, rule_configs: &BTreeMap<RuleId, RuleConfig>) -> bool {
        if self.participation != MetricParticipation::ScoredAndDiagnosable {
            return false;
        }

        self.rule_binding
            .as_ref()
            .and_then(|rule_id| rule_configs.get(rule_id))
            .and_then(|config| config.enabled)
            != Some(false)
    }
}

pub fn metric_catalog_from_definitions<'a>(
    definitions: impl IntoIterator<Item = &'a dyn MetricDefinition>,
) -> BTreeMap<MetricId, MetricMetadata> {
    definitions
        .into_iter()
        .map(|definition| {
            (
                definition.id().clone(),
                MetricMetadata::from_definition(definition),
            )
        })
        .collect()
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

#[derive(Copy, Clone, Debug, PartialEq)]
struct AdjustedWeights {
    function: f64,
    module: f64,
    project: f64,
}

impl AdjustedWeights {
    fn from_available_levels(
        weights: &ScoreWeights,
        has_function: bool,
        has_module: bool,
        has_project: bool,
    ) -> Self {
        let normalized_total = weights.function + weights.module + weights.project;
        let normalized_function = weights.function / normalized_total;
        let normalized_module = weights.module / normalized_total;
        let normalized_project = weights.project / normalized_total;

        let available_total = (if has_function {
            normalized_function
        } else {
            0.0
        }) + (if has_module { normalized_module } else { 0.0 })
            + (if has_project { normalized_project } else { 0.0 });

        if available_total == 0.0 {
            return Self {
                function: 0.0,
                module: 0.0,
                project: 0.0,
            };
        }

        Self {
            function: if has_function {
                normalized_function / available_total
            } else {
                0.0
            },
            module: if has_module {
                normalized_module / available_total
            } else {
                0.0
            },
            project: if has_project {
                normalized_project / available_total
            } else {
                0.0
            },
        }
    }
}

fn compute_level_risk(scope_metrics: &[ScopeMetrics]) -> Option<f64> {
    if scope_metrics.is_empty() {
        return None;
    }

    let total = scope_metrics
        .iter()
        .map(|scope_metrics| scope_metrics.scope_risk)
        .sum::<f64>();

    Some(round_half_up(total / scope_metrics.len() as f64, 6))
}

fn risk_to_score(risk: f64) -> u8 {
    round_half_up(100.0 * (1.0 - risk), 0).clamp(0.0, 100.0) as u8
}

fn normalize_zero(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        AnalysisMetrics, MetricMetadata, MetricParticipation, MetricValue, ScopeMetrics,
        ScoreWeights, builtin_metric_definitions, metric_catalog_from_definitions, round_half_up,
    };
    use crate::domains::diagnostics::RuleConfig;
    use crate::domains::{AnalysisLevel, MetricId, RuleId, ScopeId};

    #[test]
    fn round_half_up_rounds_positive_and_negative_values() {
        assert_eq!(round_half_up(0.5000004, 6), 0.5);
        assert_eq!(round_half_up(0.5000005, 6), 0.500001);
        assert_eq!(round_half_up(0.0000004, 6), 0.0);
        assert_eq!(round_half_up(0.0000005, 6), 0.000001);
        assert_eq!(round_half_up(0.9999994, 6), 0.999999);
        assert_eq!(round_half_up(0.9999995, 6), 1.0);
        assert_eq!(round_half_up(-1.2345674, 6), -1.234567);
        assert_eq!(round_half_up(-1.2345675, 6), -1.234567);
        assert_eq!(round_half_up(-1.2345676, 6), -1.234568);
        assert_eq!(round_half_up(-0.0000004, 6), 0.0);
        assert_eq!(round_half_up(-0.0000005, 6), 0.0);
    }

    #[test]
    fn builtin_metric_catalog_covers_all_wave2_metrics() {
        let definitions = builtin_metric_definitions();
        let refs = definitions
            .iter()
            .map(|definition| definition.as_ref())
            .collect::<Vec<_>>();
        let catalog = metric_catalog_from_definitions(refs);

        assert_eq!(catalog.len(), 10);
        assert!(catalog.contains_key(&MetricId::from("M-F001")));
        assert!(catalog.contains_key(&MetricId::from("M-M003")));
        assert!(catalog.contains_key(&MetricId::from("M-P003")));
    }

    #[test]
    fn scoring_engine_excludes_disabled_metrics_and_redistributes_weights() {
        let metric_metadata = BTreeMap::from([
            (
                MetricId::from("M-F001"),
                MetricMetadata {
                    name: None,
                    participation: MetricParticipation::ScoredAndDiagnosable,
                    rule_binding: Some(RuleId::from("KAL-F001")),
                },
            ),
            (
                MetricId::from("M-F002"),
                MetricMetadata {
                    name: None,
                    participation: MetricParticipation::ScoredAndDiagnosable,
                    rule_binding: Some(RuleId::from("KAL-F002")),
                },
            ),
            (
                MetricId::from("M-M001"),
                MetricMetadata {
                    name: None,
                    participation: MetricParticipation::ScoredAndDiagnosable,
                    rule_binding: Some(RuleId::from("KAL-M001")),
                },
            ),
        ]);
        let rule_configs = BTreeMap::from([
            (
                RuleId::from("KAL-F001"),
                RuleConfig {
                    enabled: Some(false),
                    threshold: Some(0.55),
                    severity: None,
                },
            ),
            (
                RuleId::from("KAL-F002"),
                RuleConfig {
                    enabled: Some(true),
                    threshold: Some(0.60),
                    severity: None,
                },
            ),
            (
                RuleId::from("KAL-M001"),
                RuleConfig {
                    enabled: Some(true),
                    threshold: Some(0.50),
                    severity: None,
                },
            ),
        ]);

        let analysis = AnalysisMetrics::assemble(
            vec![
                ScopeMetrics::new(
                    ScopeId::new(AnalysisLevel::Function, "crate::f1", "src/lib.rs"),
                    vec![
                        MetricValue {
                            metric_id: MetricId::from("M-F001"),
                            raw_value: 0.9,
                            normalized_risk: 0.9,
                        },
                        MetricValue {
                            metric_id: MetricId::from("M-F002"),
                            raw_value: 0.3,
                            normalized_risk: 0.3,
                        },
                    ],
                ),
                ScopeMetrics::new(
                    ScopeId::new(AnalysisLevel::Function, "crate::f2", "src/lib.rs"),
                    vec![
                        MetricValue {
                            metric_id: MetricId::from("M-F001"),
                            raw_value: 0.5,
                            normalized_risk: 0.5,
                        },
                        MetricValue {
                            metric_id: MetricId::from("M-F002"),
                            raw_value: 0.9,
                            normalized_risk: 0.9,
                        },
                    ],
                ),
            ],
            vec![ScopeMetrics::new(
                ScopeId::new(AnalysisLevel::Module, "crate", "src/lib.rs"),
                vec![MetricValue {
                    metric_id: MetricId::from("M-M001"),
                    raw_value: 0.4,
                    normalized_risk: 0.4,
                }],
            )],
            None,
            &ScoreWeights {
                function: 2.0,
                module: 1.0,
                project: 1.0,
            },
            &metric_metadata,
            &rule_configs,
        );

        assert_eq!(analysis.function_metrics[0].scope_risk(), 0.3);
        assert_eq!(analysis.function_metrics[1].scope_risk(), 0.9);
        assert_eq!(analysis.module_metrics[0].scope_risk(), 0.4);

        let overall_score = analysis.overall_score();

        assert_eq!(overall_score.function_risk, Some(0.6));
        assert_eq!(overall_score.module_risk, Some(0.4));
        assert_eq!(overall_score.project_risk, None);
        assert_eq!(overall_score.overall_risk, 0.533333);
        assert_eq!(overall_score.function_score, Some(40));
        assert_eq!(overall_score.module_score, Some(60));
        assert_eq!(overall_score.project_score, None);
        assert_eq!(overall_score.overall_score, 47);
    }

    #[test]
    fn scoring_engine_redistributes_weights_when_only_project_metrics_exist() {
        let metric_metadata = BTreeMap::from([(
            MetricId::from("M-P001"),
            MetricMetadata {
                name: None,
                participation: MetricParticipation::ScoredAndDiagnosable,
                rule_binding: Some(RuleId::from("KAL-P001")),
            },
        )]);
        let rule_configs = BTreeMap::from([(
            RuleId::from("KAL-P001"),
            RuleConfig {
                enabled: Some(true),
                threshold: Some(0.15),
                severity: None,
            },
        )]);

        let analysis = AnalysisMetrics::assemble(
            Vec::new(),
            Vec::new(),
            Some(ScopeMetrics::new(
                ScopeId::new(AnalysisLevel::Project, "<project>", "."),
                vec![MetricValue {
                    metric_id: MetricId::from("M-P001"),
                    raw_value: 0.8,
                    normalized_risk: 0.8,
                }],
            )),
            &ScoreWeights {
                function: 0.4,
                module: 0.35,
                project: 0.25,
            },
            &metric_metadata,
            &rule_configs,
        );

        let overall_score = analysis.overall_score();

        assert_eq!(overall_score.function_risk, None);
        assert_eq!(overall_score.module_risk, None);
        assert_eq!(overall_score.project_risk, Some(0.8));
        assert_eq!(overall_score.overall_risk, 0.8);
        assert_eq!(overall_score.overall_score, 20);
    }

    #[test]
    fn scoring_engine_redistributes_weights_when_only_function_metrics_exist() {
        let metric_metadata = BTreeMap::from([(
            MetricId::from("M-F002"),
            MetricMetadata {
                name: None,
                participation: MetricParticipation::ScoredAndDiagnosable,
                rule_binding: Some(RuleId::from("KAL-F002")),
            },
        )]);
        let rule_configs = BTreeMap::from([(
            RuleId::from("KAL-F002"),
            RuleConfig {
                enabled: Some(true),
                threshold: Some(0.60),
                severity: None,
            },
        )]);

        let analysis = AnalysisMetrics::assemble(
            vec![ScopeMetrics::new(
                ScopeId::new(AnalysisLevel::Function, "crate::f", "src/lib.rs"),
                vec![MetricValue {
                    metric_id: MetricId::from("M-F002"),
                    raw_value: 0.25,
                    normalized_risk: 0.25,
                }],
            )],
            Vec::new(),
            None,
            &ScoreWeights {
                function: 2.0,
                module: 1.0,
                project: 1.0,
            },
            &metric_metadata,
            &rule_configs,
        );

        let overall_score = analysis.overall_score();

        assert_eq!(overall_score.function_risk, Some(0.25));
        assert_eq!(overall_score.module_risk, None);
        assert_eq!(overall_score.project_risk, None);
        assert_eq!(overall_score.overall_risk, 0.25);
        assert_eq!(overall_score.overall_score, 75);
    }
}
