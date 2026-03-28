use std::cmp::Ordering;
use std::fmt;

macro_rules! string_newtype {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

string_newtype!(FilePath);
string_newtype!(RuleId);
string_newtype!(MetricId);
string_newtype!(DiagnosticId);

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnalysisLevel {
    Function,
    Module,
    Project,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ScopeId {
    pub level: AnalysisLevel,
    pub qualified_name: String,
    pub file_path: FilePath,
}

impl ScopeId {
    pub fn new(
        level: AnalysisLevel,
        qualified_name: impl Into<String>,
        file_path: impl Into<FilePath>,
    ) -> Self {
        Self {
            level,
            qualified_name: qualified_name.into(),
            file_path: file_path.into(),
        }
    }
}

impl Ord for ScopeId {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.level, &self.qualified_name, &self.file_path).cmp(&(
            other.level,
            &other.qualified_name,
            &other.file_path,
        ))
    }
}

impl PartialOrd for ScopeId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    const fn rank(self) -> u8 {
        match self {
            Self::Info => 0,
            Self::Warning => 1,
            Self::Error => 2,
        }
    }
}

impl Ord for Severity {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank().cmp(&other.rank())
    }
}

impl PartialOrd for Severity {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub mod config;
pub mod cpg;
pub mod diagnostics;
pub mod impact;
pub mod metrics;
pub mod reporting;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{AnalysisLevel, ScopeId, Severity};

    #[test]
    fn analysis_level_order_is_deterministic() {
        assert!(AnalysisLevel::Function < AnalysisLevel::Module);
        assert!(AnalysisLevel::Module < AnalysisLevel::Project);
    }

    #[test]
    fn scope_id_orders_by_level_then_name_then_path() {
        let mut scopes = BTreeSet::new();
        scopes.insert(ScopeId::new(AnalysisLevel::Project, "<project>", "."));
        scopes.insert(ScopeId::new(AnalysisLevel::Module, "alpha", "src/z.rs"));
        scopes.insert(ScopeId::new(AnalysisLevel::Function, "beta", "src/a.rs"));
        scopes.insert(ScopeId::new(AnalysisLevel::Function, "alpha", "src/b.rs"));
        scopes.insert(ScopeId::new(AnalysisLevel::Function, "alpha", "src/a.rs"));

        let ordered = scopes.into_iter().collect::<Vec<_>>();
        let qualified_names = ordered
            .iter()
            .map(|scope| {
                (
                    scope.level,
                    scope.qualified_name.as_str(),
                    scope.file_path.as_str(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            qualified_names,
            vec![
                (AnalysisLevel::Function, "alpha", "src/a.rs"),
                (AnalysisLevel::Function, "alpha", "src/b.rs"),
                (AnalysisLevel::Function, "beta", "src/a.rs"),
                (AnalysisLevel::Module, "alpha", "src/z.rs"),
                (AnalysisLevel::Project, "<project>", "."),
            ]
        );
    }

    #[test]
    fn severity_orders_by_impact() {
        assert!(Severity::Error > Severity::Warning);
        assert!(Severity::Warning > Severity::Info);
    }
}
