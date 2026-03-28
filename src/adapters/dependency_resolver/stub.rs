use std::convert::Infallible;

use crate::domains::FilePath;
use crate::domains::cpg::AnalysisWarning;
use crate::ports::dependency_resolver::{
    DependencyResolution, DependencyResolutionRequest, DependencyResolverPort,
};

#[derive(Clone, Debug, Default)]
pub struct StubDependencyResolver;

impl DependencyResolverPort for StubDependencyResolver {
    type Error = Infallible;

    fn resolve(
        &self,
        _request: &DependencyResolutionRequest,
    ) -> Result<DependencyResolution, Self::Error> {
        Ok(DependencyResolution {
            external_symbols: Vec::new(),
            warnings: vec![AnalysisWarning {
                file_path: FilePath::from("."),
                message: "External symbol resolution is not yet implemented (REQ-FUNC-007). Analysis results may be incomplete for cross-crate/cross-package references.".to_owned(),
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::StubDependencyResolver;
    use crate::ports::dependency_resolver::{DependencyResolutionRequest, DependencyResolverPort};

    #[test]
    fn stub_dependency_resolver_returns_explicit_warning() {
        let resolver = StubDependencyResolver;
        let resolution = resolver
            .resolve(&DependencyResolutionRequest {
                workspace_root: "/workspace".into(),
                source_files: BTreeMap::new(),
            })
            .unwrap();

        assert!(resolution.external_symbols.is_empty());
        assert_eq!(resolution.warnings.len(), 1);
        assert_eq!(
            resolution.warnings[0].message,
            "External symbol resolution is not yet implemented (REQ-FUNC-007). Analysis results may be incomplete for cross-crate/cross-package references."
        );
    }
}
