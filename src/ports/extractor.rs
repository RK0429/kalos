use std::path::PathBuf;

use crate::domains::FilePath;
use crate::domains::cpg::SourceAnalysis;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractionRequest {
    pub workspace_root: PathBuf,
    pub analysis_targets: Vec<FilePath>,
}

pub trait ExtractorPort {
    type Error;

    fn extract(&self, request: &ExtractionRequest) -> Result<SourceAnalysis, Self::Error>;
}
