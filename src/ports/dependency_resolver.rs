use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::domains::FilePath;
use crate::domains::cpg::{AnalysisWarning, CpgNode, SourceFile};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependencyResolutionRequest {
    pub workspace_root: PathBuf,
    pub source_files: BTreeMap<FilePath, SourceFile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependencyResolution {
    pub external_symbols: Vec<CpgNode>,
    pub warnings: Vec<AnalysisWarning>,
}

pub trait DependencyResolverPort {
    type Error;

    fn resolve(
        &self,
        request: &DependencyResolutionRequest,
    ) -> Result<DependencyResolution, Self::Error>;
}
