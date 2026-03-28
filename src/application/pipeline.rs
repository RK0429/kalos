use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use crate::domains::config::ProjectConfig;
use crate::domains::cpg::{SourceAnalysis, SourceLocation, UnifiedCpg};
use crate::domains::diagnostics::{
    Diagnostic, DiagnosticsScope, ExitCode, FileLocation, InlineSuppression, MetricRule,
    RuleConfig, apply_suppressions, builtin_metric_rules, builtin_pattern_rules, project_subgraph,
};
use crate::domains::metrics::{
    AnalysisMetrics, MetricConfig, MetricDefinition, ScopeMetrics, builtin_metric_definitions,
    metric_catalog_from_definitions,
};
use crate::domains::reporting::{AnalysisReport, ReportMetadata, ReportViewOptions};
use crate::domains::{AnalysisLevel, ScopeId};
use crate::ports::extractor::{ExtractionRequest, ExtractorPort};

pub struct AnalysisPipeline<E> {
    extractor: E,
}

impl<E> AnalysisPipeline<E> {
    pub fn new(extractor: E) -> Self {
        Self { extractor }
    }
}

pub struct PipelineResult {
    pub report: AnalysisReport,
    pub exit_code: ExitCode,
}

#[derive(Debug)]
pub enum PipelineError<E> {
    Extraction(E),
}

impl<E: fmt::Display> fmt::Display for PipelineError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Extraction(error) => write!(f, "failed to extract CPG: {error}"),
        }
    }
}

impl<E> Error for PipelineError<E> where E: Error + 'static {}

impl<E> AnalysisPipeline<E>
where
    E: ExtractorPort,
{
    pub fn run(
        &self,
        config: &ProjectConfig,
        view_options: ReportViewOptions,
    ) -> Result<PipelineResult, PipelineError<E::Error>> {
        let request = ExtractionRequest {
            workspace_root: config.workspace_root.abs_path.clone(),
            analysis_targets: config.analysis_targets.clone(),
        };
        let source_analysis = self
            .extractor
            .extract(&request)
            .map_err(PipelineError::Extraction)?;
        let metrics = compute_metrics(&source_analysis.cpg, config);
        let diagnostics = generate_diagnostics(&source_analysis, &metrics, config);
        let report = assemble_report(config, metrics, diagnostics, view_options);
        let exit_code = report.diagnostics.determine_exit_code(report.view.strict);

        Ok(PipelineResult { report, exit_code })
    }
}

fn assemble_report(
    config: &ProjectConfig,
    metrics: AnalysisMetrics,
    diagnostics: Vec<Diagnostic>,
    view_options: ReportViewOptions,
) -> AnalysisReport {
    AnalysisReport::project(
        ReportMetadata::new(
            config.analysis_targets.clone(),
            env!("CARGO_PKG_VERSION"),
            "1.0.0",
        ),
        &metrics,
        diagnostics,
        DiagnosticsScope::WholeProject,
        view_options,
    )
}

fn compute_metrics(cpg: &UnifiedCpg, config: &ProjectConfig) -> AnalysisMetrics {
    let definitions = builtin_metric_definitions();
    let definition_refs = definitions
        .iter()
        .map(|definition| definition.as_ref())
        .collect::<Vec<_>>();
    let metric_catalog = metric_catalog_from_definitions(definition_refs);
    let metric_config = MetricConfig {
        entries: BTreeMap::new(),
    };

    let function_metrics = compute_scope_metrics(
        cpg,
        &function_scope_ids(cpg),
        AnalysisLevel::Function,
        &definitions,
        &metric_config,
    );
    let module_metrics = compute_scope_metrics(
        cpg,
        &module_scope_ids(cpg),
        AnalysisLevel::Module,
        &definitions,
        &metric_config,
    );
    let project_metrics = Some(
        compute_scope_metrics(
            cpg,
            &[project_scope_id()],
            AnalysisLevel::Project,
            &definitions,
            &metric_config,
        )
        .into_iter()
        .next()
        .expect("project scope metrics should always exist"),
    );

    AnalysisMetrics::assemble(
        function_metrics,
        module_metrics,
        project_metrics,
        &config.score_weights,
        &metric_catalog,
        &config.rules,
    )
}

fn compute_scope_metrics(
    cpg: &UnifiedCpg,
    scope_ids: &[ScopeId],
    level: AnalysisLevel,
    definitions: &[Box<dyn MetricDefinition>],
    metric_config: &MetricConfig,
) -> Vec<ScopeMetrics> {
    let mut scope_metrics = scope_ids
        .iter()
        .map(|scope_id| {
            let subgraph = cpg.subgraph(scope_id);
            let values = definitions
                .iter()
                .filter(|definition| definition.level() == level)
                .filter_map(|definition| definition.compute(&subgraph, metric_config))
                .collect::<Vec<_>>();

            ScopeMetrics::new(scope_id.clone(), values)
        })
        .collect::<Vec<_>>();
    scope_metrics.sort_by(|left, right| left.scope_id.cmp(&right.scope_id));
    scope_metrics
}

fn generate_diagnostics(
    source_analysis: &SourceAnalysis,
    metrics: &AnalysisMetrics,
    config: &ProjectConfig,
) -> Vec<Diagnostic> {
    let metric_rules = builtin_metric_rules();
    let metric_rule_lookup = metric_rules
        .iter()
        .map(|rule| (rule.metric_id.clone(), rule))
        .collect::<BTreeMap<_, _>>();

    let mut diagnostics =
        metric_diagnostics(source_analysis, metrics, &metric_rule_lookup, &config.rules);

    for rule in builtin_pattern_rules() {
        let rule_config = config.rules.get(&rule.id).cloned().unwrap_or(RuleConfig {
            enabled: Some(true),
            threshold: None,
            severity: None,
        });

        match rule.evaluation_scope {
            AnalysisLevel::Function => {
                for scope_id in function_scope_ids(&source_analysis.cpg) {
                    diagnostics.extend(rule.detect(
                        &source_analysis.cpg.subgraph(&scope_id),
                        metrics,
                        &rule_config,
                    ));
                }
            }
            AnalysisLevel::Module => {
                for scope_id in module_scope_ids(&source_analysis.cpg) {
                    diagnostics.extend(rule.detect(
                        &source_analysis.cpg.subgraph(&scope_id),
                        metrics,
                        &rule_config,
                    ));
                }
            }
            AnalysisLevel::Project => {
                diagnostics.extend(rule.detect(
                    &project_subgraph(&source_analysis.cpg),
                    metrics,
                    &rule_config,
                ));
            }
        }
    }

    let suppressions = source_analysis
        .suppressions
        .iter()
        .map(|suppression| InlineSuppression {
            location: FileLocation {
                file_path: suppression.location.file_path.clone(),
                start_line: suppression.location.start_line,
                end_line: suppression.location.end_line,
                column: None,
            },
            rule_id: suppression.rule_id.clone(),
        })
        .collect::<Vec<_>>();
    let mut diagnostics = apply_suppressions(diagnostics, &suppressions);
    diagnostics.sort_by_key(diagnostic_sort_key);
    diagnostics
}

fn metric_diagnostics(
    source_analysis: &SourceAnalysis,
    metrics: &AnalysisMetrics,
    metric_rule_lookup: &BTreeMap<crate::domains::MetricId, &MetricRule>,
    rules: &BTreeMap<crate::domains::RuleId, RuleConfig>,
) -> Vec<Diagnostic> {
    all_scope_metrics(metrics)
        .flat_map(|scope_metrics| {
            let location = scope_location(&scope_metrics.scope_id, &source_analysis.cpg);
            scope_metrics.values.iter().filter_map(move |value| {
                let rule = metric_rule_lookup.get(&value.metric_id)?;
                let rule_config = rules.get(&rule.id).cloned().unwrap_or(RuleConfig {
                    enabled: Some(true),
                    threshold: Some(rule.default_threshold),
                    severity: None,
                });
                rule.evaluate(
                    &scope_metrics.scope_id,
                    &location,
                    &crate::domains::diagnostics::MetricObservation {
                        metric_id: value.metric_id.clone(),
                        raw_value: value.raw_value,
                        normalized_risk: value.normalized_risk,
                        threshold: 0.0,
                        overflow_ratio: 0.0,
                    },
                    &rule_config,
                )
            })
        })
        .collect()
}

fn all_scope_metrics(metrics: &AnalysisMetrics) -> impl Iterator<Item = &ScopeMetrics> {
    metrics
        .function_metrics
        .iter()
        .chain(metrics.module_metrics.iter())
        .chain(metrics.project_metrics.iter())
}

fn function_scope_ids(cpg: &UnifiedCpg) -> Vec<ScopeId> {
    let mut scope_ids = cpg
        .functions()
        .into_iter()
        .map(|node| {
            ScopeId::new(
                AnalysisLevel::Function,
                node.name.clone(),
                node.location.file_path.clone(),
            )
        })
        .collect::<Vec<_>>();
    scope_ids.sort();
    scope_ids
}

fn module_scope_ids(cpg: &UnifiedCpg) -> Vec<ScopeId> {
    let mut scope_ids = cpg
        .modules()
        .into_iter()
        .map(|node| {
            ScopeId::new(
                AnalysisLevel::Module,
                node.name.clone(),
                node.location.file_path.clone(),
            )
        })
        .collect::<Vec<_>>();
    scope_ids.sort();
    scope_ids
}

fn project_scope_id() -> ScopeId {
    ScopeId::new(AnalysisLevel::Project, "<project>", ".")
}

fn scope_location(scope_id: &ScopeId, cpg: &UnifiedCpg) -> FileLocation {
    let source_location = match scope_id.level {
        AnalysisLevel::Function => cpg
            .functions()
            .into_iter()
            .find(|node| {
                node.name == scope_id.qualified_name
                    && node.location.file_path == scope_id.file_path
            })
            .map(|node| node.location.clone()),
        AnalysisLevel::Module => cpg
            .modules()
            .into_iter()
            .find(|node| {
                node.name == scope_id.qualified_name
                    && node.location.file_path == scope_id.file_path
            })
            .map(|node| node.location.clone()),
        AnalysisLevel::Project => Some(SourceLocation {
            file_path: scope_id.file_path.clone(),
            start_line: 1,
            end_line: 1,
        }),
    }
    .unwrap_or(SourceLocation {
        file_path: scope_id.file_path.clone(),
        start_line: 1,
        end_line: 1,
    });

    FileLocation {
        file_path: source_location.file_path,
        start_line: source_location.start_line,
        end_line: source_location.end_line,
        column: None,
    }
}

fn diagnostic_sort_key(
    diagnostic: &Diagnostic,
) -> (
    crate::domains::FilePath,
    u32,
    u32,
    Option<u32>,
    crate::domains::Severity,
    crate::domains::RuleId,
    ScopeId,
) {
    (
        diagnostic.location.file_path.clone(),
        diagnostic.location.start_line,
        diagnostic.location.end_line,
        diagnostic.location.column,
        diagnostic.severity,
        diagnostic.rule_id.clone(),
        diagnostic.primary_scope_id.clone(),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{AnalysisPipeline, assemble_report, project_scope_id};
    use crate::domains::config::{Defaults, ProjectConfig, WorkspaceRoot};
    use crate::domains::cpg::{CpgId, SourceAnalysis, UnifiedCpg};
    use crate::domains::diagnostics::{
        Diagnostic, DiagnosticKind, FileLocation, MetricObservation, TemplateSuggestion,
    };
    use crate::domains::metrics::{AnalysisMetrics, OverallScore};
    use crate::domains::reporting::{
        OutputFormat, ReportScopeMetrics, ReportViewOptions, RequestedLevel,
    };
    use crate::domains::{
        AnalysisLevel, DiagnosticId, FilePath, MetricId, RuleId, ScopeId, Severity,
    };
    use crate::ports::extractor::{ExtractionRequest, ExtractorPort};

    #[derive(Clone, Debug)]
    struct MockExtractor {
        source_analysis: SourceAnalysis,
    }

    impl ExtractorPort for MockExtractor {
        type Error = std::convert::Infallible;

        fn extract(&self, _request: &ExtractionRequest) -> Result<SourceAnalysis, Self::Error> {
            Ok(self.source_analysis.clone())
        }
    }

    #[test]
    fn assemble_report_materializes_level_summary_and_strict_exit_code() {
        let report = assemble_report(
            &fixture_config(),
            fixture_metrics(),
            vec![
                metric_diagnostic(
                    "diag-function-warning",
                    AnalysisLevel::Function,
                    "crate::f",
                    "src/lib.rs",
                    Severity::Warning,
                    "KAL-F001",
                    "M-F001",
                ),
                metric_diagnostic(
                    "diag-module-error",
                    AnalysisLevel::Module,
                    "crate",
                    "src/lib.rs",
                    Severity::Error,
                    "KAL-M001",
                    "M-M001",
                ),
            ],
            ReportViewOptions {
                requested_level: RequestedLevel::Function,
                output_format: OutputFormat::Json,
                strict: true,
                minimum_severity: None,
            },
        );

        assert_eq!(report.diagnostics.summary.error_count, 0);
        assert_eq!(report.diagnostics.summary.warning_count, 1);
        assert_eq!(
            report.diagnostics.determine_exit_code(report.view.strict),
            crate::domains::diagnostics::ExitCode::DiagnosticFailure
        );
    }

    #[test]
    fn analysis_pipeline_runs_non_diff_flow_with_stub_extractor() {
        let pipeline = AnalysisPipeline::new(MockExtractor {
            source_analysis: SourceAnalysis {
                cpg: UnifiedCpg {
                    id: CpgId::from("stub"),
                    nodes: Vec::new(),
                    edges: Vec::new(),
                },
                source_files: BTreeMap::new(),
                suppressions: Vec::new(),
                warnings: Vec::new(),
            },
        });

        let result = pipeline
            .run(
                &fixture_config(),
                ReportViewOptions {
                    requested_level: RequestedLevel::All,
                    output_format: OutputFormat::Json,
                    strict: false,
                    minimum_severity: None,
                },
            )
            .expect("pipeline should succeed");

        assert_eq!(result.report.diagnostics.summary.error_count, 0);
        assert_eq!(result.report.diagnostics.summary.warning_count, 0);
        assert_eq!(result.report.diagnostics.summary.info_count, 0);
        assert_eq!(
            result.exit_code,
            crate::domains::diagnostics::ExitCode::Success
        );
        assert_eq!(
            result.report.metadata.analysis_targets,
            vec![FilePath::from(".")]
        );
        assert!(matches!(
            result.report.metrics.last(),
            Some(ReportScopeMetrics { scope_id, .. }) if *scope_id == project_scope_id()
        ));
    }

    fn fixture_config() -> ProjectConfig {
        ProjectConfig {
            workspace_root: WorkspaceRoot {
                abs_path: "/workspace".into(),
            },
            analysis_targets: vec![FilePath::from(".")],
            rules: Defaults::default().rules,
            exclude_patterns: Vec::new(),
            score_weights: Defaults::default().score_weights,
            plugin_manifest: Default::default(),
            targets_explicitly_specified: false,
        }
    }

    fn fixture_metrics() -> AnalysisMetrics {
        AnalysisMetrics {
            function_metrics: vec![
                ScopeMetricsLiteral::new(
                    ScopeId::new(AnalysisLevel::Function, "crate::f", "src/lib.rs"),
                    0.55,
                    vec![("M-F001", 1.0, 0.55)],
                )
                .into(),
            ],
            module_metrics: vec![
                ScopeMetricsLiteral::new(
                    ScopeId::new(AnalysisLevel::Module, "crate", "src/lib.rs"),
                    0.80,
                    vec![("M-M001", 4.0, 0.80)],
                )
                .into(),
            ],
            project_metrics: Some(
                ScopeMetricsLiteral::new(project_scope_id(), 0.25, vec![("M-P001", 1.0, 0.25)])
                    .into(),
            ),
            overall_score: OverallScore {
                function_risk: Some(0.55),
                module_risk: Some(0.80),
                project_risk: Some(0.25),
                overall_risk: 0.56,
                overall_score: 44,
                function_score: Some(45),
                module_score: Some(20),
                project_score: Some(75),
            },
        }
    }

    fn metric_diagnostic(
        id: &str,
        level: AnalysisLevel,
        qualified_name: &str,
        file_path: &str,
        severity: Severity,
        rule_id: &str,
        metric_id: &str,
    ) -> Diagnostic {
        Diagnostic {
            id: DiagnosticId::from(id),
            primary_scope_id: ScopeId::new(level, qualified_name, file_path),
            rule_id: RuleId::from(rule_id),
            kind: DiagnosticKind::Metric,
            severity,
            location: FileLocation {
                file_path: FilePath::from(file_path),
                start_line: 10,
                end_line: 12,
                column: Some(3),
            },
            message: "test diagnostic".to_owned(),
            metric: Some(MetricObservation {
                metric_id: MetricId::from(metric_id),
                raw_value: 1.0,
                normalized_risk: 0.8,
                threshold: 0.55,
                overflow_ratio: 0.25,
            }),
            pattern: None,
            template_suggestion: TemplateSuggestion {
                explanation: "fix it".to_owned(),
                code_example: None,
            },
        }
    }

    struct ScopeMetricsLiteral {
        scope_id: ScopeId,
        scope_risk: f64,
        values: Vec<(&'static str, f64, f64)>,
    }

    impl ScopeMetricsLiteral {
        fn new(scope_id: ScopeId, scope_risk: f64, values: Vec<(&'static str, f64, f64)>) -> Self {
            Self {
                scope_id,
                scope_risk,
                values,
            }
        }
    }

    impl From<ScopeMetricsLiteral> for crate::domains::metrics::ScopeMetrics {
        fn from(value: ScopeMetricsLiteral) -> Self {
            crate::domains::metrics::ScopeMetrics {
                scope_id: value.scope_id,
                scope_risk: value.scope_risk,
                values: value
                    .values
                    .into_iter()
                    .map(|(metric_id, raw_value, normalized_risk)| {
                        crate::domains::metrics::MetricValue {
                            metric_id: MetricId::from(metric_id),
                            raw_value,
                            normalized_risk,
                        }
                    })
                    .collect(),
            }
        }
    }
}
