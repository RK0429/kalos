use crate::domains::ScopeId;
use crate::domains::cpg::CpgSubgraph;
use crate::domains::metrics::{MetricConfig, MetricDefinition, MetricValue};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginEvaluationRequest {
    pub scope_id: ScopeId,
    pub subgraph: CpgSubgraph,
    pub config: MetricConfig,
}

pub trait PluginPort {
    type Error;

    fn load_metric_definitions(&self) -> Result<Vec<Box<dyn MetricDefinition>>, Self::Error>;
    fn evaluate(
        &self,
        definition: &dyn MetricDefinition,
        request: &PluginEvaluationRequest,
    ) -> Result<Option<MetricValue>, Self::Error>;
}
