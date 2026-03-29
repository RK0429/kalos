use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::error::Error;
use std::fmt;

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::adapters::plugin::PluginHostError;
use crate::adapters::plugin::wasm::FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET;
use crate::domains::config::ProjectConfig;
use crate::domains::cpg::{SourceAnalysis, SourceLocation, UnifiedCpg};
use crate::domains::diagnostics::{
    CpgSubgraphExcerpt, Diagnostic, DiagnosticSummary, DiagnosticsScope, ExitCode, FileLocation,
    InlineSuppression, LlmSuggestionBundle, MetricRule, RuleConfig, SummaryScope,
    apply_suppressions, builtin_metric_rules, builtin_pattern_rules, project_subgraph,
};
use crate::domains::impact::{
    BaselineFingerprint, DiffBaseline, ImpactAnalysisInput, ScopeDiagnosticSnapshot,
    analyze_impact, build_dependency_index_from_cpg,
};
use crate::domains::metrics::{
    AnalysisMetrics, MetricConfig, MetricDefinition, MetricMetadata, ScopeMetrics,
    builtin_metric_definitions, metric_catalog_from_definitions,
};
use crate::domains::reporting::{
    AnalysisReport, ReportMetadata, ReportViewOptions, summary_scope_for,
};
use crate::domains::{AnalysisLevel, FilePath, MetricId, ScopeId, Severity};
use crate::ports::cache::CachePort;
use crate::ports::dependency_resolver::{DependencyResolutionRequest, DependencyResolverPort};
use crate::ports::diff_source::{DiffRequest, DiffSourcePort};
use crate::ports::extractor::{ExtractionRequest, ExtractorPort};
use crate::ports::llm::{LlmPort, LlmRequest};
use crate::ports::{PluginEvaluationRequest, PluginPort};

pub struct AnalysisPipeline<E, D> {
    extractor: E,
    dependency_resolver: D,
}

impl<E, D> AnalysisPipeline<E, D> {
    pub fn new(extractor: E, dependency_resolver: D) -> Self {
        Self {
            extractor,
            dependency_resolver,
        }
    }
}

pub struct PipelineResult {
    pub report: AnalysisReport,
    pub exit_code: ExitCode,
    pub llm_suggestions: Option<LlmSuggestionBundle>,
}

#[derive(Debug)]
pub enum PipelineError<E, D> {
    Extraction(E),
    DependencyResolution(D),
    Plugin(PluginHostError),
}

#[derive(Debug)]
pub enum DiffPipelineError<E, D, DS, C> {
    Pipeline(PipelineError<E, D>),
    DiffSource(DS),
    Cache(C),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffConfig {
    pub base_ref: String,
}

struct AnalysisArtifacts {
    source_analysis: SourceAnalysis,
    metrics: AnalysisMetrics,
    diagnostics: Vec<Diagnostic>,
}

struct PluginMetricsContext {
    definitions: Vec<Box<dyn MetricDefinition>>,
    metric_catalog: BTreeMap<MetricId, MetricMetadata>,
    loaded_metric_ids: BTreeSet<MetricId>,
}

#[derive(Debug)]
enum FullRunWithCacheError<E, D, C> {
    Pipeline(PipelineError<E, D>),
    Cache(C),
}

type DiffRunResult<E, D, DS, C> = Result<PipelineResult, DiffPipelineError<E, D, DS, C>>;
type FullRunWithCacheResult<E, D, C> = Result<PipelineResult, FullRunWithCacheError<E, D, C>>;

impl<E: fmt::Display, D: fmt::Display> fmt::Display for PipelineError<E, D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Extraction(error) => write!(f, "failed to extract CPG: {error}"),
            Self::DependencyResolution(error) => {
                write!(f, "failed to resolve external dependencies: {error}")
            }
            Self::Plugin(error) => write!(f, "failed to evaluate plugin metrics: {error}"),
        }
    }
}

impl<E, D> Error for PipelineError<E, D>
where
    E: Error + 'static,
    D: Error + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Extraction(error) => Some(error),
            Self::DependencyResolution(error) => Some(error),
            Self::Plugin(error) => Some(error),
        }
    }
}

impl<E: fmt::Display, D: fmt::Display, DS: fmt::Display, C: fmt::Display> fmt::Display
    for DiffPipelineError<E, D, DS, C>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pipeline(error) => write!(f, "{error}"),
            Self::DiffSource(error) => write!(f, "failed to read git diff: {error}"),
            Self::Cache(error) => write!(f, "failed to access diff baseline cache: {error}"),
        }
    }
}

impl<E, D, DS, C> Error for DiffPipelineError<E, D, DS, C>
where
    E: Error + 'static,
    D: Error + 'static,
    DS: Error + 'static,
    C: Error + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Pipeline(error) => Some(error),
            Self::DiffSource(error) => Some(error),
            Self::Cache(error) => Some(error),
        }
    }
}

impl<E, D, DS, C> DiffPipelineError<E, D, DS, C> {
    fn from_pipeline_or_cache(error: FullRunWithCacheError<E, D, C>) -> Self {
        match error {
            FullRunWithCacheError::Pipeline(error) => Self::Pipeline(error),
            FullRunWithCacheError::Cache(error) => Self::Cache(error),
        }
    }
}

impl<E, D> AnalysisPipeline<E, D>
where
    E: ExtractorPort,
    D: DependencyResolverPort,
{
    pub fn run(
        &self,
        config: &ProjectConfig,
        view_options: ReportViewOptions,
        plugin_host: Option<&mut dyn PluginPort<Error = PluginHostError>>,
        llm: Option<&dyn LlmPort<Error = Infallible>>,
    ) -> Result<PipelineResult, PipelineError<E::Error, D::Error>> {
        let plugin_metrics =
            load_plugin_metrics_context(plugin_host.as_deref()).map_err(PipelineError::Plugin)?;
        let source_analysis = self.extract_and_resolve(
            config,
            ExtractionRequest {
                workspace_root: config.workspace_root.abs_path.clone(),
                analysis_targets: config.analysis_targets.clone(),
            },
        )?;
        let artifacts = analyze_source_analysis(
            config,
            source_analysis,
            None,
            &plugin_metrics.definitions,
            plugin_host,
        )
        .map_err(PipelineError::Plugin)?;
        let llm_suggestions =
            maybe_enrich_with_llm(llm, &artifacts.diagnostics, &artifacts.source_analysis);
        Ok(finalize_result(
            assemble_report(
                config,
                artifacts.metrics,
                &plugin_metrics.metric_catalog,
                artifacts.diagnostics,
                DiagnosticsScope::WholeProject,
                view_options,
                None,
            ),
            llm_suggestions,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn run_diff<DS, C>(
        &self,
        config: &ProjectConfig,
        view_options: ReportViewOptions,
        diff_config: &DiffConfig,
        diff_source: &DS,
        cache: &C,
        mut plugin_host: Option<&mut dyn PluginPort<Error = PluginHostError>>,
        llm: Option<&dyn LlmPort<Error = Infallible>>,
    ) -> DiffRunResult<E::Error, D::Error, DS::Error, C::Error>
    where
        DS: DiffSourcePort,
        C: CachePort,
    {
        if config.targets_explicitly_specified {
            eprintln!(
                "diff mode is not available for explicitly specified targets; falling back to full analysis"
            );
            if let Some(host) = plugin_host.as_mut() {
                host.reset_aggregate_fuel_budget(FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET);
            }
            return self
                .run(config, view_options, plugin_host, llm)
                .map_err(DiffPipelineError::Pipeline);
        }

        let plugin_metrics = load_plugin_metrics_context(plugin_host.as_deref())
            .map_err(PipelineError::Plugin)
            .map_err(DiffPipelineError::Pipeline)?;
        let snapshot = diff_source
            .diff(&DiffRequest {
                workspace_root: config.workspace_root.abs_path.clone(),
                base_ref: diff_config.base_ref.clone(),
                analysis_targets: config.analysis_targets.clone(),
            })
            .map_err(DiffPipelineError::DiffSource)?;
        let fingerprint = build_baseline_fingerprint(config, &snapshot);
        let baseline = cache.load(&fingerprint).map_err(DiffPipelineError::Cache)?;

        if snapshot.changed_files.is_empty() {
            let metrics = if let Some(baseline) = baseline.as_ref() {
                metrics_from_scope_map(
                    config,
                    &baseline.scope_metrics,
                    &plugin_metrics.metric_catalog,
                    &plugin_metrics.loaded_metric_ids,
                )
            } else {
                if let Some(host) = plugin_host.as_mut() {
                    empty_metrics(config, &plugin_metrics.definitions, Some(&mut **host))
                } else {
                    empty_metrics(config, &plugin_metrics.definitions, None)
                }
                .map(|(metrics, _)| metrics)
                .map_err(PipelineError::Plugin)
                .map_err(DiffPipelineError::Pipeline)?
            };
            let summary_override = baseline.as_ref().and_then(|baseline| {
                (summary_scope_for(view_options.requested_level) == SummaryScope::WholeProject)
                    .then(|| summary_from_snapshots(&baseline.diagnostic_snapshots))
            });
            return Ok(finalize_result(
                assemble_report(
                    config,
                    metrics,
                    &plugin_metrics.metric_catalog,
                    Vec::new(),
                    DiagnosticsScope::AffectedOnly,
                    view_options,
                    summary_override,
                ),
                None,
            ));
        }

        let diff_source_analysis = self
            .extract_and_resolve(
                config,
                ExtractionRequest {
                    workspace_root: config.workspace_root.abs_path.clone(),
                    analysis_targets: snapshot.changed_files.iter().cloned().collect(),
                },
            )
            .map_err(DiffPipelineError::Pipeline)?;
        let impact = analyze_impact(&ImpactAnalysisInput {
            diff_cpg: &diff_source_analysis.cpg,
            changed_files: &snapshot.changed_files,
            baseline: baseline.as_ref(),
            base_snapshot_hash: &snapshot.base_snapshot_hash,
        });

        if baseline.is_none() {
            eprintln!("compatible diff baseline was not found; falling back to full analysis");
            if let Some(host) = plugin_host.as_mut() {
                host.reset_aggregate_fuel_budget(FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET);
            }
            return self
                .run_and_store_full_baseline(
                    config,
                    view_options,
                    fingerprint,
                    cache,
                    &plugin_metrics,
                    plugin_host,
                    llm,
                )
                .map_err(DiffPipelineError::from_pipeline_or_cache);
        }

        if impact.invalidation_plan.fallback_to_full {
            eprintln!(
                "diff optimization could not determine affected scopes; falling back to full analysis"
            );
            if let Some(host) = plugin_host.as_mut() {
                host.reset_aggregate_fuel_budget(FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET);
            }
            return self
                .run_and_store_full_baseline(
                    config,
                    view_options,
                    fingerprint,
                    cache,
                    &plugin_metrics,
                    plugin_host,
                    llm,
                )
                .map_err(DiffPipelineError::from_pipeline_or_cache);
        }

        let baseline = baseline.expect("baseline existence checked above");
        let actual_recomputed_scopes = actual_recomputed_scopes(
            &diff_source_analysis.cpg,
            &impact.invalidation_plan.recompute_scopes,
        );
        let baseline_fallback_scopes = impact
            .invalidation_plan
            .recompute_scopes
            .difference(&actual_recomputed_scopes)
            .cloned()
            .collect::<BTreeSet<_>>();
        let (recomputed_metrics, loaded_plugin_metric_ids) = compute_metrics_with_plugins(
            &diff_source_analysis.cpg,
            config,
            Some(&actual_recomputed_scopes),
            &plugin_metrics.definitions,
            plugin_host,
        )
        .map_err(PipelineError::Plugin)
        .map_err(DiffPipelineError::Pipeline)?;
        let merged_metrics = merge_metrics(
            config,
            recomputed_metrics,
            &baseline,
            &impact.invalidation_plan.reuse_scopes,
            &baseline_fallback_scopes,
            &plugin_metrics.metric_catalog,
            &loaded_plugin_metric_ids,
        );
        let merged_diagnostics =
            generate_diagnostics(&diff_source_analysis, &merged_metrics, config);
        let affected_diagnostics = merged_diagnostics
            .iter()
            .filter(|diagnostic| {
                impact
                    .affected_scopes
                    .scopes
                    .contains(&diagnostic.primary_scope_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        let merged_snapshots =
            merge_diagnostic_snapshots(&baseline, &actual_recomputed_scopes, &merged_diagnostics);
        let summary_override = match summary_scope_for(view_options.requested_level) {
            SummaryScope::WholeProject => Some(summary_from_snapshots(&merged_snapshots)),
            SummaryScope::ListedDiagnostics => None,
        };
        let merged_scope_metrics = scope_metrics_map(&merged_metrics);
        let overall_score = merged_metrics.overall_score.clone();
        let llm_suggestions =
            maybe_enrich_with_llm(llm, &affected_diagnostics, &diff_source_analysis);
        let report = assemble_report(
            config,
            merged_metrics,
            &plugin_metrics.metric_catalog,
            affected_diagnostics,
            DiagnosticsScope::AffectedOnly,
            view_options,
            summary_override,
        );
        cache
            .store(&DiffBaseline {
                fingerprint,
                dependency_index: impact.merged_dependency_index,
                scope_metrics: merged_scope_metrics,
                diagnostic_snapshots: merged_snapshots,
                overall_score,
            })
            .map_err(DiffPipelineError::Cache)?;

        Ok(finalize_result(report, llm_suggestions))
    }

    fn extract_and_resolve(
        &self,
        config: &ProjectConfig,
        request: ExtractionRequest,
    ) -> Result<SourceAnalysis, PipelineError<E::Error, D::Error>> {
        let mut source_analysis = self
            .extractor
            .extract(&request)
            .map_err(PipelineError::Extraction)?;
        apply_dependency_resolution(&self.dependency_resolver, config, &mut source_analysis)
            .map_err(PipelineError::DependencyResolution)?;
        Ok(source_analysis)
    }

    #[allow(clippy::too_many_arguments)]
    fn run_and_store_full_baseline<C>(
        &self,
        config: &ProjectConfig,
        view_options: ReportViewOptions,
        fingerprint: BaselineFingerprint,
        cache: &C,
        plugin_metrics: &PluginMetricsContext,
        plugin_host: Option<&mut dyn PluginPort<Error = PluginHostError>>,
        llm: Option<&dyn LlmPort<Error = Infallible>>,
    ) -> FullRunWithCacheResult<E::Error, D::Error, C::Error>
    where
        C: CachePort,
    {
        let source_analysis = self
            .extract_and_resolve(
                config,
                ExtractionRequest {
                    workspace_root: config.workspace_root.abs_path.clone(),
                    analysis_targets: config.analysis_targets.clone(),
                },
            )
            .map_err(FullRunWithCacheError::Pipeline)?;
        let artifacts = analyze_source_analysis(
            config,
            source_analysis,
            None,
            &plugin_metrics.definitions,
            plugin_host,
        )
        .map_err(PipelineError::Plugin)
        .map_err(FullRunWithCacheError::Pipeline)?;
        let llm_suggestions =
            maybe_enrich_with_llm(llm, &artifacts.diagnostics, &artifacts.source_analysis);
        let baseline = DiffBaseline {
            fingerprint,
            dependency_index: build_dependency_index_from_cpg(&artifacts.source_analysis.cpg),
            scope_metrics: scope_metrics_map(&artifacts.metrics),
            diagnostic_snapshots: diagnostic_snapshots_from_diagnostics(&artifacts.diagnostics),
            overall_score: artifacts.metrics.overall_score.clone(),
        };
        cache
            .store(&baseline)
            .map_err(FullRunWithCacheError::Cache)?;

        Ok(finalize_result(
            assemble_report(
                config,
                artifacts.metrics,
                &plugin_metrics.metric_catalog,
                artifacts.diagnostics,
                DiagnosticsScope::WholeProject,
                view_options,
                None,
            ),
            llm_suggestions,
        ))
    }
}

fn analyze_source_analysis(
    config: &ProjectConfig,
    source_analysis: SourceAnalysis,
    metric_scope_filter: Option<&BTreeSet<ScopeId>>,
    plugin_definitions: &[Box<dyn MetricDefinition>],
    plugin_host: Option<&mut dyn PluginPort<Error = PluginHostError>>,
) -> Result<AnalysisArtifacts, PluginHostError> {
    let (metrics, _) = compute_metrics_with_plugins(
        &source_analysis.cpg,
        config,
        metric_scope_filter,
        plugin_definitions,
        plugin_host,
    )?;
    let diagnostics = generate_diagnostics(&source_analysis, &metrics, config);

    Ok(AnalysisArtifacts {
        source_analysis,
        metrics,
        diagnostics,
    })
}

fn maybe_enrich_with_llm(
    llm: Option<&dyn LlmPort<Error = Infallible>>,
    diagnostics: &[Diagnostic],
    source_analysis: &SourceAnalysis,
) -> Option<LlmSuggestionBundle> {
    let llm = llm?;
    let requests = build_llm_requests(diagnostics, source_analysis);
    (!requests.is_empty()).then(|| {
        llm.enrich(&requests)
            .expect("Infallible LLM adapter should not fail")
    })
}

fn build_llm_requests(
    diagnostics: &[Diagnostic],
    source_analysis: &SourceAnalysis,
) -> Vec<LlmRequest> {
    diagnostics
        .iter()
        .filter_map(|diagnostic| {
            let source_file = source_analysis.source_files.get(&diagnostic.location.file_path)?;
            let (metric, pattern, scopes, representation) = match diagnostic.kind {
                crate::domains::diagnostics::DiagnosticKind::Metric => {
                    let metric = diagnostic.metric.clone()?;
                    let representation = format!(
                        "scope={} lines={}-{} metric={} normalized_risk={:.6} threshold={:.6} overflow_ratio={:.6}",
                        diagnostic.primary_scope_id.qualified_name,
                        diagnostic.location.start_line,
                        diagnostic.location.end_line,
                        metric.metric_id.as_str(),
                        metric.normalized_risk,
                        metric.threshold,
                        metric.overflow_ratio,
                    );
                    (
                        Some(metric),
                        None,
                        vec![diagnostic.primary_scope_id.clone()],
                        representation,
                    )
                }
                crate::domains::diagnostics::DiagnosticKind::Pattern => {
                    let pattern = diagnostic.pattern.clone()?;
                    if pattern
                        .evidence_scopes
                        .iter()
                        .any(|scope| scope.file_path != diagnostic.location.file_path)
                    {
                        return None;
                    }
                    (
                        None,
                        Some(pattern.clone()),
                        pattern.evidence_scopes.clone(),
                        format!(
                            "primary_scope={} lines={}-{} pattern={:?} evidence={}",
                            diagnostic.primary_scope_id.qualified_name,
                            diagnostic.location.start_line,
                            diagnostic.location.end_line,
                            pattern.pattern_type,
                            pattern.evidence_message,
                        ),
                    )
                }
            };

            Some(LlmRequest {
                diagnostic_id: diagnostic.id.clone(),
                rule_id: diagnostic.rule_id.clone(),
                severity: diagnostic.severity,
                language: source_file.language,
                workspace_relative_path: diagnostic.location.file_path.clone(),
                metric,
                pattern,
                source_excerpt: None,
                cpg_excerpt: Some(CpgSubgraphExcerpt {
                    scopes,
                    representation,
                }),
            })
        })
        .collect()
}

fn apply_dependency_resolution<D>(
    dependency_resolver: &D,
    config: &ProjectConfig,
    source_analysis: &mut SourceAnalysis,
) -> Result<(), D::Error>
where
    D: DependencyResolverPort,
{
    let dep_request = DependencyResolutionRequest {
        workspace_root: config.workspace_root.abs_path.clone(),
        source_files: source_analysis.source_files.clone(),
    };
    let dep_result = dependency_resolver.resolve(&dep_request)?;
    source_analysis
        .cpg
        .nodes
        .extend(dep_result.external_symbols);
    source_analysis.warnings.extend(dep_result.warnings);
    Ok(())
}

fn assemble_report(
    config: &ProjectConfig,
    metrics: AnalysisMetrics,
    metric_catalog: &BTreeMap<MetricId, MetricMetadata>,
    diagnostics: Vec<Diagnostic>,
    diagnostics_scope: DiagnosticsScope,
    view_options: ReportViewOptions,
    summary_override: Option<DiagnosticSummary>,
) -> AnalysisReport {
    AnalysisReport::project_with_metric_catalog(
        ReportMetadata::new(
            config.analysis_targets.clone(),
            env!("CARGO_PKG_VERSION"),
            "1.0.0",
        ),
        &metrics,
        metric_catalog,
        diagnostics,
        diagnostics_scope,
        view_options,
        summary_override,
    )
}

fn compute_metrics_with_plugins(
    cpg: &UnifiedCpg,
    config: &ProjectConfig,
    scope_filter: Option<&BTreeSet<ScopeId>>,
    plugin_definitions: &[Box<dyn MetricDefinition>],
    mut plugin_port: Option<&mut dyn PluginPort<Error = PluginHostError>>,
) -> Result<(AnalysisMetrics, BTreeSet<MetricId>), PluginHostError> {
    let definitions = builtin_metric_definitions();
    let metric_catalog = metric_catalog_with_plugins(plugin_definitions);
    let metric_config = MetricConfig {
        entries: BTreeMap::new(),
    };
    let function_scope_ids = filtered_scope_ids(function_scope_ids(cpg), scope_filter);
    let module_scope_ids = filtered_scope_ids(module_scope_ids(cpg), scope_filter);
    let project_scope_metrics = scope_filter
        .map(|scopes| scopes.contains(&project_scope_id()))
        .unwrap_or(true);

    let function_metrics = if let Some(host) = plugin_port.as_mut() {
        compute_scope_metrics_with_plugins(
            cpg,
            &function_scope_ids,
            AnalysisLevel::Function,
            &definitions,
            plugin_definitions,
            &metric_config,
            Some(&mut **host),
        )?
    } else {
        compute_scope_metrics_with_plugins(
            cpg,
            &function_scope_ids,
            AnalysisLevel::Function,
            &definitions,
            plugin_definitions,
            &metric_config,
            None,
        )?
    };
    let module_metrics = if let Some(host) = plugin_port.as_mut() {
        compute_scope_metrics_with_plugins(
            cpg,
            &module_scope_ids,
            AnalysisLevel::Module,
            &definitions,
            plugin_definitions,
            &metric_config,
            Some(&mut **host),
        )?
    } else {
        compute_scope_metrics_with_plugins(
            cpg,
            &module_scope_ids,
            AnalysisLevel::Module,
            &definitions,
            plugin_definitions,
            &metric_config,
            None,
        )?
    };
    let project_metrics = project_scope_metrics.then(|| {
        if let Some(host) = plugin_port.as_mut() {
            compute_scope_metrics_with_plugins(
                cpg,
                &[project_scope_id()],
                AnalysisLevel::Project,
                &definitions,
                plugin_definitions,
                &metric_config,
                Some(&mut **host),
            )
        } else {
            compute_scope_metrics_with_plugins(
                cpg,
                &[project_scope_id()],
                AnalysisLevel::Project,
                &definitions,
                plugin_definitions,
                &metric_config,
                None,
            )
        }
        .map(|mut metrics| {
            metrics
                .drain(..)
                .next()
                .expect("project scope metrics should always exist")
        })
    });

    Ok((
        AnalysisMetrics::assemble(
            function_metrics,
            module_metrics,
            project_metrics.transpose()?,
            &config.score_weights,
            &metric_catalog,
            &config.rules,
        ),
        loaded_plugin_metric_ids(plugin_definitions),
    ))
}

fn compute_scope_metrics_with_plugins(
    cpg: &UnifiedCpg,
    scope_ids: &[ScopeId],
    level: AnalysisLevel,
    definitions: &[Box<dyn MetricDefinition>],
    plugin_definitions: &[Box<dyn MetricDefinition>],
    metric_config: &MetricConfig,
    mut plugin_port: Option<&mut dyn PluginPort<Error = PluginHostError>>,
) -> Result<Vec<ScopeMetrics>, PluginHostError> {
    let mut scope_metrics = Vec::with_capacity(scope_ids.len());
    for scope_id in scope_ids {
        let subgraph = cpg.subgraph(scope_id);
        let mut values = definitions
            .iter()
            .filter(|definition| definition.level() == level)
            .filter_map(|definition| definition.compute(&subgraph, metric_config))
            .collect::<Vec<_>>();

        if let Some(plugin_port) = plugin_port.as_deref_mut() {
            for definition in plugin_definitions
                .iter()
                .filter(|definition| definition.level() == level)
            {
                let request = PluginEvaluationRequest {
                    scope_id: scope_id.clone(),
                    subgraph: subgraph.clone(),
                    config: metric_config.clone(),
                };
                if let Some(value) = plugin_port.evaluate(definition.as_ref(), &request)? {
                    values.push(value);
                }
            }
        }

        scope_metrics.push(ScopeMetrics::new(scope_id.clone(), values));
    }
    scope_metrics.sort_by(|left, right| left.scope_id.cmp(&right.scope_id));
    Ok(scope_metrics)
}

fn finalize_result(
    report: AnalysisReport,
    llm_suggestions: Option<LlmSuggestionBundle>,
) -> PipelineResult {
    let exit_code = report.diagnostics.determine_exit_code(report.view.strict);
    PipelineResult {
        report,
        exit_code,
        llm_suggestions,
    }
}

fn empty_metrics(
    config: &ProjectConfig,
    plugin_definitions: &[Box<dyn MetricDefinition>],
    plugin_port: Option<&mut dyn PluginPort<Error = PluginHostError>>,
) -> Result<(AnalysisMetrics, BTreeSet<MetricId>), PluginHostError> {
    compute_metrics_with_plugins(
        &UnifiedCpg {
            id: crate::domains::cpg::CpgId::from("empty"),
            nodes: Vec::new(),
            edges: Vec::new(),
        },
        config,
        Some(&BTreeSet::new()),
        plugin_definitions,
        plugin_port,
    )
}

fn load_plugin_metrics_context(
    plugin_host: Option<&dyn PluginPort<Error = PluginHostError>>,
) -> Result<PluginMetricsContext, PluginHostError> {
    let definitions = plugin_host
        .map(|host| host.load_metric_definitions())
        .transpose()?
        .unwrap_or_default();

    Ok(PluginMetricsContext {
        metric_catalog: metric_catalog_with_plugins(&definitions),
        loaded_metric_ids: loaded_plugin_metric_ids(&definitions),
        definitions,
    })
}

fn metric_catalog_with_plugins(
    plugin_definitions: &[Box<dyn MetricDefinition>],
) -> BTreeMap<MetricId, MetricMetadata> {
    let definitions = builtin_metric_definitions();
    let mut definition_refs = definitions
        .iter()
        .map(|definition| definition.as_ref())
        .collect::<Vec<_>>();
    definition_refs.extend(
        plugin_definitions
            .iter()
            .map(|definition| definition.as_ref()),
    );
    metric_catalog_from_definitions(definition_refs)
}

fn loaded_plugin_metric_ids(
    plugin_definitions: &[Box<dyn MetricDefinition>],
) -> BTreeSet<MetricId> {
    plugin_definitions
        .iter()
        .map(|definition| definition.id().clone())
        .collect()
}

fn builtin_metric_ids() -> BTreeSet<MetricId> {
    builtin_metric_definitions()
        .into_iter()
        .map(|definition| definition.id().clone())
        .collect()
}

fn filter_reused_scope_metrics(
    scope_metrics: &ScopeMetrics,
    builtin_metric_ids: &BTreeSet<MetricId>,
    loaded_plugin_metric_ids: &BTreeSet<MetricId>,
) -> ScopeMetrics {
    let mut filtered = scope_metrics.clone();
    filtered.values.retain(|value| {
        builtin_metric_ids.contains(&value.metric_id)
            || loaded_plugin_metric_ids.contains(&value.metric_id)
    });
    filtered
}

fn filtered_scope_ids(
    scope_ids: Vec<ScopeId>,
    scope_filter: Option<&BTreeSet<ScopeId>>,
) -> Vec<ScopeId> {
    scope_filter.map_or(scope_ids.clone(), |filter| {
        scope_ids
            .into_iter()
            .filter(|scope_id| filter.contains(scope_id))
            .collect()
    })
}

fn merge_metrics(
    config: &ProjectConfig,
    recomputed_metrics: AnalysisMetrics,
    baseline: &DiffBaseline,
    reuse_scopes: &BTreeSet<ScopeId>,
    baseline_fallback_scopes: &BTreeSet<ScopeId>,
    metric_catalog: &BTreeMap<MetricId, MetricMetadata>,
    loaded_plugin_metric_ids: &BTreeSet<MetricId>,
) -> AnalysisMetrics {
    let builtin_metric_ids = builtin_metric_ids();
    let mut function_metrics = baseline
        .scope_metrics
        .iter()
        .filter(|(scope_id, _)| {
            (reuse_scopes.contains(*scope_id) || baseline_fallback_scopes.contains(*scope_id))
                && scope_id.level == AnalysisLevel::Function
        })
        .map(|(_, metrics)| {
            filter_reused_scope_metrics(metrics, &builtin_metric_ids, loaded_plugin_metric_ids)
        })
        .collect::<Vec<_>>();
    function_metrics.extend(recomputed_metrics.function_metrics);
    function_metrics.sort_by(|left, right| left.scope_id.cmp(&right.scope_id));

    let mut module_metrics = baseline
        .scope_metrics
        .iter()
        .filter(|(scope_id, _)| {
            (reuse_scopes.contains(*scope_id) || baseline_fallback_scopes.contains(*scope_id))
                && scope_id.level == AnalysisLevel::Module
        })
        .map(|(_, metrics)| {
            filter_reused_scope_metrics(metrics, &builtin_metric_ids, loaded_plugin_metric_ids)
        })
        .collect::<Vec<_>>();
    module_metrics.extend(recomputed_metrics.module_metrics);
    module_metrics.sort_by(|left, right| left.scope_id.cmp(&right.scope_id));

    let project_metrics = recomputed_metrics.project_metrics.or_else(|| {
        baseline
            .scope_metrics
            .get(&project_scope_id())
            .map(|metrics| {
                filter_reused_scope_metrics(metrics, &builtin_metric_ids, loaded_plugin_metric_ids)
            })
            .filter(|_| {
                reuse_scopes.contains(&project_scope_id())
                    || baseline_fallback_scopes.contains(&project_scope_id())
            })
    });

    AnalysisMetrics::assemble(
        function_metrics,
        module_metrics,
        project_metrics,
        &config.score_weights,
        metric_catalog,
        &config.rules,
    )
}

fn metrics_from_scope_map(
    config: &ProjectConfig,
    scope_metrics: &BTreeMap<ScopeId, ScopeMetrics>,
    metric_catalog: &BTreeMap<MetricId, MetricMetadata>,
    loaded_plugin_metric_ids: &BTreeSet<MetricId>,
) -> AnalysisMetrics {
    let builtin_metric_ids = builtin_metric_ids();
    let mut function_metrics = scope_metrics
        .iter()
        .filter(|(scope_id, _)| scope_id.level == AnalysisLevel::Function)
        .map(|(_, metrics)| {
            filter_reused_scope_metrics(metrics, &builtin_metric_ids, loaded_plugin_metric_ids)
        })
        .collect::<Vec<_>>();
    function_metrics.sort_by(|left, right| left.scope_id.cmp(&right.scope_id));

    let mut module_metrics = scope_metrics
        .iter()
        .filter(|(scope_id, _)| scope_id.level == AnalysisLevel::Module)
        .map(|(_, metrics)| {
            filter_reused_scope_metrics(metrics, &builtin_metric_ids, loaded_plugin_metric_ids)
        })
        .collect::<Vec<_>>();
    module_metrics.sort_by(|left, right| left.scope_id.cmp(&right.scope_id));

    let project_metrics = scope_metrics.get(&project_scope_id()).map(|metrics| {
        filter_reused_scope_metrics(metrics, &builtin_metric_ids, loaded_plugin_metric_ids)
    });

    AnalysisMetrics::assemble(
        function_metrics,
        module_metrics,
        project_metrics,
        &config.score_weights,
        metric_catalog,
        &config.rules,
    )
}

fn scope_metrics_map(metrics: &AnalysisMetrics) -> BTreeMap<ScopeId, ScopeMetrics> {
    metrics
        .function_metrics
        .iter()
        .chain(metrics.module_metrics.iter())
        .chain(metrics.project_metrics.iter())
        .map(|scope_metrics| (scope_metrics.scope_id.clone(), scope_metrics.clone()))
        .collect()
}

fn diagnostic_snapshots_from_diagnostics(
    diagnostics: &[Diagnostic],
) -> BTreeMap<ScopeId, ScopeDiagnosticSnapshot> {
    let mut snapshots = BTreeMap::<ScopeId, ScopeDiagnosticSnapshot>::new();

    for diagnostic in diagnostics {
        let entry = snapshots
            .entry(diagnostic.primary_scope_id.clone())
            .or_insert_with(|| ScopeDiagnosticSnapshot {
                scope_id: diagnostic.primary_scope_id.clone(),
                diagnostic_ids: Vec::new(),
                summary: DiagnosticSummary {
                    error_count: 0,
                    warning_count: 0,
                    info_count: 0,
                },
            });
        entry.diagnostic_ids.push(diagnostic.id.clone());
        increment_summary(&mut entry.summary, diagnostic.severity);
    }

    snapshots
}

fn merge_diagnostic_snapshots(
    baseline: &DiffBaseline,
    actual_recomputed_scopes: &BTreeSet<ScopeId>,
    merged_diagnostics: &[Diagnostic],
) -> BTreeMap<ScopeId, ScopeDiagnosticSnapshot> {
    let recomputed_snapshots = diagnostic_snapshots_from_diagnostics(
        &merged_diagnostics
            .iter()
            .filter(|diagnostic| actual_recomputed_scopes.contains(&diagnostic.primary_scope_id))
            .cloned()
            .collect::<Vec<_>>(),
    );
    let mut snapshots = baseline
        .diagnostic_snapshots
        .iter()
        .filter(|(scope_id, _)| !actual_recomputed_scopes.contains(*scope_id))
        .map(|(scope_id, snapshot)| (scope_id.clone(), snapshot.clone()))
        .collect::<BTreeMap<_, _>>();
    for (scope_id, snapshot) in recomputed_snapshots {
        snapshots.insert(scope_id, snapshot);
    }
    snapshots
}

fn actual_recomputed_scopes(
    cpg: &UnifiedCpg,
    recompute_scopes: &BTreeSet<ScopeId>,
) -> BTreeSet<ScopeId> {
    recompute_scopes
        .intersection(&known_scope_ids(cpg))
        .cloned()
        .collect()
}

fn known_scope_ids(cpg: &UnifiedCpg) -> BTreeSet<ScopeId> {
    let mut scopes = function_scope_ids(cpg).into_iter().collect::<BTreeSet<_>>();
    scopes.extend(module_scope_ids(cpg));
    scopes.insert(project_scope_id());
    scopes
}

fn summary_from_snapshots(
    snapshots: &BTreeMap<ScopeId, ScopeDiagnosticSnapshot>,
) -> DiagnosticSummary {
    snapshots.values().fold(
        DiagnosticSummary {
            error_count: 0,
            warning_count: 0,
            info_count: 0,
        },
        |mut summary, snapshot| {
            summary.error_count += snapshot.summary.error_count;
            summary.warning_count += snapshot.summary.warning_count;
            summary.info_count += snapshot.summary.info_count;
            summary
        },
    )
}

fn increment_summary(summary: &mut DiagnosticSummary, severity: Severity) {
    match severity {
        Severity::Error => summary.error_count += 1,
        Severity::Warning => summary.warning_count += 1,
        Severity::Info => summary.info_count += 1,
    }
}

fn build_baseline_fingerprint(
    config: &ProjectConfig,
    snapshot: &crate::ports::diff_source::DiffSnapshot,
) -> BaselineFingerprint {
    BaselineFingerprint {
        workspace_root_hash: sha256_hex(
            config.workspace_root.abs_path.to_string_lossy().as_bytes(),
        ),
        base_snapshot_hash: snapshot.base_snapshot_hash.clone(),
        config_hash: config_hash(config),
        analysis_targets_hash: analysis_targets_hash(&config.analysis_targets),
        rule_catalog_version: "builtin-v1".to_owned(),
        extractor_version: "codeql-v1".to_owned(),
        kalos_version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

fn config_hash(config: &ProjectConfig) -> String {
    let rules = config
        .rules
        .iter()
        .map(|(rule_id, rule_config)| {
            (
                rule_id.as_str().to_owned(),
                json!({
                    "enabled": rule_config.enabled,
                    "threshold": rule_config.threshold,
                    "severity": rule_config.severity.map(severity_label),
                }),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let payload = json!({
        "rules": rules,
        "score_weights": {
            "function": config.score_weights.function,
            "module": config.score_weights.module,
            "project": config.score_weights.project,
        },
        "plugin_manifest": config
            .plugin_manifest
            .modules
            .iter()
            .map(|module| {
                json!({
                    "path": module.workspace_relative_path.as_str(),
                    "sha256": module.sha256,
                })
            })
            .collect::<Vec<_>>(),
    });

    sha256_hex(
        serde_json::to_vec(&payload)
            .expect("diff baseline config payload should serialize")
            .as_slice(),
    )
}

fn analysis_targets_hash(analysis_targets: &[FilePath]) -> String {
    let normalized = analysis_targets
        .iter()
        .map(|path| path.as_str().to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let payload =
        serde_json::to_vec(&normalized).expect("analysis targets payload should serialize");

    sha256_hex(&payload)
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
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
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::convert::Infallible;

    use super::{
        AnalysisPipeline, DiffConfig, FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        apply_dependency_resolution, assemble_report, build_baseline_fingerprint,
        build_llm_requests, compute_metrics_with_plugins, merge_metrics, metrics_from_scope_map,
        project_scope_id,
    };
    use crate::adapters::plugin::PluginHostError;
    use crate::domains::config::{Defaults, ProjectConfig, WorkspaceRoot};
    use crate::domains::cpg::{
        AnalysisWarning, CpgEdge, CpgId, CpgNode, EdgeKind, Language, NodeId, NodeKind,
        SourceAnalysis, SourceFile, SourceLocation, UnifiedCpg,
    };
    use crate::domains::diagnostics::{
        Diagnostic, DiagnosticKind, DiagnosticSummary, DiagnosticsScope, FileLocation,
        LlmSuggestion, LlmSuggestionBundle, MetricObservation, PatternEvidence, PatternType,
        SummaryScope, TemplateSuggestion,
    };
    use crate::domains::impact::{
        BaselineFingerprint, DependencyIndexManifest, DiffBaseline, ScopeDiagnosticSnapshot,
    };
    use crate::domains::metrics::{
        AnalysisMetrics, MetricConfig, MetricDefinition, MetricOrigin, MetricParticipation,
        MetricValue, OverallScore, builtin_metric_definitions, metric_catalog_from_definitions,
    };
    use crate::domains::reporting::{
        OutputFormat, ReportScopeMetrics, ReportViewOptions, RequestedLevel,
    };
    use crate::domains::{
        AnalysisLevel, DiagnosticId, FilePath, MetricId, RuleId, ScopeId, Severity,
    };
    use crate::ports::cache::CachePort;
    use crate::ports::dependency_resolver::{
        DependencyResolution, DependencyResolutionRequest, DependencyResolverPort,
    };
    use crate::ports::diff_source::{DiffRequest, DiffSnapshot, DiffSourcePort};
    use crate::ports::extractor::{ExtractionRequest, ExtractorPort};
    use crate::ports::llm::{LlmPort, LlmRequest};
    use crate::ports::{PluginEvaluationRequest, PluginPort};

    #[derive(Clone, Debug)]
    struct TestPluginMetricDefinition {
        id: MetricId,
        level: AnalysisLevel,
    }

    impl MetricDefinition for TestPluginMetricDefinition {
        fn id(&self) -> &MetricId {
            &self.id
        }

        fn name(&self) -> &str {
            "test-plugin-metric"
        }

        fn level(&self) -> AnalysisLevel {
            self.level
        }

        fn origin(&self) -> MetricOrigin {
            MetricOrigin::Plugin
        }

        fn participation(&self) -> MetricParticipation {
            MetricParticipation::ReportOnly
        }

        fn rule_binding(&self) -> Option<&RuleId> {
            None
        }

        fn description(&self) -> &str {
            "test plugin metric"
        }

        fn compute(
            &self,
            _subgraph: &crate::domains::cpg::CpgSubgraph,
            _config: &MetricConfig,
        ) -> Option<MetricValue> {
            None
        }
    }

    #[derive(Default)]
    struct MockPluginPort {
        definitions: Vec<TestPluginMetricDefinition>,
        responses: BTreeMap<(MetricId, ScopeId), MetricValue>,
        reset_calls: Vec<u64>,
    }

    impl PluginPort for MockPluginPort {
        type Error = PluginHostError;

        fn load_metric_definitions(&self) -> Result<Vec<Box<dyn MetricDefinition>>, Self::Error> {
            Ok(self
                .definitions
                .iter()
                .cloned()
                .map(|definition| Box::new(definition) as Box<dyn MetricDefinition>)
                .collect())
        }

        fn reset_aggregate_fuel_budget(&mut self, budget: u64) {
            self.reset_calls.push(budget);
        }

        fn evaluate(
            &mut self,
            definition: &dyn MetricDefinition,
            request: &PluginEvaluationRequest,
        ) -> Result<Option<MetricValue>, Self::Error> {
            Ok(self
                .responses
                .get(&(definition.id().clone(), request.scope_id.clone()))
                .cloned())
        }
    }

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

    #[derive(Debug)]
    struct RoutingExtractor {
        responses: BTreeMap<String, SourceAnalysis>,
        requests: RefCell<Vec<Vec<FilePath>>>,
    }

    impl RoutingExtractor {
        fn new(responses: BTreeMap<String, SourceAnalysis>) -> Self {
            Self {
                responses,
                requests: RefCell::new(Vec::new()),
            }
        }
    }

    impl ExtractorPort for RoutingExtractor {
        type Error = Infallible;

        fn extract(&self, request: &ExtractionRequest) -> Result<SourceAnalysis, Self::Error> {
            self.requests
                .borrow_mut()
                .push(request.analysis_targets.clone());
            Ok(self
                .responses
                .get(&request_key(&request.analysis_targets))
                .expect("extractor response should exist for requested targets")
                .clone())
        }
    }

    #[derive(Debug)]
    struct MockDiffSource {
        snapshot: DiffSnapshot,
        requests: RefCell<Vec<DiffRequest>>,
    }

    impl MockDiffSource {
        fn new(snapshot: DiffSnapshot) -> Self {
            Self {
                snapshot,
                requests: RefCell::new(Vec::new()),
            }
        }
    }

    impl DiffSourcePort for MockDiffSource {
        type Error = Infallible;

        fn diff(&self, request: &DiffRequest) -> Result<DiffSnapshot, Self::Error> {
            self.requests.borrow_mut().push(request.clone());
            Ok(self.snapshot.clone())
        }
    }

    #[derive(Debug)]
    struct MockCache {
        loaded: Option<DiffBaseline>,
        load_fingerprints: RefCell<Vec<BaselineFingerprint>>,
        stored: RefCell<Vec<DiffBaseline>>,
    }

    impl MockCache {
        fn new(loaded: Option<DiffBaseline>) -> Self {
            Self {
                loaded,
                load_fingerprints: RefCell::new(Vec::new()),
                stored: RefCell::new(Vec::new()),
            }
        }
    }

    impl CachePort for MockCache {
        type Error = Infallible;

        fn load(
            &self,
            fingerprint: &BaselineFingerprint,
        ) -> Result<Option<DiffBaseline>, Self::Error> {
            self.load_fingerprints
                .borrow_mut()
                .push(fingerprint.clone());
            Ok(self.loaded.clone())
        }

        fn store(&self, baseline: &DiffBaseline) -> Result<(), Self::Error> {
            self.stored.borrow_mut().push(baseline.clone());
            Ok(())
        }
    }

    #[derive(Clone, Debug, Default)]
    struct NullDependencyResolver;

    impl DependencyResolverPort for NullDependencyResolver {
        type Error = Infallible;

        fn resolve(
            &self,
            _request: &DependencyResolutionRequest,
        ) -> Result<DependencyResolution, Self::Error> {
            Ok(DependencyResolution {
                external_symbols: Vec::new(),
                warnings: Vec::new(),
            })
        }
    }

    #[derive(Clone, Debug)]
    struct MockDependencyResolver {
        resolution: DependencyResolution,
    }

    impl DependencyResolverPort for MockDependencyResolver {
        type Error = Infallible;

        fn resolve(
            &self,
            _request: &DependencyResolutionRequest,
        ) -> Result<DependencyResolution, Self::Error> {
            Ok(self.resolution.clone())
        }
    }

    #[derive(Debug)]
    struct MockLlmPort {
        bundle: LlmSuggestionBundle,
        requests: RefCell<Vec<Vec<LlmRequest>>>,
    }

    impl MockLlmPort {
        fn new(bundle: LlmSuggestionBundle) -> Self {
            Self {
                bundle,
                requests: RefCell::new(Vec::new()),
            }
        }
    }

    impl LlmPort for MockLlmPort {
        type Error = Infallible;

        fn enrich(&self, requests: &[LlmRequest]) -> Result<LlmSuggestionBundle, Self::Error> {
            self.requests.borrow_mut().push(requests.to_vec());
            Ok(self.bundle.clone())
        }
    }

    #[test]
    fn assemble_report_materializes_level_summary_and_strict_exit_code() {
        let report = assemble_report(
            &fixture_config(),
            fixture_metrics(),
            &fixture_metric_catalog(),
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
            DiagnosticsScope::WholeProject,
            ReportViewOptions {
                requested_level: RequestedLevel::Function,
                output_format: OutputFormat::Json,
                strict: true,
                minimum_severity: None,
            },
            None,
        );

        assert_eq!(report.diagnostics.summary.error_count, 0);
        assert_eq!(report.diagnostics.summary.warning_count, 1);
        assert_eq!(
            report.diagnostics.determine_exit_code(report.view.strict),
            crate::domains::diagnostics::ExitCode::DiagnosticFailure
        );
    }

    #[test]
    fn compute_metrics_includes_report_only_plugin_values_without_affecting_scores() {
        let source_analysis = warning_source_analysis("src/lib.rs", "crate::f");
        let plugin_metric_id = MetricId::from("P-F001");
        let function_scope = ScopeId::new(AnalysisLevel::Function, "crate::f", "src/lib.rs");
        let mut plugin_port = MockPluginPort {
            definitions: vec![TestPluginMetricDefinition {
                id: plugin_metric_id.clone(),
                level: AnalysisLevel::Function,
            }],
            responses: BTreeMap::from([(
                (plugin_metric_id.clone(), function_scope.clone()),
                MetricValue {
                    metric_id: plugin_metric_id.clone(),
                    raw_value: 99.0,
                    normalized_risk: 1.0,
                },
            )]),
            ..Default::default()
        };
        let plugin_definitions = plugin_port.load_metric_definitions().unwrap();
        let builtin_only =
            compute_metrics_with_plugins(&source_analysis.cpg, &fixture_config(), None, &[], None)
                .unwrap()
                .0;
        let (with_plugins, loaded_metric_ids) = compute_metrics_with_plugins(
            &source_analysis.cpg,
            &fixture_config(),
            None,
            &plugin_definitions,
            Some(&mut plugin_port),
        )
        .unwrap();

        assert_eq!(
            loaded_metric_ids,
            BTreeSet::from([plugin_metric_id.clone()])
        );
        assert!(
            with_plugins.function_metrics[0]
                .values
                .iter()
                .any(|value| value.metric_id == plugin_metric_id)
        );
        assert_eq!(
            with_plugins.function_metrics[0].scope_risk,
            builtin_only.function_metrics[0].scope_risk
        );
        assert_eq!(with_plugins.overall_score, builtin_only.overall_score);
    }

    #[test]
    fn metrics_from_scope_map_filters_unloaded_plugin_values() {
        let scope_id = ScopeId::new(AnalysisLevel::Function, "crate::f", "src/lib.rs");
        let plugin_metric_id = MetricId::from("P-F001");
        let scope_metrics = BTreeMap::from([(
            scope_id.clone(),
            ScopeMetricsLiteral::new(
                scope_id,
                0.0,
                vec![("M-F001", 1.0, 0.55), ("P-F001", 99.0, 1.0)],
            )
            .into(),
        )]);

        let filtered = metrics_from_scope_map(
            &fixture_config(),
            &scope_metrics,
            &fixture_metric_catalog(),
            &BTreeSet::new(),
        );
        assert_eq!(filtered.function_metrics[0].values.len(), 1);
        assert_eq!(
            filtered.function_metrics[0].values[0].metric_id,
            MetricId::from("M-F001")
        );

        let retained = metrics_from_scope_map(
            &fixture_config(),
            &scope_metrics,
            &fixture_metric_catalog_with_plugin(plugin_metric_id.clone()),
            &BTreeSet::from([plugin_metric_id.clone()]),
        );
        assert!(
            retained.function_metrics[0]
                .values
                .iter()
                .any(|value| value.metric_id == plugin_metric_id)
        );
    }

    #[test]
    fn merge_metrics_filters_unloaded_plugin_values_from_reused_baseline() {
        let unchanged_scope = ScopeId::new(
            AnalysisLevel::Function,
            "crate::unchanged",
            "src/unchanged.rs",
        );
        let baseline = DiffBaseline {
            fingerprint: build_baseline_fingerprint(
                &fixture_config(),
                &DiffSnapshot {
                    base_snapshot_hash: "base-tree".to_owned(),
                    changed_files: BTreeSet::new(),
                },
            ),
            dependency_index: DependencyIndexManifest {
                reverse_dependencies: BTreeMap::new(),
            },
            scope_metrics: BTreeMap::from([(
                unchanged_scope.clone(),
                ScopeMetricsLiteral::new(
                    unchanged_scope.clone(),
                    0.0,
                    vec![("M-F001", 1.0, 0.55), ("P-F001", 99.0, 1.0)],
                )
                .into(),
            )]),
            diagnostic_snapshots: BTreeMap::new(),
            overall_score: OverallScore {
                function_risk: Some(0.0),
                module_risk: None,
                project_risk: None,
                overall_risk: 0.0,
                overall_score: 100,
                function_score: Some(100),
                module_score: None,
                project_score: None,
            },
        };

        let merged = merge_metrics(
            &fixture_config(),
            AnalysisMetrics {
                function_metrics: Vec::new(),
                module_metrics: Vec::new(),
                project_metrics: None,
                overall_score: OverallScore {
                    function_risk: None,
                    module_risk: None,
                    project_risk: None,
                    overall_risk: 0.0,
                    overall_score: 100,
                    function_score: None,
                    module_score: None,
                    project_score: None,
                },
            },
            &baseline,
            &BTreeSet::from([unchanged_scope]),
            &BTreeSet::new(),
            &fixture_metric_catalog(),
            &BTreeSet::new(),
        );

        assert_eq!(merged.function_metrics[0].values.len(), 1);
        assert_eq!(
            merged.function_metrics[0].values[0].metric_id,
            MetricId::from("M-F001")
        );
    }

    #[test]
    fn baseline_fingerprint_changes_when_plugin_manifest_changes() {
        let snapshot = DiffSnapshot {
            base_snapshot_hash: "base-tree".to_owned(),
            changed_files: BTreeSet::from([FilePath::from("src/lib.rs")]),
        };
        let mut with_plugin = fixture_config();
        with_plugin
            .plugin_manifest
            .modules
            .push(crate::domains::config::PluginModuleRef {
                workspace_relative_path: FilePath::from(".kalos/plugins/example.wasm"),
                sha256: "a".repeat(64),
            });

        assert_ne!(
            build_baseline_fingerprint(&fixture_config(), &snapshot).config_hash,
            build_baseline_fingerprint(&with_plugin, &snapshot).config_hash
        );
    }

    #[test]
    fn analysis_pipeline_runs_non_diff_flow_with_stub_extractor() {
        let pipeline = AnalysisPipeline::new(
            MockExtractor {
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
            },
            NullDependencyResolver,
        );

        let result = pipeline
            .run(
                &fixture_config(),
                ReportViewOptions {
                    requested_level: RequestedLevel::All,
                    output_format: OutputFormat::Json,
                    strict: false,
                    minimum_severity: None,
                },
                None,
                None,
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

    #[test]
    fn run_diff_falls_back_without_diff_io_for_explicit_targets() {
        let config = subset_fixture_config();
        let pipeline = AnalysisPipeline::new(
            RoutingExtractor::new(BTreeMap::from([(
                request_key(&config.analysis_targets),
                warning_source_analysis("src/lib.rs", "crate::subset"),
            )])),
            NullDependencyResolver,
        );
        let diff_source = MockDiffSource::new(DiffSnapshot {
            base_snapshot_hash: "base-tree".to_owned(),
            changed_files: BTreeSet::from([FilePath::from("src/lib.rs")]),
        });
        let cache = MockCache::new(None);
        let mut plugin_port = MockPluginPort::default();

        let result = pipeline
            .run_diff(
                &config,
                fixture_view_options(RequestedLevel::All, false),
                &DiffConfig {
                    base_ref: "HEAD~1".to_owned(),
                },
                &diff_source,
                &cache,
                Some(&mut plugin_port),
                None,
            )
            .expect("subset fallback should succeed");

        assert_eq!(
            result.report.diagnostics.diagnostics_scope,
            DiagnosticsScope::WholeProject
        );
        assert!(diff_source.requests.borrow().is_empty());
        assert!(cache.load_fingerprints.borrow().is_empty());
        assert!(cache.stored.borrow().is_empty());
        assert_eq!(
            plugin_port.reset_calls,
            vec![FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET]
        );
    }

    #[test]
    fn run_diff_falls_back_and_stores_full_baseline_when_cache_is_missing() {
        let config = fixture_config();
        let snapshot = DiffSnapshot {
            base_snapshot_hash: "base-tree".to_owned(),
            changed_files: BTreeSet::from([FilePath::from("src/changed.rs")]),
        };
        let pipeline = AnalysisPipeline::new(
            RoutingExtractor::new(BTreeMap::from([
                (
                    request_key(&[FilePath::from("src/changed.rs")]),
                    warning_source_analysis("src/changed.rs", "crate::changed"),
                ),
                (
                    request_key(&config.analysis_targets),
                    warning_source_analysis("src/lib.rs", "crate::full"),
                ),
            ])),
            NullDependencyResolver,
        );
        let diff_source = MockDiffSource::new(snapshot.clone());
        let cache = MockCache::new(None);
        let mut plugin_port = MockPluginPort::default();

        let result = pipeline
            .run_diff(
                &config,
                fixture_view_options(RequestedLevel::All, false),
                &DiffConfig {
                    base_ref: "HEAD~1".to_owned(),
                },
                &diff_source,
                &cache,
                Some(&mut plugin_port),
                None,
            )
            .expect("missing baseline should fall back successfully");

        assert_eq!(
            result.report.diagnostics.diagnostics_scope,
            DiagnosticsScope::WholeProject
        );
        assert_eq!(
            pipeline.extractor.requests.borrow().as_slice(),
            &[
                vec![FilePath::from("src/changed.rs")],
                vec![FilePath::from(".")],
            ]
        );
        assert_eq!(cache.stored.borrow().len(), 1);
        assert_eq!(
            cache.stored.borrow()[0].fingerprint,
            build_baseline_fingerprint(&config, &snapshot)
        );
        assert_eq!(
            plugin_port.reset_calls,
            vec![FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET]
        );
    }

    #[test]
    fn run_diff_uses_merged_snapshots_for_whole_project_summary() {
        let config = fixture_config();
        let snapshot = DiffSnapshot {
            base_snapshot_hash: "base-tree".to_owned(),
            changed_files: BTreeSet::from([FilePath::from("src/changed.rs")]),
        };
        let changed_scope =
            ScopeId::new(AnalysisLevel::Function, "crate::changed", "src/changed.rs");
        let unchanged_scope = ScopeId::new(
            AnalysisLevel::Function,
            "crate::unchanged",
            "src/unchanged.rs",
        );
        let fingerprint = build_baseline_fingerprint(&config, &snapshot);
        let baseline = baseline_fixture(
            fingerprint,
            unchanged_scope.clone(),
            DiagnosticSummary {
                error_count: 1,
                warning_count: 0,
                info_count: 0,
            },
            None,
        );
        let pipeline = AnalysisPipeline::new(
            RoutingExtractor::new(BTreeMap::from([(
                request_key(&[FilePath::from("src/changed.rs")]),
                warning_source_analysis("src/changed.rs", "crate::changed"),
            )])),
            NullDependencyResolver,
        );
        let diff_source = MockDiffSource::new(snapshot);
        let cache = MockCache::new(Some(baseline));

        let result = pipeline
            .run_diff(
                &config,
                fixture_view_options(RequestedLevel::All, false),
                &DiffConfig {
                    base_ref: "HEAD~1".to_owned(),
                },
                &diff_source,
                &cache,
                None,
                None,
            )
            .expect("diff run should succeed");

        assert_eq!(
            result.report.diagnostics.diagnostics_scope,
            DiagnosticsScope::AffectedOnly
        );
        assert_eq!(
            result.report.diagnostics.summary_scope,
            SummaryScope::WholeProject
        );
        assert_eq!(result.report.diagnostics.summary.error_count, 1);
        assert_eq!(result.report.diagnostics.summary.warning_count, 1);
        assert_eq!(
            result.exit_code,
            crate::domains::diagnostics::ExitCode::DiagnosticFailure
        );
        assert_eq!(result.report.diagnostics.diagnostics.len(), 1);
        assert_eq!(
            result.report.diagnostics.diagnostics[0].primary_scope_id,
            changed_scope
        );
        let stored = cache.stored.borrow();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].scope_metrics.contains_key(&unchanged_scope));
        assert_eq!(
            stored[0]
                .diagnostic_snapshots
                .get(&unchanged_scope)
                .expect("reused snapshot should remain")
                .summary
                .error_count,
            1
        );
    }

    #[test]
    fn run_diff_uses_listed_diagnostics_summary_for_scoped_reports() {
        let config = fixture_config();
        let snapshot = DiffSnapshot {
            base_snapshot_hash: "base-tree".to_owned(),
            changed_files: BTreeSet::from([FilePath::from("src/changed.rs")]),
        };
        let fingerprint = build_baseline_fingerprint(&config, &snapshot);
        let baseline = baseline_fixture(
            fingerprint,
            ScopeId::new(
                AnalysisLevel::Function,
                "crate::unchanged",
                "src/unchanged.rs",
            ),
            DiagnosticSummary {
                error_count: 1,
                warning_count: 0,
                info_count: 0,
            },
            None,
        );
        let pipeline = AnalysisPipeline::new(
            RoutingExtractor::new(BTreeMap::from([(
                request_key(&[FilePath::from("src/changed.rs")]),
                warning_source_analysis("src/changed.rs", "crate::changed"),
            )])),
            NullDependencyResolver,
        );
        let diff_source = MockDiffSource::new(snapshot);
        let cache = MockCache::new(Some(baseline));

        let result = pipeline
            .run_diff(
                &config,
                fixture_view_options(RequestedLevel::Function, false),
                &DiffConfig {
                    base_ref: "HEAD~1".to_owned(),
                },
                &diff_source,
                &cache,
                None,
                None,
            )
            .expect("scoped diff run should succeed");

        assert_eq!(
            result.report.diagnostics.diagnostics_scope,
            DiagnosticsScope::AffectedOnly
        );
        assert_eq!(
            result.report.diagnostics.summary_scope,
            SummaryScope::ListedDiagnostics
        );
        assert_eq!(result.report.diagnostics.summary.error_count, 0);
        assert_eq!(result.report.diagnostics.summary.warning_count, 1);
        assert_eq!(
            result.exit_code,
            crate::domains::diagnostics::ExitCode::Success
        );
    }

    #[test]
    fn run_diff_empty_diff_uses_baseline_summary_for_whole_project_reports() {
        let config = fixture_config();
        let snapshot = DiffSnapshot {
            base_snapshot_hash: "base-tree".to_owned(),
            changed_files: BTreeSet::new(),
        };
        let fingerprint = build_baseline_fingerprint(&config, &snapshot);
        let baseline = baseline_fixture(
            fingerprint,
            ScopeId::new(
                AnalysisLevel::Function,
                "crate::unchanged",
                "src/unchanged.rs",
            ),
            DiagnosticSummary {
                error_count: 1,
                warning_count: 0,
                info_count: 0,
            },
            None,
        );
        let pipeline = AnalysisPipeline::new(
            RoutingExtractor::new(BTreeMap::<String, SourceAnalysis>::new()),
            NullDependencyResolver,
        );
        let diff_source = MockDiffSource::new(snapshot);
        let cache = MockCache::new(Some(baseline));

        let result = pipeline
            .run_diff(
                &config,
                fixture_view_options(RequestedLevel::All, false),
                &DiffConfig {
                    base_ref: "HEAD~1".to_owned(),
                },
                &diff_source,
                &cache,
                None,
                None,
            )
            .expect("empty diff should succeed");

        assert_eq!(
            result.report.diagnostics.diagnostics_scope,
            DiagnosticsScope::AffectedOnly
        );
        assert_eq!(
            result.report.diagnostics.summary_scope,
            SummaryScope::WholeProject
        );
        assert_eq!(result.report.diagnostics.summary.error_count, 1);
        assert!(pipeline.extractor.requests.borrow().is_empty());
    }

    #[test]
    fn run_diff_falls_back_when_loaded_baseline_is_incompatible() {
        let config = fixture_config();
        let snapshot = DiffSnapshot {
            base_snapshot_hash: "base-tree".to_owned(),
            changed_files: BTreeSet::from([FilePath::from("src/changed.rs")]),
        };
        let fingerprint = build_baseline_fingerprint(&config, &snapshot);
        let baseline = baseline_fixture(
            fingerprint,
            ScopeId::new(
                AnalysisLevel::Function,
                "crate::unchanged",
                "src/unchanged.rs",
            ),
            DiagnosticSummary {
                error_count: 0,
                warning_count: 0,
                info_count: 0,
            },
            Some("stale-tree"),
        );
        let pipeline = AnalysisPipeline::new(
            RoutingExtractor::new(BTreeMap::from([
                (
                    request_key(&[FilePath::from("src/changed.rs")]),
                    warning_source_analysis("src/changed.rs", "crate::changed"),
                ),
                (
                    request_key(&config.analysis_targets),
                    warning_source_analysis("src/lib.rs", "crate::full"),
                ),
            ])),
            NullDependencyResolver,
        );
        let diff_source = MockDiffSource::new(snapshot);
        let cache = MockCache::new(Some(baseline));
        let mut plugin_port = MockPluginPort::default();

        let result = pipeline
            .run_diff(
                &config,
                fixture_view_options(RequestedLevel::All, false),
                &DiffConfig {
                    base_ref: "HEAD~1".to_owned(),
                },
                &diff_source,
                &cache,
                Some(&mut plugin_port),
                None,
            )
            .expect("incompatible baseline should fall back");

        assert_eq!(
            result.report.diagnostics.diagnostics_scope,
            DiagnosticsScope::WholeProject
        );
        assert_eq!(
            pipeline.extractor.requests.borrow().as_slice(),
            &[
                vec![FilePath::from("src/changed.rs")],
                vec![FilePath::from(".")],
            ]
        );
        assert_eq!(cache.stored.borrow().len(), 1);
        assert_eq!(
            plugin_port.reset_calls,
            vec![FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET]
        );
    }

    #[test]
    fn dependency_resolution_merges_external_symbols_and_warnings() {
        let mut source_analysis = SourceAnalysis {
            cpg: UnifiedCpg {
                id: CpgId::from("stub"),
                nodes: Vec::new(),
                edges: Vec::new(),
            },
            source_files: BTreeMap::from([(
                FilePath::from("src/lib.rs"),
                SourceFile {
                    path: FilePath::from("src/lib.rs"),
                    language: Language::Rust,
                },
            )]),
            suppressions: Vec::new(),
            warnings: Vec::new(),
        };

        apply_dependency_resolution(
            &MockDependencyResolver {
                resolution: DependencyResolution {
                    external_symbols: vec![CpgNode {
                        id: NodeId::from(99),
                        kind: NodeKind::ExternalSymbol,
                        name: "std::fmt::Debug".to_owned(),
                        location: SourceLocation {
                            file_path: FilePath::from("src/lib.rs"),
                            start_line: 1,
                            end_line: 1,
                        },
                        extension: None,
                    }],
                    warnings: vec![AnalysisWarning {
                        file_path: FilePath::from("src/lib.rs"),
                        message: "External symbol resolution is not yet implemented (REQ-FUNC-007). Analysis results may be incomplete for cross-crate/cross-package references.".to_owned(),
                    }],
                },
            },
            &fixture_config(),
            &mut source_analysis,
        )
        .unwrap();

        assert_eq!(source_analysis.cpg.nodes.len(), 1);
        assert_eq!(source_analysis.cpg.nodes[0].kind, NodeKind::ExternalSymbol);
        assert_eq!(source_analysis.warnings.len(), 1);
        assert_eq!(
            source_analysis.warnings[0].message,
            "External symbol resolution is not yet implemented (REQ-FUNC-007). Analysis results may be incomplete for cross-crate/cross-package references."
        );
    }

    #[test]
    fn build_llm_requests_uses_source_file_language_and_skips_multifile_patterns() {
        let diagnostics = vec![
            metric_diagnostic(
                "diag-metric",
                AnalysisLevel::Function,
                "crate::f",
                "src/lib.rs",
                Severity::Warning,
                "KAL-F001",
                "M-F001",
            ),
            pattern_diagnostic(
                "diag-pattern",
                "src/lib.rs",
                vec![
                    ScopeId::new(AnalysisLevel::Function, "crate::f", "src/lib.rs"),
                    ScopeId::new(AnalysisLevel::Function, "crate::g", "src/other.rs"),
                ],
            ),
        ];
        let source_analysis = SourceAnalysis {
            cpg: UnifiedCpg {
                id: CpgId::from("graph"),
                nodes: Vec::new(),
                edges: Vec::new(),
            },
            source_files: BTreeMap::from([
                (
                    FilePath::from("src/lib.rs"),
                    SourceFile {
                        path: FilePath::from("src/lib.rs"),
                        language: Language::Rust,
                    },
                ),
                (
                    FilePath::from("src/other.rs"),
                    SourceFile {
                        path: FilePath::from("src/other.rs"),
                        language: Language::Rust,
                    },
                ),
            ]),
            suppressions: Vec::new(),
            warnings: Vec::new(),
        };

        let requests = build_llm_requests(&diagnostics, &source_analysis);

        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].diagnostic_id, DiagnosticId::from("diag-metric"));
        assert_eq!(requests[0].language, Language::Rust);
        assert!(requests[0].metric.is_some());
        assert!(requests[0].pattern.is_none());
        assert!(requests[0].source_excerpt.is_none());
        assert!(requests[0].cpg_excerpt.is_some());
    }

    #[test]
    fn llm_enrichment_does_not_change_report_or_exit_code() {
        let source_analysis = warning_source_analysis("src/lib.rs", "crate::f");
        let pipeline = AnalysisPipeline::new(
            MockExtractor {
                source_analysis: source_analysis.clone(),
            },
            NullDependencyResolver,
        );
        let without_llm = pipeline
            .run(
                &fixture_config(),
                ReportViewOptions {
                    requested_level: RequestedLevel::All,
                    output_format: OutputFormat::Json,
                    strict: false,
                    minimum_severity: None,
                },
                None,
                None,
            )
            .expect("pipeline should succeed without llm");
        let diagnostic_id = without_llm.report.diagnostics.diagnostics[0].id.clone();
        let llm_port = MockLlmPort::new(LlmSuggestionBundle {
            enrichments: BTreeMap::from([(
                diagnostic_id.clone(),
                LlmSuggestion {
                    explanation: "LLM explanation".to_owned(),
                    code_example: Some("fn helper() {}".to_owned()),
                },
            )]),
        });

        let with_llm = pipeline
            .run(
                &fixture_config(),
                ReportViewOptions {
                    requested_level: RequestedLevel::All,
                    output_format: OutputFormat::Json,
                    strict: false,
                    minimum_severity: None,
                },
                None,
                Some(&llm_port),
            )
            .expect("pipeline should succeed with llm");

        assert_eq!(with_llm.report, without_llm.report);
        assert_eq!(with_llm.exit_code, without_llm.exit_code);
        assert_eq!(
            with_llm
                .llm_suggestions
                .as_ref()
                .expect("llm suggestions should exist")
                .enrichments
                .get(&diagnostic_id)
                .expect("suggestion should exist")
                .explanation,
            "LLM explanation"
        );
        assert_eq!(llm_port.requests.borrow().len(), 1);
        assert!(!llm_port.requests.borrow()[0].is_empty());
    }

    fn fixture_view_options(requested_level: RequestedLevel, strict: bool) -> ReportViewOptions {
        ReportViewOptions {
            requested_level,
            output_format: OutputFormat::Json,
            strict,
            minimum_severity: None,
        }
    }

    fn subset_fixture_config() -> ProjectConfig {
        ProjectConfig {
            analysis_targets: vec![FilePath::from("src/lib.rs")],
            targets_explicitly_specified: true,
            ..fixture_config()
        }
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

    fn request_key(targets: &[FilePath]) -> String {
        targets
            .iter()
            .map(FilePath::as_str)
            .collect::<Vec<_>>()
            .join("|")
    }

    fn warning_source_analysis(file_path: &str, function_name: &str) -> SourceAnalysis {
        let function_id = NodeId::from(1);
        let branch_a = NodeId::from(2);
        let branch_b = NodeId::from(3);
        let branch_c = NodeId::from(4);

        SourceAnalysis {
            cpg: UnifiedCpg {
                id: CpgId::from(format!("graph:{file_path}")),
                nodes: vec![
                    CpgNode {
                        id: function_id,
                        kind: NodeKind::Function,
                        name: function_name.to_owned(),
                        location: SourceLocation {
                            file_path: FilePath::from(file_path),
                            start_line: 1,
                            end_line: 5,
                        },
                        extension: None,
                    },
                    CpgNode {
                        id: branch_a,
                        kind: NodeKind::Variable,
                        name: "branch_alpha".to_owned(),
                        location: SourceLocation {
                            file_path: FilePath::from(file_path),
                            start_line: 2,
                            end_line: 2,
                        },
                        extension: None,
                    },
                    CpgNode {
                        id: branch_b,
                        kind: NodeKind::Variable,
                        name: "branch_beta".to_owned(),
                        location: SourceLocation {
                            file_path: FilePath::from(file_path),
                            start_line: 3,
                            end_line: 3,
                        },
                        extension: None,
                    },
                    CpgNode {
                        id: branch_c,
                        kind: NodeKind::Variable,
                        name: "branch_gamma".to_owned(),
                        location: SourceLocation {
                            file_path: FilePath::from(file_path),
                            start_line: 4,
                            end_line: 4,
                        },
                        extension: None,
                    },
                ],
                edges: vec![
                    CpgEdge {
                        source: function_id,
                        target: branch_a,
                        kind: EdgeKind::ControlFlow,
                        extension: None,
                    },
                    CpgEdge {
                        source: function_id,
                        target: branch_b,
                        kind: EdgeKind::ControlFlow,
                        extension: None,
                    },
                    CpgEdge {
                        source: function_id,
                        target: branch_c,
                        kind: EdgeKind::ControlFlow,
                        extension: None,
                    },
                ],
            },
            source_files: BTreeMap::from([(
                FilePath::from(file_path),
                SourceFile {
                    path: FilePath::from(file_path),
                    language: Language::Rust,
                },
            )]),
            suppressions: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn baseline_fixture(
        fingerprint: BaselineFingerprint,
        unchanged_scope: ScopeId,
        summary: DiagnosticSummary,
        base_snapshot_override: Option<&str>,
    ) -> DiffBaseline {
        let project_scope = project_scope_id();
        let changed_scope =
            ScopeId::new(AnalysisLevel::Function, "crate::changed", "src/changed.rs");
        let base_snapshot_hash = base_snapshot_override
            .unwrap_or(&fingerprint.base_snapshot_hash)
            .to_owned();
        let mut reverse_dependencies = BTreeMap::new();
        reverse_dependencies.insert(
            changed_scope.clone(),
            BTreeSet::from([unchanged_scope.clone()]),
        );
        reverse_dependencies.insert(unchanged_scope.clone(), BTreeSet::new());
        reverse_dependencies.insert(project_scope.clone(), BTreeSet::new());

        DiffBaseline {
            fingerprint: BaselineFingerprint {
                base_snapshot_hash,
                ..fingerprint
            },
            dependency_index: DependencyIndexManifest {
                reverse_dependencies,
            },
            scope_metrics: BTreeMap::from([(
                unchanged_scope.clone(),
                ScopeMetricsLiteral::new(unchanged_scope.clone(), 0.0, vec![("M-F001", 0.0, 0.0)])
                    .into(),
            )]),
            diagnostic_snapshots: BTreeMap::from([(
                unchanged_scope.clone(),
                ScopeDiagnosticSnapshot {
                    scope_id: unchanged_scope,
                    diagnostic_ids: vec![DiagnosticId::from("baseline-error")],
                    summary,
                },
            )]),
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

    fn fixture_metric_catalog() -> BTreeMap<MetricId, crate::domains::metrics::MetricMetadata> {
        let definitions = builtin_metric_definitions();
        let definition_refs = definitions
            .iter()
            .map(|definition| definition.as_ref())
            .collect::<Vec<_>>();
        metric_catalog_from_definitions(definition_refs)
    }

    fn fixture_metric_catalog_with_plugin(
        plugin_metric_id: MetricId,
    ) -> BTreeMap<MetricId, crate::domains::metrics::MetricMetadata> {
        let mut catalog = fixture_metric_catalog();
        catalog.insert(
            plugin_metric_id,
            crate::domains::metrics::MetricMetadata {
                participation: MetricParticipation::ReportOnly,
                rule_binding: None,
            },
        );
        catalog
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

    fn pattern_diagnostic(id: &str, file_path: &str, evidence_scopes: Vec<ScopeId>) -> Diagnostic {
        Diagnostic {
            id: DiagnosticId::from(id),
            primary_scope_id: ScopeId::new(AnalysisLevel::Function, "crate::pattern", file_path),
            rule_id: RuleId::from("KAL-PAT002"),
            kind: DiagnosticKind::Pattern,
            severity: Severity::Warning,
            location: FileLocation {
                file_path: FilePath::from(file_path),
                start_line: 20,
                end_line: 24,
                column: Some(1),
            },
            message: "pattern diagnostic".to_owned(),
            metric: None,
            pattern: Some(PatternEvidence {
                pattern_type: PatternType::FeatureEnvy,
                evidence_scopes,
                evidence_message: "cross-module access".to_owned(),
            }),
            template_suggestion: TemplateSuggestion {
                explanation: "move the behavior closer to the data".to_owned(),
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
