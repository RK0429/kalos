use crate::domains::cpg::Language;
use crate::domains::diagnostics::{
    CpgSubgraphExcerpt, LlmSuggestionBundle, MetricObservation, PatternEvidence, SourceExcerpt,
};
use crate::domains::{DiagnosticId, FilePath, RuleId, Severity};

#[derive(Clone, Debug, PartialEq)]
pub struct LlmRequest {
    pub diagnostic_id: DiagnosticId,
    pub rule_id: RuleId,
    pub severity: Severity,
    pub language: Language,
    pub workspace_relative_path: FilePath,
    pub metric: Option<MetricObservation>,
    pub pattern: Option<PatternEvidence>,
    pub source_excerpt: Option<SourceExcerpt>,
    pub cpg_excerpt: Option<CpgSubgraphExcerpt>,
}

pub trait LlmPort {
    type Error;

    fn enrich(&self, requests: &[LlmRequest]) -> Result<LlmSuggestionBundle, Self::Error>;
}
