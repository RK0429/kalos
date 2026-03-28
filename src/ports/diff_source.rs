use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::domains::FilePath;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffRequest {
    pub workspace_root: PathBuf,
    pub base_ref: String,
    pub analysis_targets: Vec<FilePath>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffSnapshot {
    pub base_snapshot_hash: String,
    pub changed_files: BTreeSet<FilePath>,
}

pub trait DiffSourcePort {
    type Error;

    fn diff(&self, request: &DiffRequest) -> Result<DiffSnapshot, Self::Error>;
}
