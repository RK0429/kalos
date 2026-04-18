use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCacheRequest {
    pub tool_name: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedToolBundle {
    pub tool_name: String,
    pub version: String,
    pub cache_path: PathBuf,
    pub checksum: String,
}

pub trait ToolCachePort {
    type Error;

    fn resolve_bundle(&self, request: &ToolCacheRequest)
    -> Result<ResolvedToolBundle, Self::Error>;
}
