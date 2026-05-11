use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde_json::{Value, json};

pub const SARIF_SCHEMA_URL: &str = "https://json.schemastore.org/sarif-2.1.0.json";
pub const SARIF_VERSION: &str = "2.1.0";
pub const OUTCOME_PASSED: &str = "passed";
pub const OUTCOME_DIAGNOSTICS_FAILED: &str = "diagnostics_failed";
pub const OUTCOME_EXPECTED_SKIP: &str = "expected_skip";
pub const OUTCOME_INPUT_ERROR: &str = "input_error";
pub const OUTCOME_INFRASTRUCTURE_ERROR: &str = "infrastructure_error";
pub const OUTCOME_TOOL_ERROR: &str = "tool_error";
const KAL_M003_GATE_GUIDANCE_PREFIX: &str = "module/all gate guidance: KAL-M003";
const MAX_ALL_LEVEL_FUNCTION_METRIC_DIAGNOSTICS_PER_RULE: usize = 25;
const MAX_FUNCTION_LEVEL_HUMAN_FUNCTION_METRIC_DIAGNOSTICS_PER_RULE: usize = 25;

use crate::domains::diagnostics::{
    Diagnostic, DiagnosticKind, DiagnosticReport, DiagnosticSummary, DiagnosticsScope, ExitCode,
    LlmSuggestionBundle, PatternEdgeProvenance, PatternType, SummaryScope, builtin_metric_rules,
    builtin_pattern_rules,
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

#[derive(Clone, Debug, PartialEq)]
pub struct ReportViewOptions {
    pub requested_level: RequestedLevel,
    pub output_format: OutputFormat,
    pub strict: bool,
    pub minimum_severity: Option<Severity>,
    pub min_risk: Option<f64>,
    pub verbose: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScoreNote {
    pub level: AnalysisLevel,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectedScores {
    pub overall: Option<u8>,
    pub function: Option<u8>,
    pub module: Option<u8>,
    pub project: Option<u8>,
    pub score_notes: Vec<ScoreNote>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffExecutionContext {
    pub requested_base_ref: String,
    pub base_status: DiffBaseStatus,
    pub effective_mode: EffectiveAnalysisMode,
    pub fallback_reason: Option<String>,
    pub changed_file_count: Option<usize>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DiffBaseStatus {
    Resolved,
    NotEvaluated,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EffectiveAnalysisMode {
    Diff,
    Full,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReportMetricValue {
    pub metric_id: crate::domains::MetricId,
    pub name: Option<String>,
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
    pub analysis_warnings: Vec<String>,
    pub diff_execution: Option<DiffExecutionContext>,
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
        analysis_warnings: Vec<String>,
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
            analysis_warnings,
        )
    }

    pub fn project_with_metric_catalog(
        metadata: ReportMetadata,
        metrics: &AnalysisMetrics,
        metric_catalog: &BTreeMap<crate::domains::MetricId, MetricMetadata>,
        diagnostics: Vec<Diagnostic>,
        diagnostics_scope: DiagnosticsScope,
        view: ReportViewOptions,
        analysis_warnings: Vec<String>,
    ) -> Self {
        let projected_metrics = project_metrics(metrics, view.requested_level, metric_catalog);
        let projected_diagnostics = project_diagnostics(&diagnostics, view.requested_level);
        let summary_scope = summary_scope_for(view.requested_level);
        let file_count = metadata.file_count;
        let summary = materialize_summary(match summary_scope {
            SummaryScope::WholeProject => &diagnostics,
            SummaryScope::ListedDiagnostics => &projected_diagnostics,
        });

        let analysis_warnings = append_function_metric_flood_guidance(
            analysis_warnings,
            view.requested_level,
            diagnostics.len(),
            projected_diagnostics.len(),
        );
        let analysis_warnings = append_module_gate_guidance(
            analysis_warnings,
            view.requested_level,
            &projected_diagnostics,
        );

        Self {
            metadata,
            view: view.clone(),
            scores: project_scores(metrics, view.requested_level, file_count),
            analysis_warnings,
            diff_execution: None,
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
        let (human_diagnostics, function_metric_flood_summary) =
            human_visible_diagnostics(&diagnostics, self.view.requested_level);
        let summary = self.visible_summary();
        let analysis_targets = self
            .metadata
            .analysis_targets
            .iter()
            .map(FilePath::as_str)
            .collect::<Vec<_>>()
            .join(", ");

        if self.is_full_analysis_fallback()
            && self.diagnostics.diagnostics_scope == DiagnosticsScope::AffectedOnly
        {
            let noun = if self.metadata.file_count == 1 {
                "file"
            } else {
                "files"
            };
            let _ = writeln!(
                output,
                "Full analysis fallback completed; showing {} changed {} in {}",
                self.metadata.file_count, noun, analysis_targets
            );
        } else if self.is_full_analysis_fallback() {
            let _ = writeln!(
                output,
                "Full analysis fallback completed; analyzed {} files in {}",
                self.metadata.file_count, analysis_targets
            );
        } else {
            let _ = writeln!(
                output,
                "Analyzed {} files in {}",
                self.metadata.file_count, analysis_targets
            );
        }
        if !human_diagnostics.is_empty() {
            output.push('\n');
        }

        for diagnostic in &human_diagnostics {
            let _ = writeln!(
                output,
                "{}  {}[{}]  [{}] {}{}",
                format_location(&diagnostic.location),
                render_severity(diagnostic.severity, use_color),
                diagnostic.rule_id.as_str(),
                diagnostic_kind_str(diagnostic.kind),
                diagnostic.message,
                human_test_module_structural_tag(diagnostic)
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
            write_human_edge_provenance(&mut output, diagnostic);

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

        for warning in &self.analysis_warnings {
            let _ = writeln!(output, "note: {warning}");
        }
        if let Some(summary) = function_metric_flood_summary {
            let _ = writeln!(
                output,
                "note: function metric flood control: hid {} lower-priority function metric diagnostic(s) in --level function human output ({}); JSON/SARIF keep the full diagnostic inventory.",
                summary.total_hidden,
                summary.hidden_by_rule_summary()
            );
        }
        if let Some(diff_execution) = &self.diff_execution {
            let _ = writeln!(
                output,
                "note: diff requested base {}; base status {}; effective analysis {}{}",
                diff_execution.requested_base_ref,
                diff_base_status_str(diff_execution.base_status),
                effective_analysis_mode_str(diff_execution.effective_mode),
                diff_execution
                    .changed_file_count
                    .map(|count| format!("; changed files {count}"))
                    .unwrap_or_default()
            );
        }
        if let Some(note) = human_test_module_structural_note(&human_diagnostics) {
            let _ = writeln!(output, "{note}");
        }
        let _ = writeln!(output, "── Summary ──────────────────────────");
        let _ = writeln!(
            output,
            "outcome: {}",
            outcome_for_exit_code(self.diagnostics.determine_exit_code(self.view.strict))
        );
        let _ = writeln!(output, "{}", self.human_score_line());
        for note in self.human_score_notes() {
            let _ = writeln!(output, "{note}");
        }
        if let Some(deduction_line) = self.human_project_deduction_line() {
            let _ = writeln!(output, "{deduction_line}");
        }
        let _ = writeln!(
            output,
            "{} errors, {} warnings, {} info",
            summary.error_count, summary.warning_count, summary.info_count
        );
        if self.view.verbose && !self.metrics.is_empty() {
            let (visible_metrics, hidden_scope_count) = self.visible_verbose_metrics();
            let _ = writeln!(output, "\n── Metrics ───────────────────────────");
            for scope_metrics in visible_metrics {
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
            if hidden_scope_count > 0 {
                let _ = writeln!(
                    output,
                    "({hidden_scope_count} scopes with risk below threshold hidden; use --min-risk 0 to show all)"
                );
            }
        }

        output
    }

    fn is_full_analysis_fallback(&self) -> bool {
        self.analysis_warnings
            .iter()
            .any(|warning| warning.contains("falling back to full analysis"))
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
        let visible_summary = self.visible_summary();
        let metrics = self
            .metrics
            .iter()
            .map(report_scope_metrics_json)
            .collect::<Vec<_>>();

        Ok(serde_json::to_string_pretty(&json!({
            "schema_version": self.metadata.schema_version,
            "outcome": outcome_for_exit_code(self.diagnostics.determine_exit_code(self.view.strict)),
            "analysis_targets": self
                .metadata
                .analysis_targets
                .iter()
                .map(FilePath::as_str)
                .collect::<Vec<_>>(),
            "files_analyzed": self.metadata.file_count,
            "analysis_warnings": self.analysis_warnings,
            "diff_execution": self.diff_execution.as_ref().map(diff_execution_json),
            "scores": {
                "overall": self.scores.overall,
                "function": self.scores.function,
                "module": self.scores.module,
                "project": self.scores.project,
                "score_notes": self
                    .scores
                    .score_notes
                    .iter()
                    .map(score_note_json)
                    .collect::<Vec<_>>(),
            },
            "metrics": metrics,
            "diagnostics": diagnostics,
            "diagnostics_scope": diagnostics_scope_str(self.diagnostics.diagnostics_scope),
            "summary": {
                "error_count": visible_summary.error_count,
                "warning_count": visible_summary.warning_count,
                "info_count": visible_summary.info_count,
            },
            "summary_scope": summary_scope_str(self.diagnostics.summary_scope),
            "tool_version": self.metadata.tool_version,
        }))?)
    }

    pub fn render_sarif(
        &self,
        llm_suggestions: Option<&LlmSuggestionBundle>,
    ) -> Result<String, RenderError> {
        let visible_summary = self.visible_summary();
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
                if let Some(classification) = evaluation_artifact_classification_json(diagnostic) {
                    kalos_properties["evaluation_artifact"] = classification;
                }
                if let Some(metric) = &diagnostic.metric {
                    kalos_properties["metric"] = metric_observation_json(metric);
                }
                if let Some(pattern) = &diagnostic.pattern {
                    kalos_properties["pattern"] = pattern_evidence_json(pattern);
                }
                if !diagnostic.edge_provenance.is_empty() {
                    kalos_properties["edge_provenance"] = diagnostic
                        .edge_provenance
                        .iter()
                        .map(edge_provenance_json)
                        .collect::<Vec<_>>()
                        .into();
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
            "$schema": SARIF_SCHEMA_URL,
            "version": SARIF_VERSION,
            "runs": [{
                "tool": {
                    "driver": {
                        "name": "kalos",
                        "version": self.metadata.tool_version,
                        "rules": rules,
                    }
                },
                "properties": {
                    "analysis_warnings": self.analysis_warnings,
                    "diff_execution": self.diff_execution.as_ref().map(diff_execution_json),
                    "kalos": {
                        "schema_version": self.metadata.schema_version,
                        "outcome": outcome_for_exit_code(self.diagnostics.determine_exit_code(self.view.strict)),
                        "tool_version": self.metadata.tool_version,
                        "files_analyzed": self.metadata.file_count,
                        "diagnostics_scope": diagnostics_scope_str(self.diagnostics.diagnostics_scope),
                        "summary_scope": summary_scope_str(self.diagnostics.summary_scope),
                        "scores": {
                            "overall": self.scores.overall,
                            "function": self.scores.function,
                            "module": self.scores.module,
                            "project": self.scores.project,
                            "score_notes": self
                                .scores
                                .score_notes
                                .iter()
                                .map(score_note_json)
                                .collect::<Vec<_>>(),
                        },
                        "metrics": self
                            .metrics
                            .iter()
                            .map(report_scope_metrics_json)
                            .collect::<Vec<_>>(),
                        "summary": {
                            "error_count": visible_summary.error_count,
                            "warning_count": visible_summary.warning_count,
                            "info_count": visible_summary.info_count,
                        },
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

    fn visible_summary(&self) -> DiagnosticSummary {
        if self.view.minimum_severity.is_none() {
            return self.diagnostics.summary.clone();
        }

        let mut summary = DiagnosticSummary {
            error_count: 0,
            warning_count: 0,
            info_count: 0,
        };

        for diagnostic in self.visible_diagnostics() {
            match diagnostic.severity {
                Severity::Error => summary.error_count += 1,
                Severity::Warning => summary.warning_count += 1,
                Severity::Info => summary.info_count += 1,
            }
        }

        summary
    }

    fn visible_verbose_metrics(&self) -> (Vec<&ReportScopeMetrics>, usize) {
        let visible_metrics = self
            .metrics
            .iter()
            .filter(|scope_metrics| {
                self.view
                    .min_risk
                    .map_or(scope_metrics.scope_risk > 0.0, |threshold| {
                        scope_metrics.scope_risk >= threshold
                    })
            })
            .collect::<Vec<_>>();
        let hidden_scope_count = self.metrics.len().saturating_sub(visible_metrics.len());

        (visible_metrics, hidden_scope_count)
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

    fn human_score_notes(&self) -> Vec<String> {
        self.scores
            .score_notes
            .iter()
            .filter(|note| self.view.requested_level.includes(note.level))
            .map(|note| {
                format!(
                    "note: {} score is not available — {}",
                    analysis_level_str(note.level),
                    note.reason
                )
            })
            .collect()
    }

    fn human_project_deduction_line(&self) -> Option<String> {
        let project_score = self.scores.project?;
        if project_score >= 100 {
            return None;
        }

        let project_scope = self
            .metrics
            .iter()
            .find(|metrics| metrics.scope_id.level == AnalysisLevel::Project)?;
        let factors = project_scope
            .values
            .iter()
            .filter(|value| {
                value.normalized_risk > 0.0
                    && value.participation == MetricParticipation::ScoredAndDiagnosable
            })
            .map(|value| match &value.name {
                Some(name) => format!(
                    "{} ({}) = {:.3}",
                    value.metric_id.as_str(),
                    name,
                    value.raw_value
                ),
                None => format!("{} = {:.3}", value.metric_id.as_str(), value.raw_value),
            })
            .collect::<Vec<_>>();

        if factors.is_empty() {
            return None;
        }

        Some(format!("  project: {}", factors.join(", ")))
    }
}

pub fn render_sarif_error_document(
    message: &str,
    cause: Option<&str>,
    tool_version: &str,
    exit_code: i64,
    error_class: &str,
) -> String {
    let outcome = outcome_for_error_class(error_class);
    let mut notification = json!({
        "level": "error",
        "message": { "text": message },
        "properties": {
            "error_class": error_class,
            "outcome": outcome,
        },
    });
    if let Some(cause) = cause {
        notification["properties"]["cause"] = json!(cause);
    }

    let mut kalos_properties = json!({
        "error": true,
        "error_class": error_class,
        "outcome": outcome,
        "message": message,
    });
    if let Some(cause) = cause {
        kalos_properties["cause"] = json!(cause);
    }

    let document = json!({
        "$schema": SARIF_SCHEMA_URL,
        "version": SARIF_VERSION,
        "runs": [{
            "tool": {
                "driver": {
                    "name": "kalos",
                    "version": tool_version,
                    "rules": [],
                }
            },
            "invocations": [{
                "executionSuccessful": false,
                "exitCode": exit_code,
                "exitCodeDescription": error_class_description(error_class),
                "toolExecutionNotifications": [notification],
            }],
            "results": [],
            "properties": {
                "kalos": kalos_properties,
            },
        }],
    });

    serde_json::to_string_pretty(&document).expect("SARIF error document should serialize")
}

fn error_class_description(error_class: &str) -> &'static str {
    match error_class {
        "codeql_infrastructure" => "CodeQL infrastructure error",
        "codeql_extraction" => "CodeQL extraction error",
        "expected_skip" => "expected skip",
        "input_error" => "input error",
        _ => "tool error",
    }
}

pub fn outcome_for_error_class(error_class: &str) -> &'static str {
    match error_class {
        "expected_skip" => OUTCOME_EXPECTED_SKIP,
        "input_error" => OUTCOME_INPUT_ERROR,
        "codeql_infrastructure" | "codeql_extraction" => OUTCOME_INFRASTRUCTURE_ERROR,
        _ => OUTCOME_TOOL_ERROR,
    }
}

fn outcome_for_exit_code(exit_code: ExitCode) -> &'static str {
    match exit_code {
        ExitCode::Success => OUTCOME_PASSED,
        ExitCode::DiagnosticFailure => OUTCOME_DIAGNOSTICS_FAILED,
        ExitCode::ToolError => OUTCOME_TOOL_ERROR,
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
    let projected = diagnostics
        .iter()
        .filter(|diagnostic| requested_level.includes(diagnostic.primary_scope_id.level))
        .cloned()
        .collect::<Vec<_>>();

    if requested_level == RequestedLevel::All {
        cap_all_level_function_metric_diagnostics(projected)
    } else {
        projected
    }
}

pub fn project_scores(
    metrics: &AnalysisMetrics,
    requested_level: RequestedLevel,
    file_count: usize,
) -> ProjectedScores {
    if file_count == 0 {
        return ProjectedScores {
            overall: None,
            function: None,
            module: None,
            project: None,
            score_notes: vec![ScoreNote {
                level: AnalysisLevel::Project,
                reason: "no source files were analyzed".to_owned(),
            }],
        };
    }

    let overall_score = metrics.overall_score();
    let function = requested_level
        .includes(AnalysisLevel::Function)
        .then_some(overall_score.function_score)
        .flatten();
    let module = requested_level
        .includes(AnalysisLevel::Module)
        .then_some(overall_score.module_score)
        .flatten();
    let project = requested_level
        .includes(AnalysisLevel::Project)
        .then_some(overall_score.project_score)
        .flatten();

    let overall = match requested_level {
        RequestedLevel::All => Some(overall_score.overall_score),
        RequestedLevel::Function => function,
        RequestedLevel::Module => module,
        RequestedLevel::Project => project,
    };

    ProjectedScores {
        overall,
        function,
        module,
        project,
        score_notes: collect_score_notes(requested_level, function, module, project),
    }
}

fn append_module_gate_guidance(
    mut analysis_warnings: Vec<String>,
    requested_level: RequestedLevel,
    diagnostics: &[Diagnostic],
) -> Vec<String> {
    if !requested_level.includes(AnalysisLevel::Module) {
        return analysis_warnings;
    }

    let kal_m003_diagnostics = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.rule_id.as_str() == "KAL-M003")
        .collect::<Vec<_>>();
    if kal_m003_diagnostics.is_empty() {
        return analysis_warnings;
    }

    let thresholds = kal_m003_diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.metric.as_ref().map(|metric| metric.threshold))
        .map(|threshold| format!("{threshold:.6}"))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    let threshold_clause = if thresholds.is_empty() {
        "configured threshold unavailable".to_owned()
    } else {
        format!("configured threshold(s) {thresholds}")
    };
    let warning = format!(
        "{KAL_M003_GATE_GUIDANCE_PREFIX} reported {} module instability diagnostic(s); use --level project as the baseline CI gate, and treat module/all as a domain-owner architecture review gate. Before failing that gate, inspect dependency direction, owner boundaries, and {threshold_clause}.",
        kal_m003_diagnostics.len()
    );

    if !analysis_warnings
        .iter()
        .any(|existing| existing == &warning)
    {
        analysis_warnings.push(warning);
    }
    analysis_warnings
}

fn append_function_metric_flood_guidance(
    mut analysis_warnings: Vec<String>,
    requested_level: RequestedLevel,
    original_count: usize,
    projected_count: usize,
) -> Vec<String> {
    if requested_level != RequestedLevel::All || projected_count >= original_count {
        return analysis_warnings;
    }

    let hidden_count = original_count - projected_count;
    let warning = format!(
        "function metric flood control: hid {hidden_count} lower-priority function metric diagnostic(s) in --level all; showing at most {MAX_ALL_LEVEL_FUNCTION_METRIC_DIAGNOSTICS_PER_RULE} per function metric rule. Use --level function for the full function diagnostic inventory."
    );
    if !analysis_warnings
        .iter()
        .any(|existing| existing == &warning)
    {
        analysis_warnings.push(warning);
    }
    analysis_warnings
}

struct FunctionMetricFloodSummary {
    total_hidden: usize,
    hidden_by_rule: BTreeMap<String, usize>,
}

impl FunctionMetricFloodSummary {
    fn hidden_by_rule_summary(&self) -> String {
        self.hidden_by_rule
            .iter()
            .map(|(rule_id, hidden_count)| format!("{rule_id}: {hidden_count} hidden"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn human_visible_diagnostics<'a>(
    diagnostics: &[&'a Diagnostic],
    requested_level: RequestedLevel,
) -> (Vec<&'a Diagnostic>, Option<FunctionMetricFloodSummary>) {
    if requested_level != RequestedLevel::Function {
        return (diagnostics.to_vec(), None);
    }

    let mut non_function_metric_diagnostics = Vec::new();
    let mut function_metric_diagnostics_by_rule: BTreeMap<String, Vec<&Diagnostic>> =
        BTreeMap::new();

    for diagnostic in diagnostics {
        if is_function_metric_diagnostic(diagnostic) {
            function_metric_diagnostics_by_rule
                .entry(diagnostic.rule_id.as_str().to_owned())
                .or_default()
                .push(*diagnostic);
        } else {
            non_function_metric_diagnostics.push(*diagnostic);
        }
    }

    if function_metric_diagnostics_by_rule.is_empty() {
        return (diagnostics.to_vec(), None);
    }

    let mut visible_function_metric_diagnostics = Vec::new();
    let mut hidden_by_rule = BTreeMap::new();
    let mut total_hidden = 0;

    for (rule_id, rule_diagnostics) in function_metric_diagnostics_by_rule.iter_mut() {
        rule_diagnostics.sort_by(|left, right| compare_function_metric_priority(left, right));
        let visible_count = rule_diagnostics
            .len()
            .min(MAX_FUNCTION_LEVEL_HUMAN_FUNCTION_METRIC_DIAGNOSTICS_PER_RULE);
        visible_function_metric_diagnostics
            .extend(rule_diagnostics.iter().take(visible_count).copied());

        let hidden_count = rule_diagnostics.len() - visible_count;
        if hidden_count > 0 {
            hidden_by_rule.insert(rule_id.clone(), hidden_count);
            total_hidden += hidden_count;
        }
    }

    non_function_metric_diagnostics
        .sort_by(|left, right| compare_diagnostic_output_order(left, right));

    let mut visible = visible_function_metric_diagnostics;
    visible.extend(non_function_metric_diagnostics);
    visible.sort_by(|left, right| compare_diagnostic_output_order(left, right));
    let summary = (total_hidden > 0).then_some(FunctionMetricFloodSummary {
        total_hidden,
        hidden_by_rule,
    });

    (visible, summary)
}

fn cap_all_level_function_metric_diagnostics(diagnostics: Vec<Diagnostic>) -> Vec<Diagnostic> {
    let mut non_function_metric_diagnostics = Vec::new();
    let mut function_metric_diagnostics_by_rule: BTreeMap<String, Vec<Diagnostic>> =
        BTreeMap::new();

    for diagnostic in diagnostics {
        if is_function_metric_diagnostic(&diagnostic) {
            function_metric_diagnostics_by_rule
                .entry(diagnostic.rule_id.as_str().to_owned())
                .or_default()
                .push(diagnostic);
        } else {
            non_function_metric_diagnostics.push(diagnostic);
        }
    }

    let mut capped = non_function_metric_diagnostics;
    for diagnostics in function_metric_diagnostics_by_rule.values_mut() {
        diagnostics.sort_by(compare_function_metric_priority);
        capped.extend(
            diagnostics
                .iter()
                .take(MAX_ALL_LEVEL_FUNCTION_METRIC_DIAGNOSTICS_PER_RULE)
                .cloned(),
        );
    }
    capped.sort_by(compare_diagnostic_output_order);
    capped
}

fn is_function_metric_diagnostic(diagnostic: &Diagnostic) -> bool {
    diagnostic.primary_scope_id.level == AnalysisLevel::Function
        && diagnostic.kind == DiagnosticKind::Metric
}

fn compare_function_metric_priority(left: &Diagnostic, right: &Diagnostic) -> std::cmp::Ordering {
    right
        .severity
        .cmp(&left.severity)
        .then_with(|| metric_overflow_ratio(right).total_cmp(&metric_overflow_ratio(left)))
        .then_with(|| metric_normalized_risk(right).total_cmp(&metric_normalized_risk(left)))
        .then_with(|| compare_diagnostic_output_order(left, right))
}

fn compare_diagnostic_output_order(left: &Diagnostic, right: &Diagnostic) -> std::cmp::Ordering {
    (
        left.primary_scope_id.level,
        left.location.file_path.as_str(),
        left.location.start_line,
        left.location.end_line,
        left.rule_id.as_str(),
        left.primary_scope_id.qualified_name.as_str(),
        left.id.as_str(),
    )
        .cmp(&(
            right.primary_scope_id.level,
            right.location.file_path.as_str(),
            right.location.start_line,
            right.location.end_line,
            right.rule_id.as_str(),
            right.primary_scope_id.qualified_name.as_str(),
            right.id.as_str(),
        ))
}

fn metric_overflow_ratio(diagnostic: &Diagnostic) -> f64 {
    diagnostic
        .metric
        .as_ref()
        .map(|metric| metric.overflow_ratio)
        .unwrap_or(0.0)
}

fn metric_normalized_risk(diagnostic: &Diagnostic) -> f64 {
    diagnostic
        .metric
        .as_ref()
        .map(|metric| metric.normalized_risk)
        .unwrap_or(0.0)
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
    let metadata = catalog.get(&value.metric_id);
    ReportMetricValue {
        metric_id: value.metric_id.clone(),
        name: metadata.and_then(|metadata| metadata.name.clone()),
        raw_value: value.raw_value,
        normalized_risk: value.normalized_risk,
        participation: metadata
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

fn human_test_module_structural_tag(diagnostic: &Diagnostic) -> &'static str {
    if is_test_module_structural_diagnostic(diagnostic) {
        " [test module structural risk]"
    } else {
        ""
    }
}

fn human_test_module_structural_note(diagnostics: &[&Diagnostic]) -> Option<String> {
    let count = diagnostics
        .iter()
        .filter(|diagnostic| is_test_module_structural_diagnostic(diagnostic))
        .count();
    if count == 0 {
        return None;
    }

    let noun = if count == 1 {
        "diagnostic"
    } else {
        "diagnostics"
    };
    Some(format!(
        "note: {count} test-module structural {noun} shown separately; these KAL-M001/KAL-M003 findings are test risk from --include-tests, not production module risk"
    ))
}

fn is_test_module_structural_diagnostic(diagnostic: &Diagnostic) -> bool {
    diagnostic.primary_scope_id.level == AnalysisLevel::Module
        && matches!(diagnostic.rule_id.as_str(), "KAL-M001" | "KAL-M003")
        && is_test_file(diagnostic.location.file_path.as_str())
}

fn is_test_file(path: &str) -> bool {
    let file_name = path.rsplit('/').next().unwrap_or(path);

    path.starts_with("tests/")
        || path.contains("/tests/")
        || path.starts_with("__tests__/")
        || path.contains("/__tests__/")
        || file_name.contains("_test.")
        || file_name.contains(".test.")
        || file_name.contains(".spec.")
        || (file_name.starts_with("test_") && file_name.ends_with(".py"))
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
    if !diagnostic.edge_provenance.is_empty() {
        object["edge_provenance"] = diagnostic
            .edge_provenance
            .iter()
            .map(edge_provenance_json)
            .collect::<Vec<_>>()
            .into();
    }
    if let Some(llm) = llm_suggestions.and_then(|bundle| bundle.enrichments.get(&diagnostic.id)) {
        object["llm_suggestion"] = llm_suggestion_json(llm);
    }
    if let Some(classification) = evaluation_artifact_classification_json(diagnostic) {
        object["evaluation_artifact"] = classification;
    }

    object
}

fn evaluation_artifact_classification_json(diagnostic: &Diagnostic) -> Option<Value> {
    if !is_untriaged_domain_debt_diagnostic(diagnostic) {
        return None;
    }

    Some(json!({
        "classification": "target_repo_quality_finding",
        "triage_status": "untriaged_domain_debt",
        "confidence": "low",
    }))
}

fn is_untriaged_domain_debt_diagnostic(diagnostic: &Diagnostic) -> bool {
    match diagnostic.rule_id.as_str() {
        "KAL-M001" | "KAL-M003" => is_production_module_diagnostic(diagnostic),
        "KAL-PAT003" => true,
        _ => false,
    }
}

fn is_production_module_diagnostic(diagnostic: &Diagnostic) -> bool {
    diagnostic.primary_scope_id.level == AnalysisLevel::Module
        && !is_test_file(diagnostic.primary_scope_id.file_path.as_str())
        && !is_test_file(diagnostic.location.file_path.as_str())
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
        "edge_provenance": pattern
            .edge_provenance
            .iter()
            .map(edge_provenance_json)
            .collect::<Vec<_>>(),
    })
}

fn edge_provenance_json(edge: &PatternEdgeProvenance) -> Value {
    json!({
        "source_scope": scope_json(&edge.source_scope),
        "target_scope": scope_json(&edge.target_scope),
        "source_file_path": edge.source_file_path.as_str(),
        "target_file_path": edge.target_file_path.as_str(),
        "source_start_line": edge.source_start_line,
        "source_end_line": edge.source_end_line,
        "target_start_line": edge.target_start_line,
        "target_end_line": edge.target_end_line,
        "source_is_test": edge.source_is_test,
        "target_is_test": edge.target_is_test,
    })
}

fn write_human_edge_provenance(output: &mut String, diagnostic: &Diagnostic) {
    for edge in &diagnostic.edge_provenance {
        let _ = writeln!(
            output,
            "  edge {} -> {} ({}:{}-{} -> {}:{}-{})",
            edge.source_scope.qualified_name,
            edge.target_scope.qualified_name,
            edge.source_file_path.as_str(),
            edge.source_start_line,
            edge.source_end_line,
            edge.target_file_path.as_str(),
            edge.target_start_line,
            edge.target_end_line
        );
    }
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

fn score_note_json(note: &ScoreNote) -> Value {
    json!({
        "level": analysis_level_str(note.level),
        "reason": note.reason,
    })
}

fn diff_execution_json(diff_execution: &DiffExecutionContext) -> Value {
    json!({
        "requested_mode": "diff",
        "requested_base_ref": diff_execution.requested_base_ref,
        "base_status": diff_base_status_str(diff_execution.base_status),
        "effective_mode": effective_analysis_mode_str(diff_execution.effective_mode),
        "fallback_reason": diff_execution.fallback_reason,
        "changed_file_count": diff_execution.changed_file_count,
    })
}

fn diff_base_status_str(status: DiffBaseStatus) -> &'static str {
    match status {
        DiffBaseStatus::Resolved => "resolved",
        DiffBaseStatus::NotEvaluated => "not_evaluated",
    }
}

fn effective_analysis_mode_str(mode: EffectiveAnalysisMode) -> &'static str {
    match mode {
        EffectiveAnalysisMode::Diff => "diff",
        EffectiveAnalysisMode::Full => "full",
    }
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

fn collect_score_notes(
    requested_level: RequestedLevel,
    function: Option<u8>,
    module: Option<u8>,
    project: Option<u8>,
) -> Vec<ScoreNote> {
    let mut notes = Vec::new();
    if requested_level.includes(AnalysisLevel::Function) && function.is_none() {
        notes.push(missing_score_note(AnalysisLevel::Function));
    }
    if requested_level.includes(AnalysisLevel::Module) && module.is_none() {
        notes.push(missing_score_note(AnalysisLevel::Module));
    }
    if requested_level.includes(AnalysisLevel::Project) && project.is_none() {
        notes.push(missing_score_note(AnalysisLevel::Project));
    }
    notes
}

fn missing_score_note(level: AnalysisLevel) -> ScoreNote {
    ScoreNote {
        level,
        reason: match level {
            AnalysisLevel::Function => "no function-level analysis scopes were detected",
            AnalysisLevel::Module => "no module-level analysis scopes were detected",
            AnalysisLevel::Project => "no project-level analysis scopes were detected",
        }
        .to_owned(),
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

    use serde_json::{Value, json};

    use super::{
        AnalysisReport, OutputFormat, ProjectedScores, ReportMetadata, ReportViewOptions,
        RequestedLevel, SARIF_SCHEMA_URL, materialize_summary, project_diagnostics, project_scores,
        render_sarif_error_document, summary_scope_for,
    };
    use crate::domains::diagnostics::{
        Diagnostic, DiagnosticKind, DiagnosticsScope, FileLocation, MetricObservation,
        PatternEdgeProvenance, PatternEvidence, PatternType, TemplateSuggestion,
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
                score_notes: vec![],
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
                score_notes: vec![],
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
                score_notes: vec![],
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
                score_notes: vec![],
            }
        );
    }

    #[test]
    fn all_level_caps_function_metric_diagnostics_per_rule() {
        let mut diagnostics = (0..30)
            .map(|index| fixture_function_metric_diagnostic("KAL-F001", "M-F001", index, 0.5))
            .collect::<Vec<_>>();
        diagnostics.extend((0..3).map(|index| {
            fixture_function_metric_diagnostic("KAL-F002", "M-F002", index + 100, 0.4)
        }));
        diagnostics.push(fixture_kal_m003_module_diagnostic());

        let report = project_report(
            RequestedLevel::All,
            None,
            &fixture_metrics(),
            diagnostics.clone(),
        );
        let projected = &report.diagnostics.diagnostics;

        assert_eq!(projected.len(), 29);
        assert_eq!(
            projected
                .iter()
                .filter(|diagnostic| diagnostic.rule_id.as_str() == "KAL-F001")
                .count(),
            super::MAX_ALL_LEVEL_FUNCTION_METRIC_DIAGNOSTICS_PER_RULE
        );
        assert_eq!(
            projected
                .iter()
                .filter(|diagnostic| diagnostic.rule_id.as_str() == "KAL-F002")
                .count(),
            3
        );
        assert!(
            projected
                .iter()
                .any(|diagnostic| diagnostic.rule_id.as_str() == "KAL-M003")
        );
        assert!(
            projected
                .iter()
                .filter(|diagnostic| diagnostic.rule_id.as_str() == "KAL-F001")
                .all(|diagnostic| diagnostic.primary_scope_id.qualified_name != "crate::f0"),
            "lowest-priority function metric diagnostic should be hidden"
        );
        assert_eq!(report.diagnostics.summary.error_count, 34);
        assert!(report.analysis_warnings.iter().any(|warning| {
            warning.contains("function metric flood control: hid 5")
                && warning.contains("Use --level function")
        }));
    }

    #[test]
    fn json_output_keeps_all_level_function_metric_flood_bounded() {
        let diagnostics = (0..30)
            .map(|index| fixture_function_metric_diagnostic("KAL-F001", "M-F001", index, 0.5))
            .collect::<Vec<_>>();
        let report = project_report(RequestedLevel::All, None, &fixture_metrics(), diagnostics);

        let rendered = report.render_json(None).expect("json should render");
        let parsed: Value = serde_json::from_str(&rendered).expect("json should parse");
        let diagnostics = parsed["diagnostics"].as_array().expect("diagnostics array");

        assert_eq!(
            diagnostics
                .iter()
                .filter(|diagnostic| diagnostic["rule_id"] == "KAL-F001")
                .count(),
            super::MAX_ALL_LEVEL_FUNCTION_METRIC_DIAGNOSTICS_PER_RULE
        );
        assert!(
            parsed["analysis_warnings"]
                .as_array()
                .expect("analysis warnings")
                .iter()
                .any(|warning| warning.as_str().is_some_and(|warning| {
                    warning.contains("function metric flood control")
                        && warning.contains("Use --level function")
                }))
        );
    }

    #[test]
    fn sarif_output_keeps_all_level_function_metric_flood_bounded() {
        let diagnostics = (0..30)
            .map(|index| fixture_function_metric_diagnostic("KAL-F001", "M-F001", index, 0.5))
            .collect::<Vec<_>>();
        let report = project_report(RequestedLevel::All, None, &fixture_metrics(), diagnostics);

        let rendered = report.render_sarif(None).expect("sarif should render");
        let parsed: Value = serde_json::from_str(&rendered).expect("sarif should parse");
        let run = &parsed["runs"][0];
        let results = run["results"].as_array().expect("results array");

        assert_eq!(
            results
                .iter()
                .filter(|result| result["ruleId"] == "KAL-F001")
                .count(),
            super::MAX_ALL_LEVEL_FUNCTION_METRIC_DIAGNOSTICS_PER_RULE
        );
        assert!(
            run["properties"]["analysis_warnings"]
                .as_array()
                .expect("analysis warnings")
                .iter()
                .any(|warning| warning.as_str().is_some_and(|warning| {
                    warning.contains("function metric flood control")
                        && warning.contains("Use --level function")
                }))
        );
    }

    #[test]
    fn function_level_keeps_full_function_metric_inventory() {
        let diagnostics = (0..30)
            .map(|index| fixture_function_metric_diagnostic("KAL-F001", "M-F001", index, 0.5))
            .collect::<Vec<_>>();

        let report = project_report(
            RequestedLevel::Function,
            None,
            &fixture_metrics(),
            diagnostics,
        );

        assert_eq!(report.diagnostics.diagnostics.len(), 30);
        assert!(
            report
                .analysis_warnings
                .iter()
                .all(|warning| !warning.contains("function metric flood control"))
        );
    }

    #[test]
    fn function_level_human_output_groups_function_metric_flood() {
        let mut diagnostics = (0..30)
            .map(|index| fixture_function_metric_diagnostic("KAL-F001", "M-F001", index, 0.5))
            .collect::<Vec<_>>();
        diagnostics.extend((0..30).map(|index| {
            fixture_function_metric_diagnostic("KAL-F003", "M-F003", index + 100, 0.7)
        }));

        let report = project_report(
            RequestedLevel::Function,
            None,
            &fixture_metrics(),
            diagnostics,
        );

        let rendered = report.render_human(None, false);

        assert_eq!(report.diagnostics.diagnostics.len(), 60);
        assert_eq!(rendered.matches("error[KAL-F001]").count(), 25);
        assert_eq!(rendered.matches("error[KAL-F003]").count(), 25);
        assert!(rendered.contains("function metric flood control: hid 10"));
        assert!(rendered.contains("KAL-F001: 5 hidden"));
        assert!(rendered.contains("KAL-F003: 5 hidden"));
        assert!(rendered.contains("JSON/SARIF keep the full diagnostic inventory"));
        assert!(rendered.contains("src/f29.rs:30:1"));
        assert!(rendered.contains("src/f129.rs:130:1"));
        assert!(!rendered.contains("src/f0.rs:1:1"));
        assert!(!rendered.contains("src/f100.rs:101:1"));
    }

    #[test]
    fn function_level_human_output_merges_non_metric_diagnostics_with_capped_metrics() {
        let mut diagnostics = (0..30)
            .map(|index| fixture_function_metric_diagnostic("KAL-F001", "M-F001", index, 0.5))
            .collect::<Vec<_>>();
        diagnostics.push(fixture_function_pattern_diagnostic());

        let report = project_report(
            RequestedLevel::Function,
            None,
            &fixture_metrics(),
            diagnostics,
        );

        let rendered = report.render_human(None, false);

        assert_eq!(rendered.matches("error[KAL-F001]").count(), 25);
        assert!(rendered.contains("src/a.rs:1  warning[KAL-PAT001]  [pattern] function pattern"));
        assert!(
            rendered.find("src/a.rs:1").unwrap() < rendered.find("src/f25.rs:26:1").unwrap(),
            "non-metric diagnostics should share the same output ordering as capped metrics"
        );
    }

    #[test]
    fn function_level_json_keeps_full_function_metric_inventory() {
        let diagnostics = (0..30)
            .map(|index| fixture_function_metric_diagnostic("KAL-F001", "M-F001", index, 0.5))
            .collect::<Vec<_>>();
        let report = project_report(
            RequestedLevel::Function,
            None,
            &fixture_metrics(),
            diagnostics,
        );

        let rendered = report.render_json(None).expect("json should render");
        let parsed: Value = serde_json::from_str(&rendered).expect("json should parse");
        let diagnostics = parsed["diagnostics"].as_array().expect("diagnostics array");

        assert_eq!(
            diagnostics
                .iter()
                .filter(|diagnostic| diagnostic["rule_id"] == "KAL-F001")
                .count(),
            30
        );
        assert!(
            parsed["analysis_warnings"]
                .as_array()
                .expect("analysis warnings")
                .iter()
                .all(|warning| warning
                    .as_str()
                    .is_none_or(|warning| !warning.contains("function metric flood control")))
        );
    }

    #[test]
    fn function_level_sarif_keeps_full_function_metric_inventory() {
        let diagnostics = (0..30)
            .map(|index| fixture_function_metric_diagnostic("KAL-F001", "M-F001", index, 0.5))
            .collect::<Vec<_>>();
        let report = project_report(
            RequestedLevel::Function,
            None,
            &fixture_metrics(),
            diagnostics,
        );

        let rendered = report.render_sarif(None).expect("sarif should render");
        let parsed: Value = serde_json::from_str(&rendered).expect("sarif should parse");
        let results = parsed["runs"][0]["results"]
            .as_array()
            .expect("sarif results");

        assert_eq!(
            results
                .iter()
                .filter(|result| result["ruleId"] == "KAL-F001")
                .count(),
            30
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
        assert_eq!(
            report.visible_summary(),
            crate::domains::diagnostics::DiagnosticSummary {
                error_count: 0,
                warning_count: 0,
                info_count: 0,
            }
        );
        assert!(
            report
                .render_human(None, false)
                .contains("0 errors, 0 warnings, 0 info")
        );
    }

    #[test]
    fn json_summary_reflects_severity_filter() {
        let mut diagnostics = fixture_diagnostics();
        diagnostics.push(Diagnostic {
            id: DiagnosticId::from("diag-function-error"),
            primary_scope_id: ScopeId::new(AnalysisLevel::Function, "crate::h", "src/lib.rs"),
            rule_id: RuleId::from("KAL-F099"),
            kind: DiagnosticKind::Metric,
            severity: Severity::Error,
            location: FileLocation {
                file_path: FilePath::from("src/lib.rs"),
                start_line: 30,
                end_line: 36,
                column: Some(7),
            },
            message: "function metric error".to_owned(),
            metric: Some(MetricObservation {
                metric_id: MetricId::from("M-F099"),
                raw_value: 5.0,
                normalized_risk: 0.95,
                threshold: 0.50,
                overflow_ratio: 0.90,
            }),
            pattern: None,
            edge_provenance: Vec::new(),
            template_suggestion: TemplateSuggestion {
                explanation: "split function".to_owned(),
                code_example: None,
            },
        });

        let report = project_report(
            RequestedLevel::All,
            Some(Severity::Error),
            &fixture_metrics(),
            diagnostics,
        );
        let rendered = report.render_json(None).expect("json should render");
        let parsed: Value = serde_json::from_str(&rendered).expect("json should parse");

        assert_eq!(parsed["summary"]["error_count"], 2);
        assert_eq!(parsed["summary"]["warning_count"], 0);
        assert_eq!(parsed["summary"]["info_count"], 0);
        assert_eq!(
            parsed["diagnostics"]
                .as_array()
                .expect("diagnostics array")
                .len(),
            2
        );
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
                min_risk: None,
                verbose: false,
            },
            Vec::new(),
        );

        assert!(report.visible_diagnostics().is_empty());
        assert_eq!(report.diagnostics.summary.warning_count, 1);
        assert_eq!(
            report.diagnostics.determine_exit_code(report.view.strict),
            crate::domains::diagnostics::ExitCode::DiagnosticFailure
        );
    }

    #[test]
    fn projected_scores_follow_requested_level() {
        let metrics = fixture_metrics();

        assert_eq!(
            project_scores(&metrics, RequestedLevel::Function, 10),
            ProjectedScores {
                overall: Some(45),
                function: Some(45),
                module: None,
                project: None,
                score_notes: vec![],
            }
        );
        assert_eq!(
            project_scores(&metrics, RequestedLevel::All, 10),
            ProjectedScores {
                overall: Some(44),
                function: Some(45),
                module: Some(20),
                project: Some(75),
                score_notes: vec![],
            }
        );
        assert_eq!(
            summary_scope_for(RequestedLevel::Project),
            crate::domains::diagnostics::SummaryScope::ListedDiagnostics
        );
    }

    #[test]
    fn score_notes_is_empty_when_all_levels_have_scores() {
        let scores = project_scores(&fixture_metrics(), RequestedLevel::All, 10);

        assert!(scores.score_notes.is_empty());
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
            "files_analyzed",
            "analysis_warnings",
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
        assert_eq!(parsed["files_analyzed"], 10);
        assert_eq!(parsed["diagnostics_scope"], "whole_project");
        assert_eq!(parsed["summary_scope"], "listed_diagnostics");
        assert_eq!(parsed["scores"]["overall"], 45);
        assert!(parsed["scores"]["module"].is_null());
        assert!(parsed["scores"]["project"].is_null());
        assert_eq!(
            parsed["scores"]["score_notes"]
                .as_array()
                .expect("score notes array")
                .len(),
            0
        );
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
    fn json_output_files_analyzed_matches_metadata() {
        let expected_file_count = 42;
        let report = AnalysisReport::project(
            ReportMetadata::new(
                vec![FilePath::from("src/")],
                expected_file_count,
                "0.1.0",
                "1.0.0",
            ),
            &fixture_metrics(),
            fixture_diagnostics(),
            DiagnosticsScope::WholeProject,
            ReportViewOptions {
                requested_level: RequestedLevel::All,
                output_format: OutputFormat::Json,
                strict: false,
                minimum_severity: None,
                min_risk: None,
                verbose: false,
            },
            Vec::new(),
        );

        let rendered = report.render_json(None).expect("json should render");
        let parsed: Value = serde_json::from_str(&rendered).expect("json should parse");

        assert_eq!(parsed["files_analyzed"], expected_file_count);
    }

    #[test]
    fn json_output_includes_pattern_edge_provenance() {
        let report = project_report(
            RequestedLevel::All,
            None,
            &fixture_metrics(),
            fixture_diagnostics(),
        );
        let rendered = report.render_json(None).expect("json should render");
        let parsed: Value = serde_json::from_str(&rendered).expect("json should parse");
        let cycle_diagnostic = parsed["diagnostics"]
            .as_array()
            .expect("diagnostics array")
            .iter()
            .find(|diagnostic| diagnostic["rule_id"] == "KAL-PAT003")
            .expect("cycle diagnostic");
        let edge_provenance = cycle_diagnostic["pattern"]["edge_provenance"]
            .as_array()
            .expect("edge provenance array");

        assert_eq!(edge_provenance.len(), 2);
        assert_eq!(edge_provenance[0]["source_file_path"], "src/parser.rs");
        assert_eq!(edge_provenance[0]["target_file_path"], "src/lexer.rs");
        assert_eq!(edge_provenance[0]["source_is_test"], false);
        assert_eq!(edge_provenance[0]["target_is_test"], false);
    }

    #[test]
    fn json_output_includes_module_metric_edge_provenance() {
        let report = project_report(
            RequestedLevel::All,
            None,
            &fixture_metrics(),
            fixture_diagnostics(),
        );
        let rendered = report.render_json(None).expect("json should render");
        let parsed: Value = serde_json::from_str(&rendered).expect("json should parse");
        let module_diagnostic = parsed["diagnostics"]
            .as_array()
            .expect("diagnostics array")
            .iter()
            .find(|diagnostic| diagnostic["rule_id"] == "KAL-M001")
            .expect("module diagnostic");
        let edge = &module_diagnostic["edge_provenance"][0];

        assert_eq!(edge["source_scope"]["qualified_name"], "crate");
        assert_eq!(edge["target_scope"]["qualified_name"], "crate::storage");
        assert_eq!(edge["source_file_path"], "src/lib.rs");
        assert_eq!(edge["target_file_path"], "src/storage.rs");
        assert_eq!(edge["source_start_line"], 14);
        assert_eq!(edge["target_start_line"], 6);
        assert!(module_diagnostic["template_suggestion"]["code_example"].is_null());
    }

    #[test]
    fn human_output_includes_module_metric_edge_provenance() {
        let report = project_report(
            RequestedLevel::All,
            None,
            &fixture_metrics(),
            fixture_diagnostics(),
        );
        let rendered = report.render_human(None, false);

        assert!(
            rendered
                .contains("edge crate -> crate::storage (src/lib.rs:14-14 -> src/storage.rs:6-6)")
        );
    }

    #[test]
    fn json_output_classifies_domain_architecture_diagnostics_as_untriaged_debt() {
        let mut diagnostics = fixture_diagnostics();
        diagnostics.push(fixture_kal_m003_module_diagnostic());
        let report = project_report(RequestedLevel::All, None, &fixture_metrics(), diagnostics);

        let rendered = report.render_json(None).expect("json should render");
        let parsed: Value = serde_json::from_str(&rendered).expect("json should parse");
        let diagnostics = parsed["diagnostics"].as_array().expect("diagnostics array");

        for rule_id in ["KAL-M001", "KAL-M003", "KAL-PAT003"] {
            let diagnostic = diagnostics
                .iter()
                .find(|diagnostic| diagnostic["rule_id"] == rule_id)
                .unwrap_or_else(|| panic!("expected {rule_id} diagnostic"));
            assert_eq!(
                diagnostic["evaluation_artifact"]["classification"],
                "target_repo_quality_finding"
            );
            assert_eq!(
                diagnostic["evaluation_artifact"]["triage_status"],
                "untriaged_domain_debt"
            );
            assert_eq!(diagnostic["evaluation_artifact"]["confidence"], "low");
        }

        let function_diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic["rule_id"] == "KAL-F001")
            .expect("function diagnostic");
        assert!(
            function_diagnostic["evaluation_artifact"].is_null(),
            "function metric diagnostics should not be marked as untriaged domain debt"
        );
    }

    #[test]
    fn json_output_does_not_classify_test_scope_module_diagnostics_as_untriaged_debt() {
        let diagnostics = vec![
            fixture_test_module_structural_diagnostic(
                "diag-test-module-fan-out",
                "KAL-M001",
                "tests/foo.rs",
            ),
            fixture_test_module_structural_diagnostic(
                "diag-test-module-instability",
                "KAL-M003",
                "tests/bar.rs",
            ),
        ];
        let report = project_report(RequestedLevel::All, None, &fixture_metrics(), diagnostics);

        let rendered = report.render_json(None).expect("json should render");
        let parsed: Value = serde_json::from_str(&rendered).expect("json should parse");
        let diagnostics = parsed["diagnostics"].as_array().expect("diagnostics array");

        for rule_id in ["KAL-M001", "KAL-M003"] {
            let diagnostic = diagnostics
                .iter()
                .find(|diagnostic| diagnostic["rule_id"] == rule_id)
                .unwrap_or_else(|| panic!("expected {rule_id} diagnostic"));
            assert!(
                diagnostic["evaluation_artifact"].is_null(),
                "test-scope {rule_id} diagnostics should not be marked as untriaged domain debt"
            );
        }
    }

    #[test]
    fn sarif_error_document_reports_tool_failure_with_invocation() {
        let rendered = render_sarif_error_document(
            "failed to load config file",
            Some("No such file or directory (os error 2)"),
            "9.9.9",
            2,
            "tool_error",
        );
        let parsed: Value = serde_json::from_str(&rendered).expect("sarif error should parse");

        assert_eq!(parsed["version"], "2.1.0");
        assert_eq!(parsed["$schema"], SARIF_SCHEMA_URL);

        let run = &parsed["runs"][0];
        assert_eq!(run["tool"]["driver"]["name"], "kalos");
        assert_eq!(run["tool"]["driver"]["version"], "9.9.9");
        assert_eq!(
            run["results"].as_array().expect("results array").len(),
            0,
            "error document must not fabricate diagnostics"
        );

        let invocation = &run["invocations"][0];
        assert_eq!(invocation["executionSuccessful"], Value::Bool(false));
        assert_eq!(invocation["exitCode"], Value::Number(2.into()));
        assert_eq!(invocation["exitCodeDescription"], "tool error");

        let notifications = invocation["toolExecutionNotifications"]
            .as_array()
            .expect("notifications array");
        assert_eq!(notifications.len(), 1);
        let notification = &notifications[0];
        assert_eq!(notification["level"], "error");
        assert_eq!(
            notification["message"]["text"],
            "failed to load config file"
        );
        assert_eq!(
            notification["properties"]["cause"],
            "No such file or directory (os error 2)"
        );
        assert_eq!(notification["properties"]["error_class"], "tool_error");
        assert_eq!(notification["properties"]["outcome"], "tool_error");

        let kalos_props = &run["properties"]["kalos"];
        assert_eq!(kalos_props["error"], Value::Bool(true));
        assert_eq!(kalos_props["error_class"], "tool_error");
        assert_eq!(kalos_props["outcome"], "tool_error");
        assert_eq!(kalos_props["message"], "failed to load config file");
        assert!(
            kalos_props["evaluation_artifact"].is_null(),
            "tool error documents must not be classified as target repo findings"
        );
        assert_eq!(
            kalos_props["cause"],
            "No such file or directory (os error 2)"
        );
    }

    #[test]
    fn sarif_error_document_omits_cause_when_source_missing() {
        let rendered = render_sarif_error_document("boom", None, "9.9.9", 2, "tool_error");
        let parsed: Value = serde_json::from_str(&rendered).expect("sarif error should parse");

        let notification = &parsed["runs"][0]["invocations"][0]["toolExecutionNotifications"][0];
        assert!(notification["properties"]["cause"].is_null());
        assert_eq!(notification["properties"]["error_class"], "tool_error");
        assert_eq!(notification["properties"]["outcome"], "tool_error");
        let kalos_props = &parsed["runs"][0]["properties"]["kalos"];
        assert!(kalos_props.get("cause").is_none() || kalos_props["cause"].is_null());
        assert_eq!(kalos_props["error_class"], "tool_error");
        assert_eq!(kalos_props["outcome"], "tool_error");
    }

    #[test]
    fn sarif_error_document_describes_codeql_infrastructure_error_class() {
        let rendered = render_sarif_error_document(
            "failed to resolve CodeQL bundle",
            Some("CodeQL bundle bootstrap lock showed no progress"),
            "9.9.9",
            2,
            "codeql_infrastructure",
        );
        let parsed: Value = serde_json::from_str(&rendered).expect("sarif error should parse");

        let invocation = &parsed["runs"][0]["invocations"][0];
        assert_eq!(
            invocation["exitCodeDescription"],
            "CodeQL infrastructure error"
        );
        assert_eq!(
            invocation["toolExecutionNotifications"][0]["properties"]["error_class"],
            "codeql_infrastructure"
        );
        assert_eq!(
            invocation["toolExecutionNotifications"][0]["properties"]["outcome"],
            "infrastructure_error"
        );
        assert_eq!(
            parsed["runs"][0]["properties"]["kalos"]["error_class"],
            "codeql_infrastructure"
        );
        assert_eq!(
            parsed["runs"][0]["properties"]["kalos"]["outcome"],
            "infrastructure_error"
        );
    }

    #[test]
    fn sarif_error_document_describes_expected_skip_error_class() {
        let rendered = render_sarif_error_document(
            "`--llm` requires KALOS_LLM_API_KEY to be set",
            None,
            "9.9.9",
            2,
            "expected_skip",
        );
        let parsed: Value = serde_json::from_str(&rendered).expect("sarif error should parse");

        let invocation = &parsed["runs"][0]["invocations"][0];
        assert_eq!(invocation["exitCodeDescription"], "expected skip");
        assert_eq!(
            invocation["toolExecutionNotifications"][0]["properties"]["error_class"],
            "expected_skip"
        );
        assert_eq!(
            invocation["toolExecutionNotifications"][0]["properties"]["outcome"],
            "expected_skip"
        );
        assert_eq!(
            parsed["runs"][0]["properties"]["kalos"]["error_class"],
            "expected_skip"
        );
        assert_eq!(
            parsed["runs"][0]["properties"]["kalos"]["outcome"],
            "expected_skip"
        );
    }

    #[test]
    fn sarif_error_document_describes_input_error_outcome() {
        let rendered = render_sarif_error_document(
            "analysis target path `missing` could not be read",
            Some("No such file or directory (os error 2)"),
            "9.9.9",
            2,
            "input_error",
        );
        let parsed: Value = serde_json::from_str(&rendered).expect("sarif error should parse");

        let invocation = &parsed["runs"][0]["invocations"][0];
        assert_eq!(invocation["exitCodeDescription"], "input error");
        assert_eq!(
            invocation["toolExecutionNotifications"][0]["properties"]["outcome"],
            "input_error"
        );
        assert_eq!(
            parsed["runs"][0]["properties"]["kalos"]["outcome"],
            "input_error"
        );
    }

    #[test]
    fn sarif_output_contains_rules_results_and_locations() {
        let mut diagnostics = fixture_diagnostics();
        diagnostics.push(fixture_kal_m003_module_diagnostic());
        let report = project_report(RequestedLevel::All, None, &fixture_metrics(), diagnostics);
        let rendered = report.render_sarif(None).expect("sarif should render");
        let parsed: Value = serde_json::from_str(&rendered).expect("sarif should parse");
        let run = &parsed["runs"][0];
        let rules = run["tool"]["driver"]["rules"]
            .as_array()
            .expect("rules array");
        let results = run["results"].as_array().expect("results array");

        assert_eq!(parsed["version"], "2.1.0");
        assert!(!rules.is_empty());
        assert_eq!(results.len(), 5);
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

        for rule_id in ["KAL-M001", "KAL-M003", "KAL-PAT003"] {
            let result = results
                .iter()
                .find(|result| result["ruleId"] == rule_id)
                .unwrap_or_else(|| panic!("expected {rule_id} result"));
            assert_eq!(
                result["properties"]["kalos"]["evaluation_artifact"]["classification"],
                "target_repo_quality_finding"
            );
            assert_eq!(
                result["properties"]["kalos"]["evaluation_artifact"]["triage_status"],
                "untriaged_domain_debt"
            );
            assert_eq!(
                result["properties"]["kalos"]["evaluation_artifact"]["confidence"],
                "low"
            );
        }

        let cross_scope_result = results
            .iter()
            .find(|result| result["ruleId"] == "KAL-PAT003")
            .expect("cross-scope result");
        assert!(
            cross_scope_result["locations"][0]["physicalLocation"]["region"]["startColumn"]
                .is_null()
        );
        assert!(cross_scope_result["properties"]["kalos"]["template_suggestion"].is_object());

        let kalos_props = &run["properties"]["kalos"];
        assert!(
            kalos_props["scores"].is_object(),
            "scores should be present in SARIF properties"
        );
        assert_eq!(kalos_props["scores"]["overall"], 44);
        assert!(kalos_props["scores"]["function"].is_number());
        assert!(kalos_props["scores"]["score_notes"].is_array());

        assert!(
            kalos_props["summary"].is_object(),
            "summary should be present in SARIF properties"
        );
        assert!(kalos_props["summary"]["error_count"].is_number());
        assert!(kalos_props["summary"]["warning_count"].is_number());
        assert!(kalos_props["summary"]["info_count"].is_number());
        assert!(run["properties"]["analysis_warnings"].is_array());
    }

    #[test]
    fn sarif_output_does_not_classify_test_scope_module_diagnostics_as_untriaged_debt() {
        let diagnostics = vec![
            fixture_test_module_structural_diagnostic(
                "diag-test-module-fan-out",
                "KAL-M001",
                "tests/foo.rs",
            ),
            fixture_test_module_structural_diagnostic(
                "diag-test-module-instability",
                "KAL-M003",
                "tests/bar.rs",
            ),
        ];
        let report = project_report(RequestedLevel::All, None, &fixture_metrics(), diagnostics);

        let rendered = report.render_sarif(None).expect("sarif should render");
        let parsed: Value = serde_json::from_str(&rendered).expect("sarif should parse");
        let results = parsed["runs"][0]["results"]
            .as_array()
            .expect("results array");

        for rule_id in ["KAL-M001", "KAL-M003"] {
            let result = results
                .iter()
                .find(|result| result["ruleId"] == rule_id)
                .unwrap_or_else(|| panic!("expected {rule_id} result"));
            assert!(
                result["properties"]["kalos"]["evaluation_artifact"].is_null(),
                "test-scope {rule_id} results should not be marked as untriaged domain debt"
            );
        }
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
                min_risk: None,
                verbose: true,
            },
            Vec::new(),
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
                min_risk: None,
                verbose: false,
            },
            Vec::new(),
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
                min_risk: None,
                verbose: false,
            },
            Vec::new(),
        );

        let rendered = report.render_human(None, false);

        assert!(rendered.starts_with("Analyzed 42 files in src/, tests/\n\n"));
    }

    #[test]
    fn human_output_marks_test_module_structural_diagnostics() {
        let mut diagnostics = fixture_diagnostics();
        diagnostics.push(fixture_test_module_structural_diagnostic(
            "diag-test-module-fan-out",
            "KAL-M001",
            "tests/foo.rs",
        ));
        let report = AnalysisReport::project(
            ReportMetadata::new(
                vec![FilePath::from("src/"), FilePath::from("tests/")],
                42,
                "0.1.0",
                "1.0.0",
            ),
            &fixture_metrics(),
            diagnostics,
            DiagnosticsScope::WholeProject,
            ReportViewOptions {
                requested_level: RequestedLevel::All,
                output_format: OutputFormat::Human,
                strict: false,
                minimum_severity: None,
                min_risk: None,
                verbose: false,
            },
            Vec::new(),
        );

        let rendered = report.render_human(None, false);

        assert!(rendered.contains(
            "tests/foo.rs:1:1  warning[KAL-M001]  [metric] test module metric warning [test module structural risk]"
        ));
        assert!(rendered.contains(
            "note: 1 test-module structural diagnostic shown separately; these KAL-M001/KAL-M003 findings are test risk from --include-tests, not production module risk"
        ));
    }

    #[test]
    fn human_output_marks_kal_m003_test_module_structural_diagnostic() {
        let mut diagnostics = fixture_diagnostics();
        diagnostics.push(fixture_test_module_structural_diagnostic(
            "diag-test-module-instability",
            "KAL-M003",
            "tests/foo.rs",
        ));
        let report = AnalysisReport::project(
            ReportMetadata::new(
                vec![FilePath::from("src/"), FilePath::from("tests/")],
                42,
                "0.1.0",
                "1.0.0",
            ),
            &fixture_metrics(),
            diagnostics,
            DiagnosticsScope::WholeProject,
            ReportViewOptions {
                requested_level: RequestedLevel::All,
                output_format: OutputFormat::Human,
                strict: false,
                minimum_severity: None,
                min_risk: None,
                verbose: false,
            },
            Vec::new(),
        );

        let rendered = report.render_human(None, false);

        assert!(rendered.contains(
            "tests/foo.rs:1:1  warning[KAL-M003]  [metric] test module metric warning [test module structural risk]"
        ));
        assert!(rendered.contains(
            "note: 1 test-module structural diagnostic shown separately; these KAL-M001/KAL-M003 findings are test risk from --include-tests, not production module risk"
        ));
    }

    #[test]
    fn human_output_counts_multiple_test_module_structural_diagnostics() {
        let mut diagnostics = fixture_diagnostics();
        diagnostics.push(fixture_test_module_structural_diagnostic(
            "diag-test-module-fan-out",
            "KAL-M001",
            "tests/foo.rs",
        ));
        diagnostics.push(fixture_test_module_structural_diagnostic(
            "diag-test-module-instability",
            "KAL-M003",
            "tests/bar.rs",
        ));
        let report = AnalysisReport::project(
            ReportMetadata::new(
                vec![FilePath::from("src/"), FilePath::from("tests/")],
                42,
                "0.1.0",
                "1.0.0",
            ),
            &fixture_metrics(),
            diagnostics,
            DiagnosticsScope::WholeProject,
            ReportViewOptions {
                requested_level: RequestedLevel::All,
                output_format: OutputFormat::Human,
                strict: false,
                minimum_severity: None,
                min_risk: None,
                verbose: false,
            },
            Vec::new(),
        );

        let rendered = report.render_human(None, false);

        assert!(rendered.contains(
            "tests/foo.rs:1:1  warning[KAL-M001]  [metric] test module metric warning [test module structural risk]"
        ));
        assert!(rendered.contains(
            "tests/bar.rs:1:1  warning[KAL-M003]  [metric] test module metric warning [test module structural risk]"
        ));
        assert!(rendered.contains(
            "note: 2 test-module structural diagnostics shown separately; these KAL-M001/KAL-M003 findings are test risk from --include-tests, not production module risk"
        ));
    }

    #[test]
    fn human_output_keeps_production_module_diagnostics_unmarked() {
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
                min_risk: None,
                verbose: false,
            },
            Vec::new(),
        );

        let rendered = report.render_human(None, false);

        assert!(rendered.contains("src/lib.rs:1:1  error[KAL-M001]  [metric] module metric error"));
        assert!(!rendered.contains("[test module structural risk]"));
        assert!(!rendered.contains("test-module structural diagnostic"));
    }

    #[test]
    fn human_output_shows_note_when_module_score_is_na() {
        let report = AnalysisReport::project(
            ReportMetadata::new(vec![FilePath::from("src/")], 42, "0.1.0", "1.0.0"),
            &fixture_metrics_without_module_score(),
            fixture_diagnostics(),
            DiagnosticsScope::WholeProject,
            ReportViewOptions {
                requested_level: RequestedLevel::All,
                output_format: OutputFormat::Human,
                strict: false,
                minimum_severity: None,
                min_risk: None,
                verbose: false,
            },
            Vec::new(),
        );

        let rendered = report.render_human(None, false);

        assert!(rendered.contains("Score: 100/100  (function: 100, module: n/a, project: 100)"));
        assert!(rendered.contains(
            "note: module score is not available — no module-level analysis scopes were detected"
        ));
    }

    #[test]
    fn human_output_shows_project_deduction_factors_when_score_below_100() {
        let report = human_report(&fixture_metrics(), false, None);

        let rendered = report.render_human(None, false);

        assert!(rendered.contains("Score: 44/100  (function: 45, module: 20, project: 75)"));
        assert!(rendered.contains("  project: M-P001 (Cyclic Coupling Risk) = 1.000"));
        assert!(rendered.contains(
            "  project: M-P001 (Cyclic Coupling Risk) = 1.000\n1 errors, 2 warnings, 1 info"
        ));
    }

    #[test]
    fn human_output_omits_project_deduction_when_score_is_100() {
        let report = human_report(&fixture_metrics_without_module_score(), false, None);

        let rendered = report.render_human(None, false);

        assert!(!rendered.contains("  project: "));
    }

    #[test]
    fn human_output_omits_project_deduction_when_no_project_score() {
        let mut metrics = fixture_metrics();
        metrics.overall_score.project_risk = None;
        metrics.overall_score.project_score = None;

        let report = human_report(&metrics, false, None);
        let rendered = report.render_human(None, false);

        assert!(!rendered.contains("  project: "));
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
                min_risk: None,
                verbose: false,
            },
            Vec::new(),
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
                min_risk: None,
                verbose: true,
            },
            Vec::new(),
        );

        let rendered = report.render_human(None, false);

        assert!(rendered.contains("── Metrics ──"));
    }

    #[test]
    fn verbose_default_filters_zero_risk_scopes() {
        let report = human_report(&fixture_metrics_with_mixed_risk(), true, None);

        let rendered = report.render_human(None, false);

        assert!(rendered.contains("crate::half [function] risk=0.500"));
        assert!(rendered.contains("crate [module] risk=0.800"));
        assert!(!rendered.contains("crate::zero [function] risk=0.000"));
        assert!(!rendered.contains("<project> [project] risk=0.000"));
    }

    #[test]
    fn verbose_min_risk_zero_shows_all_scopes() {
        let report = human_report(&fixture_metrics_with_mixed_risk(), true, Some(0.0));

        let rendered = report.render_human(None, false);

        assert!(rendered.contains("crate::zero [function] risk=0.000"));
        assert!(rendered.contains("crate::half [function] risk=0.500"));
        assert!(rendered.contains("crate [module] risk=0.800"));
        assert!(rendered.contains("<project> [project] risk=0.000"));
    }

    #[test]
    fn verbose_min_risk_threshold_filters_scopes_below_threshold() {
        let report = human_report(&fixture_metrics_with_mixed_risk(), true, Some(0.5));

        let rendered = report.render_human(None, false);

        assert!(!rendered.contains("crate::zero [function] risk=0.000"));
        assert!(rendered.contains("crate::half [function] risk=0.500"));
        assert!(rendered.contains("crate [module] risk=0.800"));
        assert!(!rendered.contains("<project> [project] risk=0.000"));
    }

    #[test]
    fn verbose_filtered_scope_note_shows_hidden_count() {
        let report = human_report(&fixture_metrics_with_mixed_risk(), true, None);

        let rendered = report.render_human(None, false);

        assert!(
            rendered.contains(
                "(2 scopes with risk below threshold hidden; use --min-risk 0 to show all)"
            )
        );
    }

    #[test]
    fn non_verbose_output_is_unaffected_by_min_risk() {
        let baseline = human_report(&fixture_metrics_with_mixed_risk(), false, None);
        let with_threshold = human_report(&fixture_metrics_with_mixed_risk(), false, Some(0.5));

        assert_eq!(
            baseline.render_human(None, false),
            with_threshold.render_human(None, false)
        );
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
                min_risk: None,
                verbose: false,
            },
            Vec::new(),
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

    #[test]
    fn json_output_includes_score_notes_for_na_levels() {
        let report = AnalysisReport::project(
            ReportMetadata::new(vec![FilePath::from("src/")], 42, "0.1.0", "1.0.0"),
            &fixture_metrics_without_module_score(),
            fixture_diagnostics(),
            DiagnosticsScope::WholeProject,
            ReportViewOptions {
                requested_level: RequestedLevel::All,
                output_format: OutputFormat::Json,
                strict: false,
                minimum_severity: None,
                min_risk: None,
                verbose: false,
            },
            Vec::new(),
        );

        let rendered = report.render_json(None).expect("json should render");
        let parsed: Value = serde_json::from_str(&rendered).expect("json should parse");
        let score_notes = parsed["scores"]["score_notes"]
            .as_array()
            .expect("score notes array");

        assert_eq!(score_notes.len(), 1);
        assert_eq!(score_notes[0]["level"], "module");
        assert_eq!(
            score_notes[0]["reason"],
            "no module-level analysis scopes were detected"
        );
    }

    #[test]
    fn json_output_adds_kal_m003_module_gate_guidance_for_all_level() {
        let mut diagnostics = fixture_diagnostics();
        diagnostics.push(fixture_kal_m003_module_diagnostic());
        let report = project_report(RequestedLevel::All, None, &fixture_metrics(), diagnostics);

        let rendered = report.render_json(None).expect("json should render");
        let parsed: Value = serde_json::from_str(&rendered).expect("json should parse");
        let warnings = parsed["analysis_warnings"]
            .as_array()
            .expect("analysis warnings array");

        assert!(warnings.iter().any(|warning| {
            warning
                .as_str()
                .is_some_and(|text| text.contains("module/all gate guidance: KAL-M003"))
        }));
        assert!(warnings.iter().any(|warning| {
            warning
                .as_str()
                .is_some_and(|text| text.contains("configured threshold(s) 0.750000"))
        }));
        assert!(warnings.iter().any(|warning| {
            warning
                .as_str()
                .is_some_and(|text| text.contains("domain-owner architecture review gate"))
        }));
    }

    #[test]
    fn sarif_output_adds_kal_m003_module_gate_guidance_for_module_level() {
        let report = project_report(
            RequestedLevel::Module,
            None,
            &fixture_metrics(),
            vec![fixture_kal_m003_module_diagnostic()],
        );

        let rendered = report.render_sarif(None).expect("sarif should render");
        let parsed: Value = serde_json::from_str(&rendered).expect("sarif should parse");
        let warnings = parsed["runs"][0]["properties"]["analysis_warnings"]
            .as_array()
            .expect("analysis warnings array");

        assert!(warnings.iter().any(|warning| {
            warning
                .as_str()
                .is_some_and(|text| text.contains("module/all gate guidance: KAL-M003"))
        }));
        assert!(warnings.iter().any(|warning| {
            warning
                .as_str()
                .is_some_and(|text| text.contains("use --level project as the baseline CI gate"))
        }));
    }

    #[test]
    fn function_output_does_not_add_kal_m003_module_gate_guidance() {
        let report = project_report(
            RequestedLevel::Function,
            None,
            &fixture_metrics(),
            vec![fixture_kal_m003_module_diagnostic()],
        );

        assert!(
            report
                .analysis_warnings
                .iter()
                .all(|warning| !warning.contains("module/all gate guidance: KAL-M003"))
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
                min_risk: None,
                verbose: false,
            },
            Vec::new(),
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

    fn fixture_metrics_without_module_score() -> AnalysisMetrics {
        AnalysisMetrics {
            function_metrics: vec![ScopeMetrics {
                scope_id: ScopeId::new(AnalysisLevel::Function, "crate::f", "src/lib.rs"),
                scope_risk: 0.0,
                values: vec![MetricValue {
                    metric_id: MetricId::from("M-F001"),
                    raw_value: 1.0,
                    normalized_risk: 0.0,
                }],
            }],
            module_metrics: vec![],
            project_metrics: Some(ScopeMetrics {
                scope_id: ScopeId::new(AnalysisLevel::Project, "<project>", "."),
                scope_risk: 0.0,
                values: vec![MetricValue {
                    metric_id: MetricId::from("M-P001"),
                    raw_value: 1.0,
                    normalized_risk: 0.0,
                }],
            }),
            overall_score: OverallScore {
                function_risk: Some(0.0),
                module_risk: None,
                project_risk: Some(0.0),
                overall_risk: 0.0,
                overall_score: 100,
                function_score: Some(100),
                module_score: None,
                project_score: Some(100),
            },
        }
    }

    fn fixture_metrics_with_mixed_risk() -> AnalysisMetrics {
        AnalysisMetrics {
            function_metrics: vec![
                ScopeMetrics {
                    scope_id: ScopeId::new(AnalysisLevel::Function, "crate::zero", "src/lib.rs"),
                    scope_risk: 0.0,
                    values: vec![MetricValue {
                        metric_id: MetricId::from("M-F001"),
                        raw_value: 0.0,
                        normalized_risk: 0.0,
                    }],
                },
                ScopeMetrics {
                    scope_id: ScopeId::new(AnalysisLevel::Function, "crate::half", "src/lib.rs"),
                    scope_risk: 0.5,
                    values: vec![MetricValue {
                        metric_id: MetricId::from("M-F002"),
                        raw_value: 2.0,
                        normalized_risk: 0.5,
                    }],
                },
            ],
            module_metrics: vec![ScopeMetrics {
                scope_id: ScopeId::new(AnalysisLevel::Module, "crate", "src/lib.rs"),
                scope_risk: 0.8,
                values: vec![MetricValue {
                    metric_id: MetricId::from("M-M001"),
                    raw_value: 4.0,
                    normalized_risk: 0.8,
                }],
            }],
            project_metrics: Some(ScopeMetrics {
                scope_id: ScopeId::new(AnalysisLevel::Project, "<project>", "."),
                scope_risk: 0.0,
                values: vec![MetricValue {
                    metric_id: MetricId::from("M-P001"),
                    raw_value: 1.0,
                    normalized_risk: 0.0,
                }],
            }),
            overall_score: OverallScore {
                function_risk: Some(0.5),
                module_risk: Some(0.8),
                project_risk: Some(0.0),
                overall_risk: 0.43,
                overall_score: 57,
                function_score: Some(50),
                module_score: Some(20),
                project_score: Some(100),
            },
        }
    }

    fn human_report(
        metrics: &AnalysisMetrics,
        verbose: bool,
        min_risk: Option<f64>,
    ) -> AnalysisReport {
        AnalysisReport::project(
            ReportMetadata::new(vec![FilePath::from("src/")], 42, "0.1.0", "1.0.0"),
            metrics,
            fixture_diagnostics(),
            DiagnosticsScope::WholeProject,
            ReportViewOptions {
                requested_level: RequestedLevel::All,
                output_format: OutputFormat::Human,
                strict: false,
                minimum_severity: None,
                min_risk,
                verbose,
            },
            Vec::new(),
        )
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
                name: None,
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
                edge_provenance: Vec::new(),
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
                edge_provenance: vec![PatternEdgeProvenance {
                    source_scope: ScopeId::new(AnalysisLevel::Module, "crate", "src/lib.rs"),
                    target_scope: ScopeId::new(
                        AnalysisLevel::Module,
                        "crate::storage",
                        "src/storage.rs",
                    ),
                    source_file_path: FilePath::from("src/lib.rs"),
                    target_file_path: FilePath::from("src/storage.rs"),
                    source_start_line: 14,
                    source_end_line: 14,
                    target_start_line: 6,
                    target_end_line: 6,
                    source_is_test: false,
                    target_is_test: false,
                }],
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
                    edge_provenance: vec![
                        PatternEdgeProvenance {
                            source_scope: ScopeId::new(
                                AnalysisLevel::Module,
                                "parser",
                                "src/parser.rs",
                            ),
                            target_scope: ScopeId::new(
                                AnalysisLevel::Module,
                                "lexer",
                                "src/lexer.rs",
                            ),
                            source_file_path: FilePath::from("src/parser.rs"),
                            target_file_path: FilePath::from("src/lexer.rs"),
                            source_start_line: 12,
                            source_end_line: 12,
                            target_start_line: 8,
                            target_end_line: 8,
                            source_is_test: false,
                            target_is_test: false,
                        },
                        PatternEdgeProvenance {
                            source_scope: ScopeId::new(
                                AnalysisLevel::Module,
                                "lexer",
                                "src/lexer.rs",
                            ),
                            target_scope: ScopeId::new(
                                AnalysisLevel::Module,
                                "parser",
                                "src/parser.rs",
                            ),
                            source_file_path: FilePath::from("src/lexer.rs"),
                            target_file_path: FilePath::from("src/parser.rs"),
                            source_start_line: 8,
                            source_end_line: 8,
                            target_start_line: 12,
                            target_end_line: 12,
                            source_is_test: false,
                            target_is_test: false,
                        },
                    ],
                }),
                edge_provenance: Vec::new(),
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
            edge_provenance: Vec::new(),
            template_suggestion: TemplateSuggestion {
                explanation: "extract helper".to_owned(),
                code_example: None,
            },
        }
    }

    fn fixture_function_metric_diagnostic(
        rule_id: &str,
        metric_id: &str,
        index: u32,
        base_risk: f64,
    ) -> Diagnostic {
        let normalized_risk = base_risk + f64::from(index) / 1000.0;
        Diagnostic {
            id: DiagnosticId::from(format!("diag-function-{rule_id}-{index}")),
            primary_scope_id: ScopeId::new(
                AnalysisLevel::Function,
                format!("crate::f{index}"),
                format!("src/f{index}.rs"),
            ),
            rule_id: RuleId::from(rule_id),
            kind: DiagnosticKind::Metric,
            severity: Severity::Error,
            location: FileLocation {
                file_path: FilePath::from(format!("src/f{index}.rs")),
                start_line: index + 1,
                end_line: index + 2,
                column: Some(1),
            },
            message: "function metric flood".to_owned(),
            metric: Some(MetricObservation {
                metric_id: MetricId::from(metric_id),
                raw_value: f64::from(index),
                normalized_risk,
                threshold: 0.50,
                overflow_ratio: normalized_risk - 0.50,
            }),
            pattern: None,
            edge_provenance: Vec::new(),
            template_suggestion: TemplateSuggestion {
                explanation: "extract helper".to_owned(),
                code_example: None,
            },
        }
    }

    fn fixture_function_pattern_diagnostic() -> Diagnostic {
        Diagnostic {
            id: DiagnosticId::from("diag-function-pattern"),
            primary_scope_id: ScopeId::new(AnalysisLevel::Function, "crate::a", "src/a.rs"),
            rule_id: RuleId::from("KAL-PAT001"),
            kind: DiagnosticKind::Pattern,
            severity: Severity::Warning,
            location: FileLocation {
                file_path: FilePath::from("src/a.rs"),
                start_line: 1,
                end_line: 3,
                column: None,
            },
            message: "function pattern".to_owned(),
            metric: None,
            pattern: Some(PatternEvidence {
                pattern_type: PatternType::FeatureEnvy,
                evidence_scopes: Vec::new(),
                evidence_message: "function reaches across module boundary".to_owned(),
                edge_provenance: Vec::new(),
            }),
            edge_provenance: Vec::new(),
            template_suggestion: TemplateSuggestion {
                explanation: "move behavior closer to data".to_owned(),
                code_example: None,
            },
        }
    }

    fn fixture_test_module_structural_diagnostic(
        id: &str,
        rule_id: &str,
        file_path: &str,
    ) -> Diagnostic {
        let metric_id = match rule_id {
            "KAL-M003" => "M-M003",
            _ => "M-M001",
        };

        Diagnostic {
            id: DiagnosticId::from(id),
            primary_scope_id: ScopeId::new(AnalysisLevel::Module, "crate::tests", file_path),
            rule_id: RuleId::from(rule_id),
            kind: DiagnosticKind::Metric,
            severity: Severity::Warning,
            location: FileLocation {
                file_path: FilePath::from(file_path),
                start_line: 1,
                end_line: 80,
                column: Some(1),
            },
            message: "test module metric warning".to_owned(),
            metric: Some(MetricObservation {
                metric_id: MetricId::from(metric_id),
                raw_value: 4.0,
                normalized_risk: 0.80,
                threshold: 0.50,
                overflow_ratio: 0.60,
            }),
            pattern: None,
            edge_provenance: Vec::new(),
            template_suggestion: TemplateSuggestion {
                explanation: "reduce test fan-out".to_owned(),
                code_example: None,
            },
        }
    }

    fn fixture_kal_m003_module_diagnostic() -> Diagnostic {
        Diagnostic {
            id: DiagnosticId::from("diag-module-instability"),
            primary_scope_id: ScopeId::new(
                AnalysisLevel::Module,
                "crate::service",
                "src/service.rs",
            ),
            rule_id: RuleId::from("KAL-M003"),
            kind: DiagnosticKind::Metric,
            severity: Severity::Error,
            location: FileLocation {
                file_path: FilePath::from("src/service.rs"),
                start_line: 1,
                end_line: 120,
                column: Some(1),
            },
            message: "M-M003 normalized risk 0.900000 exceeded threshold 0.750000".to_owned(),
            metric: Some(MetricObservation {
                metric_id: MetricId::from("M-M003"),
                raw_value: 0.9,
                normalized_risk: 0.9,
                threshold: 0.75,
                overflow_ratio: 0.6,
            }),
            pattern: None,
            edge_provenance: Vec::new(),
            template_suggestion: TemplateSuggestion {
                explanation: "review dependency direction".to_owned(),
                code_example: None,
            },
        }
    }

    fn fixture_analysis_warning() -> String {
        "no files with supported extensions (.py, .ts, .tsx, .rs, .go) were found in the analysis targets".to_owned()
    }

    #[test]
    fn project_scores_returns_none_when_zero_files() {
        let metrics = fixture_metrics();
        let scores = project_scores(&metrics, RequestedLevel::All, 0);
        assert_eq!(scores.overall, None);
        assert_eq!(scores.function, None);
        assert_eq!(scores.module, None);
        assert_eq!(scores.project, None);
        assert_eq!(scores.score_notes.len(), 1);
        assert!(scores.score_notes[0].reason.contains("no source files"));
    }

    #[test]
    fn human_output_shows_na_score_when_zero_files() {
        let report = AnalysisReport::project(
            ReportMetadata::new(vec![FilePath::from(".")], 0, "0.1.0", "1.0.0"),
            &fixture_metrics(),
            vec![],
            DiagnosticsScope::WholeProject,
            ReportViewOptions {
                requested_level: RequestedLevel::All,
                output_format: OutputFormat::Human,
                strict: false,
                minimum_severity: None,
                min_risk: None,
                verbose: false,
            },
            vec![fixture_analysis_warning()],
        );
        let rendered = report.render_human(None, false);
        assert!(
            rendered.contains("Score: n/a"),
            "expected 'Score: n/a' but got:\n{rendered}"
        );
        assert!(rendered.contains("no source files were analyzed"));
        assert!(rendered.contains(
            "note: no files with supported extensions (.py, .ts, .tsx, .rs, .go) were found in the analysis targets"
        ));
    }

    #[test]
    fn json_output_has_null_overall_when_zero_files() {
        let report = AnalysisReport::project(
            ReportMetadata::new(vec![FilePath::from(".")], 0, "0.1.0", "1.0.0"),
            &fixture_metrics(),
            vec![],
            DiagnosticsScope::WholeProject,
            ReportViewOptions {
                requested_level: RequestedLevel::All,
                output_format: OutputFormat::Json,
                strict: false,
                minimum_severity: None,
                min_risk: None,
                verbose: false,
            },
            vec![fixture_analysis_warning()],
        );
        let rendered = report.render_json(None).unwrap();
        let json: Value = serde_json::from_str(&rendered).unwrap();
        assert!(json["scores"]["overall"].is_null());
        assert!(json["scores"]["function"].is_null());
        assert!(json["scores"]["module"].is_null());
        assert!(json["scores"]["project"].is_null());
        assert_eq!(
            json["analysis_warnings"],
            json!([fixture_analysis_warning()])
        );
    }

    #[test]
    fn sarif_scores_and_summary_match_json_output() {
        let report = project_report(
            RequestedLevel::All,
            None,
            &fixture_metrics(),
            fixture_diagnostics(),
        );
        let json_rendered = report.render_json(None).expect("json should render");
        let json: Value = serde_json::from_str(&json_rendered).expect("json should parse");

        let sarif_rendered = report.render_sarif(None).expect("sarif should render");
        let sarif: Value = serde_json::from_str(&sarif_rendered).expect("sarif should parse");
        let kalos = &sarif["runs"][0]["properties"]["kalos"];

        assert_eq!(kalos["outcome"], json["outcome"]);
        assert_eq!(kalos["scores"]["overall"], json["scores"]["overall"]);
        assert_eq!(kalos["scores"]["function"], json["scores"]["function"]);
        assert_eq!(kalos["scores"]["module"], json["scores"]["module"]);
        assert_eq!(kalos["scores"]["project"], json["scores"]["project"]);
        assert_eq!(
            kalos["scores"]["score_notes"],
            json["scores"]["score_notes"]
        );

        assert_eq!(
            kalos["summary"]["error_count"],
            json["summary"]["error_count"]
        );
        assert_eq!(
            kalos["summary"]["warning_count"],
            json["summary"]["warning_count"]
        );
        assert_eq!(
            kalos["summary"]["info_count"],
            json["summary"]["info_count"]
        );
        assert_eq!(
            sarif["runs"][0]["properties"]["analysis_warnings"],
            json["analysis_warnings"]
        );

        for field in [
            "files_analyzed",
            "diagnostics_scope",
            "summary_scope",
            "schema_version",
            "tool_version",
        ] {
            assert_eq!(
                kalos[field], json[field],
                "SARIF properties.kalos[{field}] should match JSON"
            );
        }
    }
}
