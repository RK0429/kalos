use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::domains::diagnostics::{
    Diagnostic, DiagnosticKind, DiagnosticReport, DiagnosticSummary, DiagnosticsScope,
    LlmSuggestionBundle, PatternType, SummaryScope, builtin_metric_rules, builtin_pattern_rules,
};
use crate::domains::metrics::{
    AnalysisMetrics, MetricMetadata, MetricParticipation, MetricValue, ScopeMetrics,
    builtin_metric_definitions, metric_catalog_from_definitions,
};
use crate::domains::{AnalysisLevel, FilePath, ScopeId, Severity};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportMetadata {
    pub analysis_targets: Vec<FilePath>,
    pub file_count: usize,
    pub tool_version: String,
    pub schema_version: String,
}

impl ReportMetadata {
    pub fn new(
        analysis_targets: Vec<FilePath>,
        file_count: usize,
        tool_version: impl Into<String>,
        schema_version: impl Into<String>,
    ) -> Self {
        Self {
            analysis_targets,
            file_count,
            tool_version: tool_version.into(),
            schema_version: schema_version.into(),
        }
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum OutputFormat {
    #[default]
    Human,
    Json,
    Sarif,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum RequestedLevel {
    Function,
    Module,
    Project,
    #[default]
    All,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportViewOptions {
    pub requested_level: RequestedLevel,
    pub output_format: OutputFormat,
    pub strict: bool,
    pub minimum_severity: Option<Severity>,
    pub verbose: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectedScores {
    pub overall: Option<u8>,
    pub function: Option<u8>,
    pub module: Option<u8>,
    pub project: Option<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReportMetricValue {
    pub metric_id: crate::domains::MetricId,
    pub raw_value: f64,
    pub normalized_risk: f64,
    pub participation: MetricParticipation,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReportScopeMetrics {
    pub scope_id: ScopeId,
    pub scope_risk: f64,
    pub values: Vec<ReportMetricValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnalysisReport {
    pub metadata: ReportMetadata,
    pub view: ReportViewOptions,
    pub scores: ProjectedScores,
    pub metrics: Vec<ReportScopeMetrics>,
    pub diagnostics: DiagnosticReport,
}

#[derive(Debug)]
pub enum RenderError {
    Json(serde_json::Error),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(error) => write!(f, "failed to render JSON: {error}"),
        }
    }
}

impl std::error::Error for RenderError {}

impl From<serde_json::Error> for RenderError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl AnalysisReport {
    pub fn project(
        metadata: ReportMetadata,
        metrics: &AnalysisMetrics,
        diagnostics: Vec<Diagnostic>,
        diagnostics_scope: DiagnosticsScope,
        view: ReportViewOptions,
        summary_override: Option<DiagnosticSummary>,
    ) -> Self {
        let definitions = builtin_metric_definitions();
        let definition_refs = definitions
            .iter()
            .map(|definition| definition.as_ref())
            .collect::<Vec<_>>();
        let catalog = metric_catalog_from_definitions(definition_refs);

        Self::project_with_metric_catalog(
            metadata,
            metrics,
            &catalog,
            diagnostics,
            diagnostics_scope,
            view,
            summary_override,
        )
    }

    pub fn project_with_metric_catalog(
        metadata: ReportMetadata,
        metrics: &AnalysisMetrics,
        metric_catalog: &BTreeMap<crate::domains::MetricId, MetricMetadata>,
        diagnostics: Vec<Diagnostic>,
        diagnostics_scope: DiagnosticsScope,
        view: ReportViewOptions,
        summary_override: Option<DiagnosticSummary>,
    ) -> Self {
        let projected_metrics = project_metrics(metrics, view.requested_level, metric_catalog);
        let projected_diagnostics = project_diagnostics(&diagnostics, view.requested_level);
        let summary_scope = summary_scope_for(view.requested_level);
        let summary = summary_override.unwrap_or_else(|| {
            materialize_summary(match summary_scope {
                SummaryScope::WholeProject => &diagnostics,
                SummaryScope::ListedDiagnostics => &projected_diagnostics,
            })
        });

        Self {
            metadata,
            view: view.clone(),
            scores: project_scores(metrics, view.requested_level),
            metrics: projected_metrics,
            diagnostics: DiagnosticReport {
                diagnostics: projected_diagnostics,
                summary,
                diagnostics_scope,
                summary_scope,
            },
        }
    }

    pub fn render(
        &self,
        llm_suggestions: Option<&LlmSuggestionBundle>,
        use_color: bool,
    ) -> Result<String, RenderError> {
        match self.view.output_format {
            OutputFormat::Human => Ok(self.render_human(llm_suggestions, use_color)),
            OutputFormat::Json => self.render_json(llm_suggestions),
            OutputFormat::Sarif => self.render_sarif(llm_suggestions),
        }
    }

    pub fn render_human(
        &self,
        llm_suggestions: Option<&LlmSuggestionBundle>,
        use_color: bool,
    ) -> String {
        let mut output = String::new();
        let diagnostics = self.visible_diagnostics();
        let analysis_targets = self
            .metadata
            .analysis_targets
            .iter()
            .map(FilePath::as_str)
            .collect::<Vec<_>>()
            .join(", ");

        let _ = writeln!(
            output,
            "Analyzed {} files in {}",
            self.metadata.file_count, analysis_targets
        );
        if !diagnostics.is_empty() {
            output.push('\n');
        }

        for diagnostic in diagnostics {
            let _ = writeln!(
                output,
                "{}  {}[{}]  [{}] {}",
                format_location(&diagnostic.location),
                render_severity(diagnostic.severity, use_color),
                diagnostic.rule_id.as_str(),
                diagnostic_kind_str(diagnostic.kind),
                diagnostic.message
            );

            match diagnostic.kind {
                DiagnosticKind::Metric => {
                    if let Some(metric) = &diagnostic.metric {
                        let _ = writeln!(
                            output,
                            "  metric={} raw={:.3} normalized={:.3} threshold={:.3} overflow={:.3}",
                            metric.metric_id.as_str(),
                            metric.raw_value,
                            metric.normalized_risk,
                            metric.threshold,
                            metric.overflow_ratio
                        );
                    }
                }
                DiagnosticKind::Pattern => {
                    if let Some(pattern) = &diagnostic.pattern {
                        let _ = writeln!(
                            output,
                            "  pattern={} evidence={}",
                            pattern_type_str(pattern.pattern_type),
                            pattern.evidence_message
                        );
                    }
                }
            }

            let _ = writeln!(
                output,
                "  template -> {}",
                diagnostic.template_suggestion.explanation
            );
            if let Some(llm) =
                llm_suggestions.and_then(|bundle| bundle.enrichments.get(&diagnostic.id))
            {
                let _ = writeln!(output, "  llm      -> {}", llm.explanation);
            }
            output.push('\n');
        }

        let _ = writeln!(output, "── Summary ──────────────────────────");
        let _ = writeln!(output, "{}", self.human_score_line());
        let _ = writeln!(
            output,
            "{} errors, {} warnings, {} info",
            self.diagnostics.summary.error_count,
            self.diagnostics.summary.warning_count,
            self.diagnostics.summary.info_count
        );
        if self.view.verbose && !self.metrics.is_empty() {
            let _ = writeln!(output, "\n── Metrics ───────────────────────────");
            for scope_metrics in &self.metrics {
                let _ = writeln!(
                    output,
                    "{} [{}] risk={:.3}",
                    scope_metrics.scope_id.qualified_name,
                    analysis_level_str(scope_metrics.scope_id.level),
                    scope_metrics.scope_risk
                );
                for value in &scope_metrics.values {
                    let _ = writeln!(
                        output,
                        "  metric={} raw={:.3} normalized={:.3} participation={}",
                        value.metric_id.as_str(),
                        value.raw_value,
                        value.normalized_risk,
                        participation_str(value.participation)
                    );
                }
            }
        }

        output
    }

    pub fn render_json(
        &self,
        llm_suggestions: Option<&LlmSuggestionBundle>,
    ) -> Result<String, RenderError> {
        let diagnostics = self
            .visible_diagnostics()
            .into_iter()
            .map(|diagnostic| diagnostic_json(diagnostic, llm_suggestions))
            .collect::<Vec<_>>();
        let metrics = self
            .metrics
            .iter()
            .map(report_scope_metrics_json)
            .collect::<Vec<_>>();

        Ok(serde_json::to_string_pretty(&json!({
            "schema_version": self.metadata.schema_version,
            "analysis_targets": self
                .metadata
                .analysis_targets
                .iter()
                .map(FilePath::as_str)
                .collect::<Vec<_>>(),
            "scores": {
                "overall": self.scores.overall,
                "function": self.scores.function,
                "module": self.scores.module,
                "project": self.scores.project,
            },
            "metrics": metrics,
            "diagnostics": diagnostics,
            "diagnostics_scope": diagnostics_scope_str(self.diagnostics.diagnostics_scope),
            "summary": {
                "error_count": self.diagnostics.summary.error_count,
                "warning_count": self.diagnostics.summary.warning_count,
                "info_count": self.diagnostics.summary.info_count,
            },
            "summary_scope": summary_scope_str(self.diagnostics.summary_scope),
            "tool_version": self.metadata.tool_version,
        }))?)
    }

    pub fn render_sarif(
        &self,
        llm_suggestions: Option<&LlmSuggestionBundle>,
    ) -> Result<String, RenderError> {
        let diagnostics = self.visible_diagnostics();
        let rule_catalog = sarif_rule_catalog();
        let rule_ids = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.rule_id.as_str().to_owned())
            .collect::<BTreeSet<_>>();

        let mut rule_indices = BTreeMap::new();
        let rules = rule_ids
            .iter()
            .enumerate()
            .map(|(index, rule_id)| {
                rule_indices.insert(rule_id.clone(), index);
                let metadata = rule_catalog
                    .get(rule_id)
                    .cloned()
                    .unwrap_or(SarifRuleMetadata {
                        short_description: rule_id.clone(),
                        default_level: "warning",
                    });
                json!({
                    "id": rule_id,
                    "shortDescription": {
                        "text": metadata.short_description,
                    },
                    "defaultConfiguration": {
                        "level": metadata.default_level,
                    },
                })
            })
            .collect::<Vec<_>>();

        let results = diagnostics
            .into_iter()
            .map(|diagnostic| {
                let mut region = json!({
                    "startLine": diagnostic.location.start_line,
                    "endLine": diagnostic.location.end_line,
                });
                if let Some(column) = diagnostic.location.column {
                    region["startColumn"] = json!(column);
                }

                let mut kalos_properties = json!({
                    "kind": diagnostic_kind_str(diagnostic.kind),
                    "template_suggestion": template_suggestion_json(&diagnostic.template_suggestion),
                });
                if let Some(metric) = &diagnostic.metric {
                    kalos_properties["metric"] = metric_observation_json(metric);
                }
                if let Some(pattern) = &diagnostic.pattern {
                    kalos_properties["pattern"] = pattern_evidence_json(pattern);
                }
                if let Some(llm) =
                    llm_suggestions.and_then(|bundle| bundle.enrichments.get(&diagnostic.id))
                {
                    kalos_properties["llm_suggestion"] = llm_suggestion_json(llm);
                }

                json!({
                    "ruleId": diagnostic.rule_id.as_str(),
                    "ruleIndex": rule_indices[diagnostic.rule_id.as_str()],
                    "level": sarif_level(diagnostic.severity),
                    "message": {
                        "text": diagnostic.message,
                    },
                    "locations": [{
                        "physicalLocation": {
                            "artifactLocation": {
                                "uri": diagnostic.location.file_path.as_str(),
                            },
                            "region": region,
                        }
                    }],
                    "properties": {
                        "kalos": kalos_properties,
                    },
                })
            })
            .collect::<Vec<_>>();

        Ok(serde_json::to_string_pretty(&json!({
            "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
            "version": "2.1.0",
            "runs": [{
                "tool": {
                    "driver": {
                        "name": "kalos",
                        "version": self.metadata.tool_version,
                        "rules": rules,
                    }
                },
                "properties": {
                    "kalos": {
                        "metrics": self
                            .metrics
                            .iter()
                            .map(report_scope_metrics_json)
                            .collect::<Vec<_>>(),
                    },
                },
                "results": results,
            }]
        }))?)
    }

    fn visible_diagnostics(&self) -> Vec<&Diagnostic> {
        self.diagnostics
            .diagnostics
            .iter()
            .filter(|diagnostic| {
                self.view
                    .minimum_severity
                    .is_none_or(|severity| diagnostic.severity >= severity)
            })
            .collect()
    }

    fn human_score_line(&self) -> String {
        let mut score_parts = Vec::new();
        if matches!(
            self.view.requested_level,
            RequestedLevel::All | RequestedLevel::Function
        ) {
            score_parts.push(format!(
                "function: {}",
                display_level_score(self.scores.function)
            ));
        }
        if matches!(
            self.view.requested_level,
            RequestedLevel::All | RequestedLevel::Module
        ) {
            score_parts.push(format!(
                "module: {}",
                display_level_score(self.scores.module)
            ));
        }
        if matches!(
            self.view.requested_level,
            RequestedLevel::All | RequestedLevel::Project
        ) {
            score_parts.push(format!(
                "project: {}",
                display_level_score(self.scores.project)
            ));
        }

        format!(
            "Score: {}{}",
            display_overall_score(self.scores.overall),
            if score_parts.is_empty() {
                String::new()
            } else {
                format!("  ({})", score_parts.join(", "))
            }
        )
    }
}

pub fn project_metrics(
    metrics: &AnalysisMetrics,
    requested_level: RequestedLevel,
    catalog: &BTreeMap<crate::domains::MetricId, MetricMetadata>,
) -> Vec<ReportScopeMetrics> {
    let mut projected = Vec::new();
    if requested_level.includes(AnalysisLevel::Function) {
        projected.extend(
            metrics
                .function_metrics
                .iter()
                .map(|scope_metrics| project_scope_metrics(scope_metrics, catalog)),
        );
    }
    if requested_level.includes(AnalysisLevel::Module) {
        projected.extend(
            metrics
                .module_metrics
                .iter()
                .map(|scope_metrics| project_scope_metrics(scope_metrics, catalog)),
        );
    }
    if requested_level.includes(AnalysisLevel::Project) {
        projected.extend(
            metrics
                .project_metrics
                .iter()
                .map(|scope_metrics| project_scope_metrics(scope_metrics, catalog)),
        );
    }

    projected
}

pub fn project_diagnostics(
    diagnostics: &[Diagnostic],
    requested_level: RequestedLevel,
) -> Vec<Diagnostic> {
    diagnostics
        .iter()
        .filter(|diagnostic| requested_level.includes(diagnostic.primary_scope_id.level))
        .cloned()
        .collect()
}

pub fn project_scores(
    metrics: &AnalysisMetrics,
    requested_level: RequestedLevel,
) -> ProjectedScores {
    let overall_score = metrics.overall_score();

    match requested_level {
        RequestedLevel::All => ProjectedScores {
            overall: Some(overall_score.overall_score),
            function: overall_score.function_score,
            module: overall_score.module_score,
            project: overall_score.project_score,
        },
        RequestedLevel::Function => ProjectedScores {
            overall: overall_score.function_score,
            function: overall_score.function_score,
            module: None,
            project: None,
        },
        RequestedLevel::Module => ProjectedScores {
            overall: overall_score.module_score,
            function: None,
            module: overall_score.module_score,
            project: None,
        },
        RequestedLevel::Project => ProjectedScores {
            overall: overall_score.project_score,
            function: None,
            module: None,
            project: overall_score.project_score,
        },
    }
}

pub fn summary_scope_for(requested_level: RequestedLevel) -> SummaryScope {
    match requested_level {
        RequestedLevel::All => SummaryScope::WholeProject,
        RequestedLevel::Function | RequestedLevel::Module | RequestedLevel::Project => {
            SummaryScope::ListedDiagnostics
        }
    }
}

pub fn materialize_summary(diagnostics: &[Diagnostic]) -> DiagnosticSummary {
    diagnostics.iter().fold(
        DiagnosticSummary {
            error_count: 0,
            warning_count: 0,
            info_count: 0,
        },
        |mut summary, diagnostic| {
            match diagnostic.severity {
                Severity::Error => summary.error_count += 1,
                Severity::Warning => summary.warning_count += 1,
                Severity::Info => summary.info_count += 1,
            }
            summary
        },
    )
}

fn project_scope_metrics(
    scope_metrics: &ScopeMetrics,
    catalog: &BTreeMap<crate::domains::MetricId, MetricMetadata>,
) -> ReportScopeMetrics {
    ReportScopeMetrics {
        scope_id: scope_metrics.scope_id.clone(),
        scope_risk: scope_metrics.scope_risk,
        values: scope_metrics
            .values
            .iter()
            .map(|value| project_metric_value(value, catalog))
            .collect(),
    }
}

fn project_metric_value(
    value: &MetricValue,
    catalog: &BTreeMap<crate::domains::MetricId, MetricMetadata>,
) -> ReportMetricValue {
    ReportMetricValue {
        metric_id: value.metric_id.clone(),
        raw_value: value.raw_value,
        normalized_risk: value.normalized_risk,
        participation: catalog
            .get(&value.metric_id)
            .map(|metadata| metadata.participation)
            .unwrap_or(MetricParticipation::ReportOnly),
    }
}

fn format_location(location: &crate::domains::diagnostics::FileLocation) -> String {
    match location.column {
        Some(column) => format!(
            "{}:{}:{}",
            location.file_path.as_str(),
            location.start_line,
            column
        ),
        None => format!("{}:{}", location.file_path.as_str(), location.start_line),
    }
}

fn render_severity(severity: Severity, use_color: bool) -> String {
    let label = severity_str(severity);
    if !use_color {
        return label.to_owned();
    }

    let color = match severity {
        Severity::Error => "\u{1b}[31m",
        Severity::Warning => "\u{1b}[33m",
        Severity::Info => "\u{1b}[34m",
    };
    format!("{color}{label}\u{1b}[0m")
}

fn display_overall_score(score: Option<u8>) -> String {
    score
        .map(|value| format!("{value}/100"))
        .unwrap_or_else(|| "n/a".to_owned())
}

fn display_level_score(score: Option<u8>) -> String {
    score
        .map(|value| value.to_string())
        .unwrap_or_else(|| "n/a".to_owned())
}

fn report_scope_metrics_json(scope_metrics: &ReportScopeMetrics) -> Value {
    json!({
        "scope": scope_json(&scope_metrics.scope_id),
        "scope_risk": scope_metrics.scope_risk,
        "values": scope_metrics
            .values
            .iter()
            .map(|value| json!({
                "metric_id": value.metric_id.as_str(),
                "raw_value": value.raw_value,
                "normalized_risk": value.normalized_risk,
                "participation": participation_str(value.participation),
            }))
            .collect::<Vec<_>>(),
    })
}

fn diagnostic_json(
    diagnostic: &Diagnostic,
    llm_suggestions: Option<&LlmSuggestionBundle>,
) -> Value {
    let mut object = json!({
        "id": diagnostic.id.as_str(),
        "rule_id": diagnostic.rule_id.as_str(),
        "kind": diagnostic_kind_str(diagnostic.kind),
        "severity": severity_str(diagnostic.severity),
        "primary_scope": scope_json(&diagnostic.primary_scope_id),
        "location": {
            "file_path": diagnostic.location.file_path.as_str(),
            "start_line": diagnostic.location.start_line,
            "end_line": diagnostic.location.end_line,
            "column": diagnostic.location.column,
        },
        "message": diagnostic.message,
        "template_suggestion": template_suggestion_json(&diagnostic.template_suggestion),
    });

    if let Some(metric) = &diagnostic.metric {
        object["metric"] = metric_observation_json(metric);
    }
    if let Some(pattern) = &diagnostic.pattern {
        object["pattern"] = pattern_evidence_json(pattern);
    }
    if let Some(llm) = llm_suggestions.and_then(|bundle| bundle.enrichments.get(&diagnostic.id)) {
        object["llm_suggestion"] = llm_suggestion_json(llm);
    }

    object
}

fn metric_observation_json(metric: &crate::domains::diagnostics::MetricObservation) -> Value {
    json!({
        "metric_id": metric.metric_id.as_str(),
        "raw_value": metric.raw_value,
        "normalized_risk": metric.normalized_risk,
        "threshold": metric.threshold,
        "overflow_ratio": metric.overflow_ratio,
    })
}

fn pattern_evidence_json(pattern: &crate::domains::diagnostics::PatternEvidence) -> Value {
    json!({
        "pattern_type": pattern_type_str(pattern.pattern_type),
        "evidence_scopes": pattern
            .evidence_scopes
            .iter()
            .map(scope_json)
            .collect::<Vec<_>>(),
        "evidence_message": pattern.evidence_message,
    })
}

fn template_suggestion_json(suggestion: &crate::domains::diagnostics::TemplateSuggestion) -> Value {
    json!({
        "explanation": suggestion.explanation,
        "code_example": suggestion.code_example,
    })
}

fn llm_suggestion_json(suggestion: &crate::domains::diagnostics::LlmSuggestion) -> Value {
    json!({
        "explanation": suggestion.explanation,
        "code_example": suggestion.code_example,
    })
}

fn scope_json(scope_id: &ScopeId) -> Value {
    json!({
        "level": analysis_level_str(scope_id.level),
        "qualified_name": scope_id.qualified_name,
        "file_path": scope_id.file_path.as_str(),
    })
}

fn severity_str(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

fn diagnostic_kind_str(kind: DiagnosticKind) -> &'static str {
    match kind {
        DiagnosticKind::Metric => "metric",
        DiagnosticKind::Pattern => "pattern",
    }
}

fn pattern_type_str(pattern_type: PatternType) -> &'static str {
    match pattern_type {
        PatternType::GodUnit => "god_unit",
        PatternType::FeatureEnvy => "feature_envy",
        PatternType::CircularDependency => "circular_dependency",
    }
}

fn analysis_level_str(level: AnalysisLevel) -> &'static str {
    match level {
        AnalysisLevel::Function => "function",
        AnalysisLevel::Module => "module",
        AnalysisLevel::Project => "project",
    }
}

fn diagnostics_scope_str(scope: DiagnosticsScope) -> &'static str {
    match scope {
        DiagnosticsScope::WholeProject => "whole_project",
        DiagnosticsScope::AffectedOnly => "affected_only",
    }
}

fn summary_scope_str(scope: SummaryScope) -> &'static str {
    match scope {
        SummaryScope::ListedDiagnostics => "listed_diagnostics",
        SummaryScope::WholeProject => "whole_project",
    }
}

fn participation_str(participation: MetricParticipation) -> &'static str {
    match participation {
        MetricParticipation::ScoredAndDiagnosable => "scored_and_diagnosable",
        MetricParticipation::ReportOnly => "report_only",
    }
}

fn sarif_level(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "note",
    }
}

#[derive(Clone, Debug)]
struct SarifRuleMetadata {
    short_description: String,
    default_level: &'static str,
}

fn sarif_rule_catalog() -> BTreeMap<String, SarifRuleMetadata> {
    let mut catalog = builtin_metric_rules()
        .into_iter()
        .map(|rule| {
            (
                rule.id.as_str().to_owned(),
                SarifRuleMetadata {
                    short_description: rule.description,
                    default_level: "warning",
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

    for rule in builtin_pattern_rules() {
        catalog.insert(
            rule.id.as_str().to_owned(),
            SarifRuleMetadata {
                short_description: rule.description,
                default_level: sarif_level(rule.default_severity),
            },
        );
    }

    catalog
}

trait RequestedLevelExt {
    fn includes(self, level: AnalysisLevel) -> bool;
}

impl RequestedLevelExt for RequestedLevel {
    fn includes(self, level: AnalysisLevel) -> bool {
        match self {
            RequestedLevel::All => true,
            RequestedLevel::Function => level == AnalysisLevel::Function,
            RequestedLevel::Module => level == AnalysisLevel::Module,
            RequestedLevel::Project => level == AnalysisLevel::Project,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::Value;

    use super::{
        AnalysisReport, OutputFormat, ProjectedScores, ReportMetadata, ReportViewOptions,
        RequestedLevel, materialize_summary, project_diagnostics, project_scores,
        summary_scope_for,
    };
    use crate::domains::diagnostics::{
        Diagnostic, DiagnosticKind, DiagnosticsScope, FileLocation, MetricObservation,
        PatternEvidence, PatternType, TemplateSuggestion,
    };
    use crate::domains::metrics::{
        AnalysisMetrics, MetricMetadata, MetricParticipation, MetricValue, OverallScore,
        ScopeMetrics, builtin_metric_definitions, metric_catalog_from_definitions,
    };
    use crate::domains::{
        AnalysisLevel, DiagnosticId, FilePath, MetricId, RuleId, ScopeId, Severity,
    };

    #[test]
    fn level_projection_handles_all_requested_levels() {
        let metrics = fixture_metrics();
        let diagnostics = fixture_diagnostics();

        let all_report = project_report(RequestedLevel::All, None, &metrics, diagnostics.clone());
        assert_eq!(all_report.metrics.len(), 3);
        assert_eq!(all_report.diagnostics.diagnostics.len(), 4);
        assert_eq!(
            all_report.scores,
            ProjectedScores {
                overall: Some(44),
                function: Some(45),
                module: Some(20),
                project: Some(75),
            }
        );
        assert_eq!(
            all_report.diagnostics.summary_scope,
            crate::domains::diagnostics::SummaryScope::WholeProject
        );

        let function_report = project_report(
            RequestedLevel::Function,
            None,
            &metrics,
            diagnostics.clone(),
        );
        assert_eq!(function_report.metrics.len(), 1);
        assert!(
            function_report
                .metrics
                .iter()
                .all(|metric| metric.scope_id.level == AnalysisLevel::Function)
        );
        assert!(
            function_report
                .diagnostics
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.primary_scope_id.level == AnalysisLevel::Function)
        );
        assert_eq!(
            function_report.scores,
            ProjectedScores {
                overall: Some(45),
                function: Some(45),
                module: None,
                project: None,
            }
        );
        assert_eq!(function_report.diagnostics.summary.error_count, 0);
        assert_eq!(function_report.diagnostics.summary.warning_count, 1);
        assert_eq!(function_report.diagnostics.summary.info_count, 1);

        let module_report =
            project_report(RequestedLevel::Module, None, &metrics, diagnostics.clone());
        assert_eq!(module_report.metrics.len(), 1);
        assert!(
            module_report
                .metrics
                .iter()
                .all(|metric| metric.scope_id.level == AnalysisLevel::Module)
        );
        assert_eq!(
            module_report.scores,
            ProjectedScores {
                overall: Some(20),
                function: None,
                module: Some(20),
                project: None,
            }
        );

        let project_report = project_report(RequestedLevel::Project, None, &metrics, diagnostics);
        assert_eq!(project_report.metrics.len(), 1);
        assert!(
            project_report
                .metrics
                .iter()
                .all(|metric| metric.scope_id.level == AnalysisLevel::Project)
        );
        assert_eq!(
            project_report.scores,
            ProjectedScores {
                overall: Some(75),
                function: None,
                module: None,
                project: Some(75),
            }
        );
    }

    #[test]
    fn summary_materialization_counts_severities() {
        let diagnostics = fixture_diagnostics();
        let summary = materialize_summary(&diagnostics);

        assert_eq!(summary.error_count, 1);
        assert_eq!(summary.warning_count, 2);
        assert_eq!(summary.info_count, 1);
        assert_eq!(
            materialize_summary(&project_diagnostics(&diagnostics, RequestedLevel::Function)),
            crate::domains::diagnostics::DiagnosticSummary {
                error_count: 0,
                warning_count: 1,
                info_count: 1,
            }
        );
    }

    #[test]
    fn summary_uses_level_projection_before_severity_filtering() {
        let report = project_report(
            RequestedLevel::Function,
            Some(Severity::Error),
            &fixture_metrics(),
            fixture_diagnostics(),
        );

        assert!(report.visible_diagnostics().is_empty());
        assert_eq!(report.diagnostics.summary.error_count, 0);
        assert_eq!(report.diagnostics.summary.warning_count, 1);
        assert_eq!(report.diagnostics.summary.info_count, 1);
    }

    #[test]
    fn strict_exit_code_uses_unfiltered_summary_not_visible_diagnostics() {
        let report = AnalysisReport::project(
            ReportMetadata::new(vec![FilePath::from(".")], 10, "0.1.0", "1.0.0"),
            &fixture_metrics(),
            vec![fixture_warning_diagnostic()],
            DiagnosticsScope::WholeProject,
            ReportViewOptions {
                requested_level: RequestedLevel::All,
                output_format: OutputFormat::Json,
                strict: true,
                minimum_severity: Some(Severity::Error),
                verbose: false,
            },
            None,
        );

        assert!(report.visible_diagnostics().is_empty());
        assert_eq!(report.diagnostics.summary.warning_count, 1);
        assert_eq!(
            report.diagnostics.determine_exit_code(report.view.strict),
            crate::domains::diagnostics::ExitCode::DiagnosticFailure
        );
    }

    #[test]
    fn project_report_prefers_summary_override_when_provided() {
        let report = AnalysisReport::project(
            ReportMetadata::new(vec![FilePath::from(".")], 10, "0.1.0", "1.0.0"),
            &fixture_metrics(),
            vec![fixture_warning_diagnostic()],
            DiagnosticsScope::AffectedOnly,
            ReportViewOptions {
                requested_level: RequestedLevel::All,
                output_format: OutputFormat::Json,
                strict: false,
                minimum_severity: None,
                verbose: false,
            },
            Some(crate::domains::diagnostics::DiagnosticSummary {
                error_count: 2,
                warning_count: 3,
                info_count: 4,
            }),
        );

        assert_eq!(report.diagnostics.summary.error_count, 2);
        assert_eq!(report.diagnostics.summary.warning_count, 3);
        assert_eq!(report.diagnostics.summary.info_count, 4);
    }

    #[test]
    fn projected_scores_follow_requested_level() {
        let metrics = fixture_metrics();

        assert_eq!(
            project_scores(&metrics, RequestedLevel::Function),
            ProjectedScores {
                overall: Some(45),
                function: Some(45),
                module: None,
                project: None,
            }
        );
        assert_eq!(
            project_scores(&metrics, RequestedLevel::All),
            ProjectedScores {
                overall: Some(44),
                function: Some(45),
                module: Some(20),
                project: Some(75),
            }
        );
        assert_eq!(
            summary_scope_for(RequestedLevel::Project),
            crate::domains::diagnostics::SummaryScope::ListedDiagnostics
        );
    }

    #[test]
    fn json_output_has_required_fields_and_hides_non_target_levels() {
        let report = project_report(
            RequestedLevel::Function,
            Some(Severity::Warning),
            &fixture_metrics(),
            fixture_diagnostics(),
        );
        let rendered = report.render_json(None).expect("json should render");
        let parsed: Value = serde_json::from_str(&rendered).expect("json should parse");

        for field in [
            "schema_version",
            "analysis_targets",
            "scores",
            "metrics",
            "diagnostics",
            "diagnostics_scope",
            "summary",
            "summary_scope",
            "tool_version",
        ] {
            assert!(parsed.get(field).is_some(), "missing field `{field}`");
        }

        assert_eq!(parsed["schema_version"], "1.0.0");
        assert_eq!(parsed["diagnostics_scope"], "whole_project");
        assert_eq!(parsed["summary_scope"], "listed_diagnostics");
        assert_eq!(parsed["scores"]["overall"], 45);
        assert!(parsed["scores"]["module"].is_null());
        assert!(parsed["scores"]["project"].is_null());
        assert_eq!(
            parsed["metrics"]
                .as_array()
                .expect("metrics array")
                .iter()
                .filter_map(|entry| entry["scope"]["level"].as_str())
                .collect::<Vec<_>>(),
            vec!["function"]
        );
        assert_eq!(
            parsed["diagnostics"]
                .as_array()
                .expect("diagnostics array")
                .len(),
            1
        );
    }

    #[test]
    fn sarif_output_contains_rules_results_and_locations() {
        let report = project_report(
            RequestedLevel::All,
            None,
            &fixture_metrics(),
            fixture_diagnostics(),
        );
        let rendered = report.render_sarif(None).expect("sarif should render");
        let parsed: Value = serde_json::from_str(&rendered).expect("sarif should parse");
        let run = &parsed["runs"][0];
        let rules = run["tool"]["driver"]["rules"]
            .as_array()
            .expect("rules array");
        let results = run["results"].as_array().expect("results array");

        assert_eq!(parsed["version"], "2.1.0");
        assert!(!rules.is_empty());
        assert_eq!(results.len(), 4);
        assert!(rules.iter().any(|rule| rule["id"] == "KAL-F001"));
        assert!(results.iter().any(|result| result["ruleId"] == "KAL-F001"));

        let metric_result = results
            .iter()
            .find(|result| result["ruleId"] == "KAL-F001")
            .expect("metric result");
        assert_eq!(
            metric_result["locations"][0]["physicalLocation"]["region"]["startColumn"],
            3
        );

        let cross_scope_result = results
            .iter()
            .find(|result| result["ruleId"] == "KAL-PAT003")
            .expect("cross-scope result");
        assert!(
            cross_scope_result["locations"][0]["physicalLocation"]["region"]["startColumn"]
                .is_null()
        );
        assert!(cross_scope_result["properties"]["kalos"]["template_suggestion"].is_object());
    }

    #[test]
    fn renderers_include_report_only_metrics() {
        let plugin_metric_id = MetricId::from("P-F001");
        let report = AnalysisReport::project_with_metric_catalog(
            ReportMetadata::new(
                vec![FilePath::from("."), FilePath::from("src")],
                10,
                "0.1.0",
                "1.0.0",
            ),
            &fixture_metrics_with_plugin(plugin_metric_id.clone()),
            &fixture_metric_catalog_with_plugin(plugin_metric_id.clone()),
            fixture_diagnostics(),
            DiagnosticsScope::WholeProject,
            ReportViewOptions {
                requested_level: RequestedLevel::All,
                output_format: OutputFormat::Human,
                strict: false,
                minimum_severity: None,
                verbose: true,
            },
            None,
        );

        let human = report.render_human(None, false);
        assert!(human.contains("metric=P-F001"));
        assert!(human.contains("participation=report_only"));

        let json = report.render_json(None).expect("json should render");
        let json_value: Value = serde_json::from_str(&json).expect("json should parse");
        assert!(
            json_value["metrics"]
                .as_array()
                .expect("metrics array")
                .iter()
                .flat_map(|scope| scope["values"].as_array().expect("values array"))
                .any(|value| {
                    value["metric_id"] == "P-F001" && value["participation"] == "report_only"
                })
        );

        let sarif = report.render_sarif(None).expect("sarif should render");
        let sarif_value: Value = serde_json::from_str(&sarif).expect("sarif should parse");
        assert!(
            sarif_value["runs"][0]["properties"]["kalos"]["metrics"]
                .as_array()
                .expect("sarif metrics array")
                .iter()
                .flat_map(|scope| scope["values"].as_array().expect("values array"))
                .any(|value| {
                    value["metric_id"] == "P-F001" && value["participation"] == "report_only"
                })
        );
    }

    #[test]
    fn human_output_starts_with_analyzed_summary_line() {
        let report = AnalysisReport::project(
            ReportMetadata::new(vec![FilePath::from("src/")], 42, "0.1.0", "1.0.0"),
            &fixture_metrics(),
            fixture_diagnostics(),
            DiagnosticsScope::WholeProject,
            ReportViewOptions {
                requested_level: RequestedLevel::All,
                output_format: OutputFormat::Human,
                strict: false,
                minimum_severity: None,
                verbose: false,
            },
            None,
        );

        let rendered = report.render_human(None, false);

        assert!(rendered.starts_with("Analyzed 42 files in src/\n\n"));
    }

    #[test]
    fn human_output_summary_line_with_multiple_targets() {
        let report = AnalysisReport::project(
            ReportMetadata::new(
                vec![FilePath::from("src/"), FilePath::from("tests/")],
                42,
                "0.1.0",
                "1.0.0",
            ),
            &fixture_metrics(),
            fixture_diagnostics(),
            DiagnosticsScope::WholeProject,
            ReportViewOptions {
                requested_level: RequestedLevel::All,
                output_format: OutputFormat::Human,
                strict: false,
                minimum_severity: None,
                verbose: false,
            },
            None,
        );

        let rendered = report.render_human(None, false);

        assert!(rendered.starts_with("Analyzed 42 files in src/, tests/\n\n"));
    }

    #[test]
    fn human_output_omits_metrics_by_default() {
        let report = AnalysisReport::project(
            ReportMetadata::new(vec![FilePath::from("src/")], 42, "0.1.0", "1.0.0"),
            &fixture_metrics(),
            fixture_diagnostics(),
            DiagnosticsScope::WholeProject,
            ReportViewOptions {
                requested_level: RequestedLevel::All,
                output_format: OutputFormat::Human,
                strict: false,
                minimum_severity: None,
                verbose: false,
            },
            None,
        );

        let rendered = report.render_human(None, false);

        assert!(!rendered.contains("── Metrics ──"));
    }

    #[test]
    fn human_output_includes_metrics_when_verbose() {
        let report = AnalysisReport::project(
            ReportMetadata::new(vec![FilePath::from("src/")], 42, "0.1.0", "1.0.0"),
            &fixture_metrics(),
            fixture_diagnostics(),
            DiagnosticsScope::WholeProject,
            ReportViewOptions {
                requested_level: RequestedLevel::All,
                output_format: OutputFormat::Human,
                strict: false,
                minimum_severity: None,
                verbose: true,
            },
            None,
        );

        let rendered = report.render_human(None, false);

        assert!(rendered.contains("── Metrics ──"));
    }

    #[test]
    fn json_output_always_includes_metrics_regardless_of_verbose() {
        let report = AnalysisReport::project(
            ReportMetadata::new(vec![FilePath::from("src/")], 42, "0.1.0", "1.0.0"),
            &fixture_metrics(),
            fixture_diagnostics(),
            DiagnosticsScope::WholeProject,
            ReportViewOptions {
                requested_level: RequestedLevel::All,
                output_format: OutputFormat::Json,
                strict: false,
                minimum_severity: None,
                verbose: false,
            },
            None,
        );

        let rendered = report.render_json(None).expect("json should render");
        let parsed: Value = serde_json::from_str(&rendered).expect("json should parse");

        assert!(
            !parsed["metrics"]
                .as_array()
                .expect("metrics array")
                .is_empty()
        );
    }

    fn project_report(
        requested_level: RequestedLevel,
        minimum_severity: Option<Severity>,
        metrics: &AnalysisMetrics,
        diagnostics: Vec<Diagnostic>,
    ) -> AnalysisReport {
        AnalysisReport::project(
            ReportMetadata::new(
                vec![FilePath::from("."), FilePath::from("src")],
                10,
                "0.1.0",
                "1.0.0",
            ),
            metrics,
            diagnostics,
            DiagnosticsScope::WholeProject,
            ReportViewOptions {
                requested_level,
                output_format: OutputFormat::Json,
                strict: false,
                minimum_severity,
                verbose: false,
            },
            None,
        )
    }

    fn fixture_metrics() -> AnalysisMetrics {
        AnalysisMetrics {
            function_metrics: vec![ScopeMetrics {
                scope_id: ScopeId::new(AnalysisLevel::Function, "crate::f", "src/lib.rs"),
                scope_risk: 0.55,
                values: vec![MetricValue {
                    metric_id: MetricId::from("M-F001"),
                    raw_value: 1.0,
                    normalized_risk: 0.55,
                }],
            }],
            module_metrics: vec![ScopeMetrics {
                scope_id: ScopeId::new(AnalysisLevel::Module, "crate", "src/lib.rs"),
                scope_risk: 0.80,
                values: vec![MetricValue {
                    metric_id: MetricId::from("M-M001"),
                    raw_value: 4.0,
                    normalized_risk: 0.80,
                }],
            }],
            project_metrics: Some(ScopeMetrics {
                scope_id: ScopeId::new(AnalysisLevel::Project, "<project>", "."),
                scope_risk: 0.25,
                values: vec![MetricValue {
                    metric_id: MetricId::from("M-P001"),
                    raw_value: 1.0,
                    normalized_risk: 0.25,
                }],
            }),
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

    fn fixture_metrics_with_plugin(plugin_metric_id: MetricId) -> AnalysisMetrics {
        let mut metrics = fixture_metrics();
        metrics.function_metrics[0].values.push(MetricValue {
            metric_id: plugin_metric_id,
            raw_value: 99.0,
            normalized_risk: 1.0,
        });
        metrics
    }

    fn fixture_metric_catalog_with_plugin(
        plugin_metric_id: MetricId,
    ) -> BTreeMap<MetricId, MetricMetadata> {
        let definitions = builtin_metric_definitions();
        let definition_refs = definitions
            .iter()
            .map(|definition| definition.as_ref())
            .collect::<Vec<_>>();
        let mut catalog = metric_catalog_from_definitions(definition_refs);
        catalog.insert(
            plugin_metric_id,
            MetricMetadata {
                participation: MetricParticipation::ReportOnly,
                rule_binding: None,
            },
        );
        catalog
    }

    fn fixture_diagnostics() -> Vec<Diagnostic> {
        vec![
            fixture_warning_diagnostic(),
            Diagnostic {
                id: DiagnosticId::from("diag-function-info"),
                primary_scope_id: ScopeId::new(AnalysisLevel::Function, "crate::g", "src/lib.rs"),
                rule_id: RuleId::from("KAL-F002"),
                kind: DiagnosticKind::Metric,
                severity: Severity::Info,
                location: FileLocation {
                    file_path: FilePath::from("src/lib.rs"),
                    start_line: 20,
                    end_line: 25,
                    column: Some(3),
                },
                message: "function metric info".to_owned(),
                metric: Some(MetricObservation {
                    metric_id: MetricId::from("M-F002"),
                    raw_value: 2.0,
                    normalized_risk: 0.61,
                    threshold: 0.60,
                    overflow_ratio: 0.025,
                }),
                pattern: None,
                template_suggestion: TemplateSuggestion {
                    explanation: "split branches".to_owned(),
                    code_example: None,
                },
            },
            Diagnostic {
                id: DiagnosticId::from("diag-module-error"),
                primary_scope_id: ScopeId::new(AnalysisLevel::Module, "crate", "src/lib.rs"),
                rule_id: RuleId::from("KAL-M001"),
                kind: DiagnosticKind::Metric,
                severity: Severity::Error,
                location: FileLocation {
                    file_path: FilePath::from("src/lib.rs"),
                    start_line: 1,
                    end_line: 80,
                    column: Some(1),
                },
                message: "module metric error".to_owned(),
                metric: Some(MetricObservation {
                    metric_id: MetricId::from("M-M001"),
                    raw_value: 4.0,
                    normalized_risk: 0.80,
                    threshold: 0.50,
                    overflow_ratio: 0.60,
                }),
                pattern: None,
                template_suggestion: TemplateSuggestion {
                    explanation: "reduce fan-out".to_owned(),
                    code_example: None,
                },
            },
            Diagnostic {
                id: DiagnosticId::from("diag-project-pattern"),
                primary_scope_id: ScopeId::new(AnalysisLevel::Project, "<project>", "."),
                rule_id: RuleId::from("KAL-PAT003"),
                kind: DiagnosticKind::Pattern,
                severity: Severity::Warning,
                location: FileLocation {
                    file_path: FilePath::from("."),
                    start_line: 1,
                    end_line: 1,
                    column: None,
                },
                message: "circular dependency".to_owned(),
                metric: None,
                pattern: Some(PatternEvidence {
                    pattern_type: PatternType::CircularDependency,
                    evidence_scopes: vec![
                        ScopeId::new(AnalysisLevel::Module, "parser", "src/parser.rs"),
                        ScopeId::new(AnalysisLevel::Module, "lexer", "src/lexer.rs"),
                    ],
                    evidence_message: "parser -> lexer -> parser".to_owned(),
                }),
                template_suggestion: TemplateSuggestion {
                    explanation: "break the cycle".to_owned(),
                    code_example: None,
                },
            },
        ]
    }

    fn fixture_warning_diagnostic() -> Diagnostic {
        Diagnostic {
            id: DiagnosticId::from("diag-function-warning"),
            primary_scope_id: ScopeId::new(AnalysisLevel::Function, "crate::f", "src/lib.rs"),
            rule_id: RuleId::from("KAL-F001"),
            kind: DiagnosticKind::Metric,
            severity: Severity::Warning,
            location: FileLocation {
                file_path: FilePath::from("src/lib.rs"),
                start_line: 10,
                end_line: 12,
                column: Some(3),
            },
            message: "function metric warning".to_owned(),
            metric: Some(MetricObservation {
                metric_id: MetricId::from("M-F001"),
                raw_value: 1.0,
                normalized_risk: 0.8,
                threshold: 0.55,
                overflow_ratio: 0.25,
            }),
            pattern: None,
            template_suggestion: TemplateSuggestion {
                explanation: "extract helper".to_owned(),
                code_example: None,
            },
        }
    }
}
