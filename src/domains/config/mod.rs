use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use crate::domains::diagnostics::RuleConfig;
use crate::domains::metrics::ScoreWeights;
use crate::domains::{FilePath, RuleId, Severity};
use crate::platform::fs::path_to_forward_slashes;

pub const CONFIG_FILE_NAME: &str = ".kalos.toml";

pub(crate) const DEFAULT_METRIC_RULE_THRESHOLDS: [(&str, f64); 10] = [
    ("KAL-F001", 0.55),
    ("KAL-F002", 0.60),
    ("KAL-F003", 0.45),
    ("KAL-F004", 0.55),
    ("KAL-M001", 0.50),
    ("KAL-M002", 0.20),
    ("KAL-M003", 0.75),
    ("KAL-P001", 0.15),
    ("KAL-P002", 0.45),
    ("KAL-P003", 0.35),
];

pub(crate) const DEFAULT_PATTERN_RULE_SEVERITIES: [(&str, Severity); 3] = [
    ("KAL-PAT001", Severity::Warning),
    ("KAL-PAT002", Severity::Warning),
    ("KAL-PAT003", Severity::Error),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceRoot {
    pub abs_path: PathBuf,
}

impl WorkspaceRoot {
    pub fn as_path(&self) -> &Path {
        &self.abs_path
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobPattern {
    pub pattern: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginModuleRef {
    pub workspace_relative_path: FilePath,
    pub sha256: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolvedPluginManifest {
    pub modules: Vec<PluginModuleRef>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectConfig {
    pub workspace_root: WorkspaceRoot,
    pub analysis_targets: Vec<FilePath>,
    pub rules: BTreeMap<RuleId, RuleConfig>,
    pub exclude_patterns: Vec<GlobPattern>,
    pub score_weights: ScoreWeights,
    pub plugin_manifest: ResolvedPluginManifest,
    pub targets_explicitly_specified: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolveOptions {
    pub cwd: PathBuf,
    pub config_path: Option<PathBuf>,
    pub analysis_targets: Vec<PathBuf>,
    pub targets_explicitly_specified: bool,
    pub exclude_patterns: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredWorkspace {
    pub workspace_root: WorkspaceRoot,
    pub config_path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Defaults {
    pub score_weights: ScoreWeights,
    pub rules: BTreeMap<RuleId, RuleConfig>,
}

impl Default for Defaults {
    fn default() -> Self {
        let mut rules = BTreeMap::new();

        for (rule_id, threshold) in DEFAULT_METRIC_RULE_THRESHOLDS {
            rules.insert(
                RuleId::from(rule_id),
                RuleConfig {
                    enabled: Some(true),
                    threshold: Some(threshold),
                    severity: None,
                },
            );
        }

        for (rule_id, severity) in DEFAULT_PATTERN_RULE_SEVERITIES {
            rules.insert(
                RuleId::from(rule_id),
                RuleConfig {
                    enabled: Some(true),
                    threshold: None,
                    severity: Some(severity),
                },
            );
        }

        Self {
            score_weights: ScoreWeights {
                function: 0.4,
                module: 0.35,
                project: 0.25,
            },
            rules,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConfigFile {
    pub path: PathBuf,
    pub content: ConfigContent,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConfigContent {
    pub exclude_patterns: Vec<String>,
    pub rules: BTreeMap<RuleId, RuleConfig>,
    pub score_weights: ScoreWeightsOverride,
    pub plugins: Vec<PluginConfig>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScoreWeightsOverride {
    pub function: Option<f64>,
    pub module: Option<f64>,
    pub project: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginConfig {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to canonicalize workspace root `{path}`: {source}")]
    CanonicalizeWorkspaceRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to read config file `{path}`: {source}")]
    ReadConfig {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to parse config file `{path}`: {source}")]
    ParseConfig {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("config path `{path}` has no parent directory")]
    MissingConfigParent { path: PathBuf },
    #[error("invalid severity `{value}` for rule `{rule_id}`")]
    InvalidSeverity { rule_id: RuleId, value: String },
    #[error("score weight `{name}` must be finite and greater than 0.0, got `{value}`")]
    InvalidScoreWeight { name: &'static str, value: f64 },
    #[error("rule `{rule_id}` threshold must be within [0.0, 1.0], got `{value}`")]
    InvalidThreshold { rule_id: RuleId, value: f64 },
    #[error("plugin `{path}` sha256 must be a 64-character hex string, got `{value}`")]
    InvalidSha256 { path: String, value: String },
    #[error("{kind} path `{path}` is outside workspace root `{workspace_root}`")]
    PathOutsideWorkspace {
        kind: &'static str,
        path: PathBuf,
        workspace_root: PathBuf,
    },
}

impl ConfigFile {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = normalize_absolute_path(path.as_ref());
        let contents = fs::read_to_string(&path).map_err(|source| ConfigError::ReadConfig {
            path: path.clone(),
            source,
        })?;

        Self::parse(path, &contents)
    }

    pub fn parse(path: impl Into<PathBuf>, contents: &str) -> Result<Self, ConfigError> {
        let path = path.into();
        let raw: RawConfigFile =
            toml::from_str(contents).map_err(|source| ConfigError::ParseConfig {
                path: path.clone(),
                source,
            })?;

        let mut rules = BTreeMap::new();
        for (rule_id, rule) in raw.rules {
            let rule_id = RuleId::from(rule_id);
            let severity = rule
                .severity
                .map(|value| parse_severity(&rule_id, value))
                .transpose()?;
            rules.insert(
                rule_id,
                RuleConfig {
                    enabled: rule.enabled,
                    threshold: rule.threshold,
                    severity,
                },
            );
        }

        let score_weights = raw
            .score
            .weights
            .map(|weights| ScoreWeightsOverride {
                function: weights.function,
                module: weights.module,
                project: weights.project,
            })
            .unwrap_or_default();

        let plugins = raw
            .plugins
            .into_iter()
            .map(|plugin| PluginConfig {
                path: plugin.path,
                sha256: plugin.sha256,
            })
            .collect();

        Ok(Self {
            path,
            content: ConfigContent {
                exclude_patterns: raw.general.exclude,
                rules,
                score_weights,
                plugins,
            },
        })
    }
}

impl ProjectConfig {
    pub fn discover_workspace(
        options: &ResolveOptions,
    ) -> Result<DiscoveredWorkspace, ConfigError> {
        if let Some(config_path) = &options.config_path {
            let config_path = absolute_from_base(&options.cwd, config_path);
            let parent = config_path
                .parent()
                .ok_or_else(|| ConfigError::MissingConfigParent {
                    path: config_path.clone(),
                })?;
            let config_path = canonicalize_if_exists(&config_path)?;

            return Ok(DiscoveredWorkspace {
                workspace_root: WorkspaceRoot {
                    abs_path: canonicalize_workspace_root(parent)?,
                },
                config_path: Some(config_path),
            });
        }

        if let Some(config_path) = find_upward(&options.cwd, CONFIG_FILE_NAME) {
            let parent = config_path
                .parent()
                .ok_or_else(|| ConfigError::MissingConfigParent {
                    path: config_path.clone(),
                })?;
            let config_path = canonicalize_if_exists(&config_path)?;

            return Ok(DiscoveredWorkspace {
                workspace_root: WorkspaceRoot {
                    abs_path: canonicalize_workspace_root(parent)?,
                },
                config_path: Some(config_path),
            });
        }

        if let Some(git_path) = find_upward(&options.cwd, ".git") {
            let parent = git_path.parent().unwrap_or_else(|| Path::new("/"));
            return Ok(DiscoveredWorkspace {
                workspace_root: WorkspaceRoot {
                    abs_path: canonicalize_workspace_root(parent)?,
                },
                config_path: None,
            });
        }

        Ok(DiscoveredWorkspace {
            workspace_root: WorkspaceRoot {
                abs_path: canonicalize_workspace_root(&options.cwd)?,
            },
            config_path: None,
        })
    }

    pub fn resolve(
        options: &ResolveOptions,
        config_file: Option<&ConfigFile>,
        defaults: &Defaults,
    ) -> Result<Self, ConfigError> {
        let workspace_root = Self::discover_workspace(options)?.workspace_root;
        Self::resolve_with_workspace(workspace_root, options, config_file, defaults)
    }

    pub fn load_and_resolve(
        options: &ResolveOptions,
        defaults: &Defaults,
    ) -> Result<Self, ConfigError> {
        let discovery = Self::discover_workspace(options)?;
        let config_file = discovery
            .config_path
            .as_ref()
            .map(ConfigFile::load)
            .transpose()?;

        Self::resolve_with_workspace(
            discovery.workspace_root,
            options,
            config_file.as_ref(),
            defaults,
        )
    }

    fn resolve_with_workspace(
        workspace_root: WorkspaceRoot,
        options: &ResolveOptions,
        config_file: Option<&ConfigFile>,
        defaults: &Defaults,
    ) -> Result<Self, ConfigError> {
        let effective_targets = if options.analysis_targets.is_empty() {
            vec![PathBuf::from(".")]
        } else {
            options.analysis_targets.clone()
        };

        let analysis_targets = effective_targets
            .iter()
            .map(|target| normalize_relative_path(&workspace_root, target, "analysis target"))
            .collect::<Result<Vec<_>, _>>()?;

        let mut seen_patterns = BTreeSet::new();
        let mut exclude_patterns = Vec::new();
        for pattern in config_file
            .map(|file| file.content.exclude_patterns.iter().map(String::as_str))
            .into_iter()
            .flatten()
            .chain(options.exclude_patterns.iter().map(String::as_str))
        {
            if seen_patterns.insert(pattern.to_owned()) {
                exclude_patterns.push(GlobPattern {
                    pattern: pattern.to_owned(),
                });
            }
        }

        let score_weights = ScoreWeights {
            function: config_file
                .and_then(|file| file.content.score_weights.function)
                .unwrap_or(defaults.score_weights.function),
            module: config_file
                .and_then(|file| file.content.score_weights.module)
                .unwrap_or(defaults.score_weights.module),
            project: config_file
                .and_then(|file| file.content.score_weights.project)
                .unwrap_or(defaults.score_weights.project),
        };
        validate_score_weights(&score_weights)?;

        let mut rule_ids = BTreeSet::new();
        rule_ids.extend(defaults.rules.keys().cloned());
        if let Some(file) = config_file {
            rule_ids.extend(file.content.rules.keys().cloned());
        }

        let mut rules = BTreeMap::new();
        for rule_id in rule_ids {
            let base = defaults.rules.get(&rule_id).cloned().unwrap_or(RuleConfig {
                enabled: Some(true),
                threshold: None,
                severity: None,
            });
            let from_file = config_file.and_then(|file| file.content.rules.get(&rule_id));
            let resolved = RuleConfig {
                enabled: from_file.and_then(|config| config.enabled).or(base.enabled),
                threshold: from_file
                    .and_then(|config| config.threshold)
                    .or(base.threshold),
                severity: from_file
                    .and_then(|config| config.severity)
                    .or(base.severity),
            };
            validate_rule_config(&rule_id, &resolved)?;
            rules.insert(rule_id, resolved);
        }

        let mut plugin_manifest = ResolvedPluginManifest::default();
        if let Some(file) = config_file {
            for plugin in &file.content.plugins {
                validate_sha256(&plugin.path, &plugin.sha256)?;
                plugin_manifest.modules.push(PluginModuleRef {
                    workspace_relative_path: normalize_relative_path(
                        &workspace_root,
                        &plugin.path,
                        "plugin",
                    )?,
                    sha256: plugin.sha256.clone(),
                });
            }
        }
        plugin_manifest.modules.sort_by(|left, right| {
            left.workspace_relative_path
                .cmp(&right.workspace_relative_path)
        });

        Ok(Self {
            workspace_root,
            analysis_targets,
            rules,
            exclude_patterns,
            score_weights,
            plugin_manifest,
            targets_explicitly_specified: options.targets_explicitly_specified,
        })
    }
}

pub fn render_default_config() -> String {
    let mut output = String::new();

    output.push_str("# kalos configuration\n");
    output.push_str(
        "# All rules are enabled by default. Uncomment rule blocks to override them.\n\n",
    );
    output.push_str("[general]\n");
    output.push_str("# exclude = [\"vendor/**\", \"generated/**\"]\n\n");
    output.push_str("# Metric rule thresholds (defaults shown as comments)\n");

    for (rule_id, threshold) in DEFAULT_METRIC_RULE_THRESHOLDS {
        let _ = writeln!(output, "# [rules.{rule_id}]");
        output.push_str("# enabled = true\n");
        let _ = writeln!(output, "# threshold = {threshold:.2}");
        output.push_str("# severity = \"warning\"\n\n");
    }

    output.push_str("# Pattern rule overrides\n");
    for (rule_id, severity) in DEFAULT_PATTERN_RULE_SEVERITIES {
        let _ = writeln!(output, "# [rules.{rule_id}]");
        output.push_str("# enabled = true\n");
        let _ = writeln!(output, "# severity = \"{}\"\n", severity.as_str());
        output.push_str("# threshold = \"n/a\"\n\n");
    }

    output.push_str("[score.weights]\n");
    output.push_str("function = 0.4\n");
    output.push_str("module = 0.35\n");
    output.push_str("project = 0.25\n\n");
    output.push_str("# [[plugins]]\n");
    output.push_str("# path = \".kalos/plugins/example.wasm\"\n");
    output.push_str(
        "# sha256 = \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"\n",
    );

    output
}

fn parse_severity(rule_id: &RuleId, value: String) -> Result<Severity, ConfigError> {
    match value.as_str() {
        "error" => Ok(Severity::Error),
        "warning" => Ok(Severity::Warning),
        "info" => Ok(Severity::Info),
        _ => Err(ConfigError::InvalidSeverity {
            rule_id: rule_id.clone(),
            value,
        }),
    }
}

fn validate_score_weights(weights: &ScoreWeights) -> Result<(), ConfigError> {
    for (name, value) in [
        ("function", weights.function),
        ("module", weights.module),
        ("project", weights.project),
    ] {
        if !value.is_finite() || value <= 0.0 {
            return Err(ConfigError::InvalidScoreWeight { name, value });
        }
    }

    Ok(())
}

fn validate_rule_config(rule_id: &RuleId, config: &RuleConfig) -> Result<(), ConfigError> {
    if let Some(threshold) = config.threshold {
        if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
            return Err(ConfigError::InvalidThreshold {
                rule_id: rule_id.clone(),
                value: threshold,
            });
        }
    }

    Ok(())
}

fn validate_sha256(path: &Path, value: &str) -> Result<(), ConfigError> {
    let is_valid = value.len() == 64 && value.chars().all(|ch| ch.is_ascii_hexdigit());
    if is_valid {
        Ok(())
    } else {
        Err(ConfigError::InvalidSha256 {
            path: path.display().to_string(),
            value: value.to_owned(),
        })
    }
}

fn find_upward(start: &Path, needle: &str) -> Option<PathBuf> {
    let mut current = canonicalize_workspace_root(start).ok()?;

    loop {
        let candidate = current.join(needle);
        if candidate.exists() {
            return Some(candidate);
        }

        let parent = current.parent()?.to_path_buf();
        if parent == current {
            return None;
        }
        current = parent;
    }
}

fn canonicalize_workspace_root(path: &Path) -> Result<PathBuf, ConfigError> {
    fs::canonicalize(path).map_err(|source| ConfigError::CanonicalizeWorkspaceRoot {
        path: path.to_path_buf(),
        source,
    })
}

fn canonicalize_if_exists(path: &Path) -> Result<PathBuf, ConfigError> {
    if path.exists() {
        fs::canonicalize(path).map_err(|source| ConfigError::ReadConfig {
            path: path.to_path_buf(),
            source,
        })
    } else {
        Ok(path.to_path_buf())
    }
}

fn absolute_from_base(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize_absolute_path(path)
    } else {
        normalize_absolute_path(&base.join(path))
    }
}

fn normalize_relative_path(
    workspace_root: &WorkspaceRoot,
    input: &Path,
    kind: &'static str,
) -> Result<FilePath, ConfigError> {
    let absolute = absolute_from_base(workspace_root.as_path(), input);
    if !absolute.starts_with(workspace_root.as_path()) {
        return Err(ConfigError::PathOutsideWorkspace {
            kind,
            path: input.to_path_buf(),
            workspace_root: workspace_root.abs_path.clone(),
        });
    }

    let relative = absolute
        .strip_prefix(workspace_root.as_path())
        .expect("path prefix checked")
        .to_path_buf();

    if relative.as_os_str().is_empty() {
        return Ok(FilePath::from("."));
    }

    Ok(FilePath::from(path_to_forward_slashes(&relative)))
}

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    normalized
}
impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct RawConfigFile {
    #[serde(default)]
    general: RawGeneralSection,
    #[serde(default)]
    rules: BTreeMap<String, RawRuleConfig>,
    #[serde(default)]
    score: RawScoreSection,
    #[serde(default)]
    plugins: Vec<RawPluginConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct RawGeneralSection {
    #[serde(default)]
    exclude: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawRuleConfig {
    enabled: Option<bool>,
    threshold: Option<f64>,
    severity: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawScoreSection {
    weights: Option<RawScoreWeights>,
}

#[derive(Debug, Default, Deserialize)]
struct RawScoreWeights {
    function: Option<f64>,
    module: Option<f64>,
    project: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct RawPluginConfig {
    path: PathBuf,
    sha256: String,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::{
        ConfigError, ConfigFile, Defaults, GlobPattern, ProjectConfig, ResolveOptions, RuleConfig,
        ScoreWeightsOverride, WorkspaceRoot,
    };
    use crate::domains::metrics::ScoreWeights;
    use crate::domains::{FilePath, RuleId, Severity};

    #[test]
    fn workspace_root_uses_explicit_config_parent() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("repo");
        let nested = workspace.join("services/api");
        fs::create_dir_all(&nested).unwrap();
        let config_path = nested.join(".kalos.toml");
        fs::write(&config_path, "").unwrap();

        let options = ResolveOptions {
            cwd: nested.clone(),
            config_path: Some(config_path.clone()),
            analysis_targets: vec![PathBuf::from(".")],
            targets_explicitly_specified: false,
            exclude_patterns: Vec::new(),
        };

        let discovery = ProjectConfig::discover_workspace(&options).unwrap();
        assert_eq!(
            discovery.workspace_root.abs_path,
            fs::canonicalize(&nested).unwrap()
        );
        assert_eq!(
            discovery.config_path,
            Some(fs::canonicalize(&config_path).unwrap())
        );
    }

    #[test]
    fn workspace_root_discovers_nearest_kalos_file() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("repo");
        let nested = workspace.join("src/bin");
        fs::create_dir_all(&nested).unwrap();
        fs::write(workspace.join(".kalos.toml"), "").unwrap();

        let options = ResolveOptions {
            cwd: nested,
            config_path: None,
            analysis_targets: vec![PathBuf::from(".")],
            targets_explicitly_specified: false,
            exclude_patterns: Vec::new(),
        };

        let discovery = ProjectConfig::discover_workspace(&options).unwrap();
        assert_eq!(
            discovery.workspace_root.abs_path,
            fs::canonicalize(&workspace).unwrap()
        );
    }

    #[test]
    fn workspace_root_falls_back_to_git_directory() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("repo");
        let nested = workspace.join("src/bin");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir(workspace.join(".git")).unwrap();

        let options = ResolveOptions {
            cwd: nested,
            config_path: None,
            analysis_targets: vec![PathBuf::from(".")],
            targets_explicitly_specified: false,
            exclude_patterns: Vec::new(),
        };

        let discovery = ProjectConfig::discover_workspace(&options).unwrap();
        assert_eq!(
            discovery.workspace_root.abs_path,
            fs::canonicalize(&workspace).unwrap()
        );
    }

    #[test]
    fn workspace_root_falls_back_to_current_directory() {
        let temp = TempDir::new().unwrap();
        let cwd = temp.path().join("standalone");
        fs::create_dir_all(&cwd).unwrap();

        let options = ResolveOptions {
            cwd: cwd.clone(),
            config_path: None,
            analysis_targets: vec![PathBuf::from(".")],
            targets_explicitly_specified: false,
            exclude_patterns: Vec::new(),
        };

        let discovery = ProjectConfig::discover_workspace(&options).unwrap();
        assert_eq!(
            discovery.workspace_root.abs_path,
            fs::canonicalize(&cwd).unwrap()
        );
    }

    #[test]
    fn config_file_parsing_and_resolution_merge_expected_values() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("repo");
        fs::create_dir_all(workspace.join("src")).unwrap();
        let config_path = workspace.join(".kalos.toml");
        fs::write(
            &config_path,
            r#"
[general]
exclude = ["vendor/**", "generated/**"]

[rules.KAL-F001]
enabled = false
severity = "warning"
threshold = 0.60

[score.weights]
function = 0.5
module = 0.3
project = 0.2

[[plugins]]
path = ".kalos/plugins/halstead.wasm"
sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
"#,
        )
        .unwrap();

        let config_file = ConfigFile::load(&config_path).unwrap();
        let options = ResolveOptions {
            cwd: workspace.clone(),
            config_path: Some(config_path),
            analysis_targets: vec![PathBuf::from("src")],
            targets_explicitly_specified: true,
            exclude_patterns: vec!["cli/**".to_owned()],
        };

        let project =
            ProjectConfig::resolve(&options, Some(&config_file), &Defaults::default()).unwrap();

        assert_eq!(project.analysis_targets, vec![FilePath::from("src")]);
        assert!(project.targets_explicitly_specified);
        assert_eq!(
            project.exclude_patterns,
            vec![
                GlobPattern {
                    pattern: "vendor/**".to_owned()
                },
                GlobPattern {
                    pattern: "generated/**".to_owned()
                },
                GlobPattern {
                    pattern: "cli/**".to_owned()
                }
            ]
        );
        assert_eq!(
            project.score_weights,
            ScoreWeights {
                function: 0.5,
                module: 0.3,
                project: 0.2,
            }
        );
        assert_eq!(
            project.rules.get(&RuleId::from("KAL-F001")).unwrap(),
            &RuleConfig {
                enabled: Some(false),
                threshold: Some(0.60),
                severity: Some(Severity::Warning),
            }
        );
        assert_eq!(
            project.plugin_manifest.modules[0].workspace_relative_path,
            FilePath::from(".kalos/plugins/halstead.wasm")
        );
    }

    #[test]
    fn merge_priority_is_file_over_defaults_and_cli_adds_excludes() {
        let temp = TempDir::new().unwrap();
        let workspace = fs::canonicalize(temp.path()).unwrap();
        let config_file = ConfigFile {
            path: workspace.join(".kalos.toml"),
            content: super::ConfigContent {
                exclude_patterns: vec!["file/**".to_owned()],
                rules: [(
                    RuleId::from("KAL-F001"),
                    RuleConfig {
                        enabled: Some(false),
                        threshold: Some(0.80),
                        severity: Some(Severity::Info),
                    },
                )]
                .into_iter()
                .collect(),
                score_weights: ScoreWeightsOverride {
                    function: Some(0.8),
                    module: None,
                    project: None,
                },
                plugins: Vec::new(),
            },
        };
        let options = ResolveOptions {
            cwd: workspace.clone(),
            config_path: Some(config_file.path.clone()),
            analysis_targets: Vec::new(),
            targets_explicitly_specified: false,
            exclude_patterns: vec!["cli/**".to_owned()],
        };

        let project =
            ProjectConfig::resolve(&options, Some(&config_file), &Defaults::default()).unwrap();

        assert_eq!(project.analysis_targets, vec![FilePath::from(".")]);
        assert!(!project.targets_explicitly_specified);
        assert_eq!(project.score_weights.function, 0.8);
        assert_eq!(project.score_weights.module, 0.35);
        assert_eq!(
            project.rules.get(&RuleId::from("KAL-F001")).unwrap(),
            &RuleConfig {
                enabled: Some(false),
                threshold: Some(0.80),
                severity: Some(Severity::Info),
            }
        );
        assert_eq!(
            project
                .exclude_patterns
                .iter()
                .map(|pattern| pattern.pattern.as_str())
                .collect::<Vec<_>>(),
            vec!["file/**", "cli/**"]
        );
    }

    #[test]
    fn analysis_targets_are_normalized_and_must_stay_within_workspace() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("repo");
        fs::create_dir_all(workspace.join("src/inner")).unwrap();
        let options = ResolveOptions {
            cwd: workspace.clone(),
            config_path: None,
            analysis_targets: vec![PathBuf::from("src/inner/../lib.rs"), PathBuf::from(".")],
            targets_explicitly_specified: true,
            exclude_patterns: Vec::new(),
        };

        let project = ProjectConfig::resolve(&options, None, &Defaults::default()).unwrap();
        assert_eq!(
            project.analysis_targets,
            vec![FilePath::from("src/lib.rs"), FilePath::from(".")]
        );

        let escaping = ResolveOptions {
            analysis_targets: vec![PathBuf::from("../outside")],
            ..options
        };

        let error = ProjectConfig::resolve(&escaping, None, &Defaults::default()).unwrap_err();
        assert!(matches!(error, ConfigError::PathOutsideWorkspace { .. }));
    }

    #[test]
    fn validation_rejects_invalid_score_weights() {
        let temp = TempDir::new().unwrap();
        let workspace = fs::canonicalize(temp.path()).unwrap();

        for value in [-1.0, 0.0, f64::NAN, f64::INFINITY] {
            let config_file = ConfigFile {
                path: workspace.join(".kalos.toml"),
                content: super::ConfigContent {
                    exclude_patterns: Vec::new(),
                    rules: BTreeMap::new(),
                    score_weights: ScoreWeightsOverride {
                        function: Some(value),
                        module: None,
                        project: None,
                    },
                    plugins: Vec::new(),
                },
            };
            let options = ResolveOptions {
                cwd: workspace.clone(),
                config_path: Some(config_file.path.clone()),
                analysis_targets: vec![PathBuf::from(".")],
                targets_explicitly_specified: false,
                exclude_patterns: Vec::new(),
            };

            let error = ProjectConfig::resolve(&options, Some(&config_file), &Defaults::default())
                .unwrap_err();
            assert!(matches!(error, ConfigError::InvalidScoreWeight { .. }));
        }
    }

    #[test]
    fn validation_rejects_invalid_threshold() {
        let temp = TempDir::new().unwrap();
        let workspace = fs::canonicalize(temp.path()).unwrap();

        for value in [-0.1, 1.5] {
            let config_file = ConfigFile {
                path: workspace.join(".kalos.toml"),
                content: super::ConfigContent {
                    exclude_patterns: Vec::new(),
                    rules: [(
                        RuleId::from("KAL-F001"),
                        RuleConfig {
                            enabled: Some(true),
                            threshold: Some(value),
                            severity: None,
                        },
                    )]
                    .into_iter()
                    .collect(),
                    score_weights: ScoreWeightsOverride::default(),
                    plugins: Vec::new(),
                },
            };
            let options = ResolveOptions {
                cwd: workspace.clone(),
                config_path: Some(config_file.path.clone()),
                analysis_targets: vec![PathBuf::from(".")],
                targets_explicitly_specified: false,
                exclude_patterns: Vec::new(),
            };

            let error = ProjectConfig::resolve(&options, Some(&config_file), &Defaults::default())
                .unwrap_err();
            assert!(matches!(error, ConfigError::InvalidThreshold { .. }));
        }
    }

    #[test]
    fn validation_rejects_invalid_severity() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".kalos.toml");
        let error = ConfigFile::parse(
            &path,
            r#"
[rules.KAL-F001]
severity = "fatal"
"#,
        )
        .unwrap_err();

        assert!(matches!(error, ConfigError::InvalidSeverity { .. }));
    }

    #[test]
    fn validation_rejects_invalid_sha256() {
        let temp = TempDir::new().unwrap();
        let workspace = fs::canonicalize(temp.path()).unwrap();
        let config_file = ConfigFile {
            path: workspace.join(".kalos.toml"),
            content: super::ConfigContent {
                exclude_patterns: Vec::new(),
                rules: BTreeMap::new(),
                score_weights: ScoreWeightsOverride::default(),
                plugins: vec![super::PluginConfig {
                    path: PathBuf::from(".kalos/plugins/example.wasm"),
                    sha256: "abc".to_owned(),
                }],
            },
        };
        let options = ResolveOptions {
            cwd: workspace,
            config_path: Some(config_file.path.clone()),
            analysis_targets: vec![PathBuf::from(".")],
            targets_explicitly_specified: false,
            exclude_patterns: Vec::new(),
        };

        let error =
            ProjectConfig::resolve(&options, Some(&config_file), &Defaults::default()).unwrap_err();
        assert!(matches!(error, ConfigError::InvalidSha256 { .. }));
    }

    #[test]
    fn render_default_config_lists_known_rules() {
        let rendered = super::render_default_config();
        assert!(rendered.contains("# [rules.KAL-F001]"));
        assert!(rendered.contains("# threshold = 0.55"));
        assert!(rendered.contains("# [rules.KAL-PAT003]"));
        assert!(rendered.contains("project = 0.25"));
    }

    #[test]
    fn normalize_relative_path_uses_workspace_relative_dot() {
        let temp = TempDir::new().unwrap();
        let workspace = WorkspaceRoot {
            abs_path: fs::canonicalize(temp.path()).unwrap(),
        };

        let normalized =
            super::normalize_relative_path(&workspace, Path::new("."), "analysis target").unwrap();
        assert_eq!(normalized, FilePath::from("."));
    }
}
