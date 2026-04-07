use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use crate::domains::FilePath;
use crate::domains::cpg::{Language, SourceFile};
use crate::platform::fs::{FileSystem, path_to_forward_slashes};

/// Well-known build and artifact directories excluded by default.
/// These are always excluded regardless of `.gitignore` or `--exclude` settings.
const DEFAULT_EXCLUDE_PATTERNS: &[&str] = &[
    "target/**",
    "node_modules/**",
    "__pycache__/**",
    ".venv/**",
    ".git/**",
    ".kalos/**",
];

#[derive(Debug)]
pub struct FileCollector<'a, F> {
    file_system: &'a F,
    workspace_root: PathBuf,
    extension_filters: Vec<String>,
    exclude_rules: Vec<String>,
}

impl<'a, F> FileCollector<'a, F>
where
    F: FileSystem,
{
    pub fn new(
        file_system: &'a F,
        workspace_root: impl Into<PathBuf>,
        extension_filters: &[&str],
        exclude_rules: &[String],
    ) -> Self {
        Self {
            file_system,
            workspace_root: workspace_root.into(),
            extension_filters: extension_filters
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            exclude_rules: exclude_rules.to_vec(),
        }
    }

    pub fn collect(
        &self,
        analysis_targets: &[FilePath],
    ) -> Result<BTreeMap<FilePath, SourceFile>, io::Error> {
        let matcher = ExcludeMatcher::new(self.merged_exclude_rules()?);
        let mut files = BTreeMap::new();
        let extension_filters = self
            .extension_filters
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let targets = if analysis_targets.is_empty() {
            vec![FilePath::from(".")]
        } else {
            analysis_targets.to_vec()
        };

        for target in targets {
            let target_path = target_path(&self.workspace_root, &target);
            for candidate in self
                .file_system
                .read_dir_recursive(&target_path, &extension_filters)?
            {
                let relative_path =
                    normalize_workspace_relative_path(&self.workspace_root, &candidate)?;
                if matcher.matches(relative_path.as_str()) {
                    continue;
                }
                if let Some(language) = detect_language(&candidate) {
                    let file_path = FilePath::from(relative_path.clone());
                    files.entry(file_path.clone()).or_insert(SourceFile {
                        path: file_path,
                        language,
                    });
                }
            }
        }

        Ok(files)
    }

    fn merged_exclude_rules(&self) -> Result<Vec<String>, io::Error> {
        let mut merged = BTreeSet::new();
        for pattern in DEFAULT_EXCLUDE_PATTERNS {
            merged.insert((*pattern).to_owned());
        }
        if let Some(gitignore) = self.load_gitignore()? {
            for pattern in parse_ignore_file(&gitignore) {
                merged.insert(pattern);
            }
        }
        for pattern in &self.exclude_rules {
            if let Some(pattern) = normalize_pattern(pattern) {
                merged.insert(pattern);
            }
        }
        Ok(merged.into_iter().collect())
    }

    fn load_gitignore(&self) -> Result<Option<String>, io::Error> {
        let path = self.workspace_root.join(".gitignore");
        match self.file_system.read_to_string(&path) {
            Ok(contents) => Ok(Some(contents)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

fn target_path(workspace_root: &Path, target: &FilePath) -> PathBuf {
    if target.as_str() == "." {
        workspace_root.to_path_buf()
    } else {
        workspace_root.join(target.as_str())
    }
}

fn detect_language(path: &Path) -> Option<Language> {
    match path.extension()?.to_str()? {
        "py" => Some(Language::Python),
        "ts" => Some(Language::TypeScript),
        "tsx" => Some(Language::TypeScript),
        "rs" => Some(Language::Rust),
        "go" => Some(Language::Go),
        _ => None,
    }
}

fn normalize_workspace_relative_path(
    workspace_root: &Path,
    path: &Path,
) -> Result<String, io::Error> {
    let relative = path.strip_prefix(workspace_root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "path `{}` is outside workspace root `{}`",
                path.display(),
                workspace_root.display()
            ),
        )
    })?;

    Ok(path_to_forward_slashes(relative))
}
fn parse_ignore_file(contents: &str) -> Vec<String> {
    contents.lines().filter_map(normalize_pattern).collect()
}

fn normalize_pattern(pattern: &str) -> Option<String> {
    let trimmed = pattern.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
        return None;
    }

    let mut normalized = trimmed.replace('\\', "/");
    while let Some(stripped) = normalized.strip_prefix("./") {
        normalized = stripped.to_owned();
    }
    if let Some(stripped) = normalized.strip_prefix('/') {
        normalized = stripped.to_owned();
    }
    if normalized.ends_with('/') {
        normalized.push_str("**");
    }

    Some(normalized)
}

#[derive(Debug)]
struct ExcludeMatcher {
    patterns: Vec<String>,
}

impl ExcludeMatcher {
    fn new(patterns: Vec<String>) -> Self {
        Self { patterns }
    }

    fn matches(&self, path: &str) -> bool {
        self.patterns
            .iter()
            .any(|pattern| matches_pattern(path, pattern))
    }
}

fn matches_pattern(path: &str, pattern: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }

    let path_segments = split_segments(path);
    let pattern_segments = split_segments(pattern);

    if !pattern.contains('/') {
        return path_segments
            .iter()
            .any(|segment| matches_segment(segment, pattern));
    }

    matches_segments(&path_segments, &pattern_segments)
}

fn split_segments(value: &str) -> Vec<&str> {
    value
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect()
}

fn matches_segments(path: &[&str], pattern: &[&str]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }

    if pattern[0] == "**" {
        return matches_segments(path, &pattern[1..])
            || (!path.is_empty() && matches_segments(&path[1..], pattern));
    }

    !path.is_empty()
        && matches_segment(path[0], pattern[0])
        && matches_segments(&path[1..], &pattern[1..])
}

fn matches_segment(value: &str, pattern: &str) -> bool {
    let value_chars = value.chars().collect::<Vec<_>>();
    let pattern_chars = pattern.chars().collect::<Vec<_>>();
    let mut dp = vec![vec![false; value_chars.len() + 1]; pattern_chars.len() + 1];
    dp[0][0] = true;

    for index in 0..pattern_chars.len() {
        if pattern_chars[index] == '*' {
            dp[index + 1][0] = dp[index][0];
        }
    }

    for (pattern_index, pattern_char) in pattern_chars.iter().enumerate() {
        for (value_index, value_char) in value_chars.iter().enumerate() {
            dp[pattern_index + 1][value_index + 1] = match pattern_char {
                '*' => {
                    dp[pattern_index][value_index + 1]
                        || dp[pattern_index + 1][value_index]
                        || dp[pattern_index][value_index]
                }
                '?' => dp[pattern_index][value_index],
                character => dp[pattern_index][value_index] && character == value_char,
            };
        }
    }

    dp[pattern_chars.len()][value_chars.len()]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use tempfile::{NamedTempFile, TempDir};

    use super::{FileCollector, matches_pattern};
    use crate::domains::FilePath;
    use crate::domains::cpg::{Language, SourceFile};
    use crate::platform::fs::RealFileSystem;

    #[test]
    fn collects_supported_languages_only() {
        let temp = TempDir::new().unwrap();
        let workspace_root = fs::canonicalize(temp.path()).unwrap();
        fs::create_dir_all(workspace_root.join("src")).unwrap();
        fs::create_dir_all(workspace_root.join("web")).unwrap();
        fs::create_dir_all(workspace_root.join("cmd")).unwrap();
        fs::write(
            workspace_root.join("src/main.py"),
            "def main():\n    pass\n",
        )
        .unwrap();
        fs::write(workspace_root.join("web/app.ts"), "export const app = 1;\n").unwrap();
        fs::write(
            workspace_root.join("web/Component.tsx"),
            "export const Component = () => null;\n",
        )
        .unwrap();
        fs::write(workspace_root.join("src/lib.rs"), "fn lib() {}\n").unwrap();
        fs::write(workspace_root.join("cmd/main.go"), "package main\n").unwrap();
        fs::write(workspace_root.join("README.md"), "# ignore\n").unwrap();

        let collector = FileCollector::new(
            &RealFileSystem,
            &workspace_root,
            &[".py", ".ts", ".tsx", ".rs", ".go"],
            &[],
        );

        let files = collector.collect(&[FilePath::from(".")]).unwrap();

        assert_eq!(
            files,
            BTreeMap::from([
                (
                    FilePath::from("cmd/main.go"),
                    SourceFile {
                        path: FilePath::from("cmd/main.go"),
                        language: Language::Go,
                    },
                ),
                (
                    FilePath::from("src/lib.rs"),
                    SourceFile {
                        path: FilePath::from("src/lib.rs"),
                        language: Language::Rust,
                    },
                ),
                (
                    FilePath::from("src/main.py"),
                    SourceFile {
                        path: FilePath::from("src/main.py"),
                        language: Language::Python,
                    },
                ),
                (
                    FilePath::from("web/app.ts"),
                    SourceFile {
                        path: FilePath::from("web/app.ts"),
                        language: Language::TypeScript,
                    },
                ),
                (
                    FilePath::from("web/Component.tsx"),
                    SourceFile {
                        path: FilePath::from("web/Component.tsx"),
                        language: Language::TypeScript,
                    },
                ),
            ])
        );
    }

    #[test]
    fn merges_gitignore_and_explicit_excludes() {
        let temp = TempDir::new().unwrap();
        let workspace_root = fs::canonicalize(temp.path()).unwrap();
        fs::create_dir_all(workspace_root.join("vendor")).unwrap();
        fs::create_dir_all(workspace_root.join("cli")).unwrap();
        fs::create_dir_all(workspace_root.join("src")).unwrap();
        fs::write(workspace_root.join(".gitignore"), "ignored.py\n").unwrap();
        fs::write(workspace_root.join("ignored.py"), "print('skip')\n").unwrap();
        fs::write(workspace_root.join("vendor/lib.rs"), "fn vendor() {}\n").unwrap();
        fs::write(
            workspace_root.join("cli/app.ts"),
            "export const skip = 1;\n",
        )
        .unwrap();
        fs::write(workspace_root.join("src/main.py"), "print('keep')\n").unwrap();

        let collector = FileCollector::new(
            &RealFileSystem,
            &workspace_root,
            &[".py", ".ts", ".tsx", ".rs", ".go"],
            &["vendor/**".to_owned(), "cli/**".to_owned()],
        );

        let files = collector.collect(&[FilePath::from(".")]).unwrap();

        assert_eq!(files.len(), 1);
        assert!(files.contains_key(&FilePath::from("src/main.py")));
    }

    #[test]
    fn excludes_default_build_artifact_directories() {
        let temp = TempDir::new().unwrap();
        let workspace_root = fs::canonicalize(temp.path()).unwrap();
        fs::create_dir_all(workspace_root.join("src")).unwrap();
        fs::create_dir_all(workspace_root.join("target/debug/build/some-crate/out")).unwrap();
        fs::create_dir_all(workspace_root.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(workspace_root.join("__pycache__")).unwrap();
        fs::create_dir_all(workspace_root.join(".venv/bin")).unwrap();
        fs::create_dir_all(workspace_root.join(".git/hooks")).unwrap();
        fs::create_dir_all(workspace_root.join(".kalos/cache")).unwrap();
        fs::write(workspace_root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(
            workspace_root.join("target/debug/build/some-crate/out/generated.rs"),
            "fn generated() {}\n",
        )
        .unwrap();
        fs::write(
            workspace_root.join("node_modules/pkg/index.ts"),
            "export const x = 1;\n",
        )
        .unwrap();
        fs::write(
            workspace_root.join("__pycache__/module.py"),
            "print('skip')\n",
        )
        .unwrap();
        fs::write(
            workspace_root.join(".venv/bin/activate.py"),
            "print('skip')\n",
        )
        .unwrap();
        fs::write(
            workspace_root.join(".git/hooks/pre-commit.py"),
            "print('skip')\n",
        )
        .unwrap();
        fs::write(
            workspace_root.join(".kalos/cache/generated.ts"),
            "export const generated = true;\n",
        )
        .unwrap();

        let collector = FileCollector::new(
            &RealFileSystem,
            &workspace_root,
            &[".py", ".ts", ".rs"],
            &[],
        );

        let files = collector.collect(&[FilePath::from(".")]).unwrap();

        assert_eq!(files.len(), 1);
        assert!(files.contains_key(&FilePath::from("src/main.rs")));
    }

    #[test]
    fn returns_workspace_relative_paths_for_nested_targets() {
        let temp = TempDir::new().unwrap();
        let workspace_root = fs::canonicalize(temp.path()).unwrap();
        fs::create_dir_all(workspace_root.join("services/api/src")).unwrap();
        fs::write(
            workspace_root.join("services/api/src/main.rs"),
            "fn main() {}\n",
        )
        .unwrap();

        let collector = FileCollector::new(
            &RealFileSystem,
            &workspace_root,
            &[".py", ".ts", ".tsx", ".rs", ".go"],
            &[],
        );

        let files = collector
            .collect(&[FilePath::from("services/api/./src")])
            .unwrap();

        assert_eq!(
            files.keys().cloned().collect::<Vec<_>>(),
            vec![FilePath::from("services/api/src/main.rs")]
        );
    }

    #[test]
    fn excludes_files_with_misleading_suffixes() {
        let temp = TempDir::new().unwrap();
        let workspace_root = fs::canonicalize(temp.path()).unwrap();
        fs::create_dir_all(workspace_root.join("src")).unwrap();
        fs::write(workspace_root.join("src/main.py"), "print('keep')\n").unwrap();
        fs::write(workspace_root.join("src/backup.spy"), "print('skip')\n").unwrap();
        fs::write(
            workspace_root.join("src/not_a_test.gors"),
            "package misleading\n",
        )
        .unwrap();

        let collector = FileCollector::new(&RealFileSystem, &workspace_root, &[".py", ".go"], &[]);

        let files = collector.collect(&[FilePath::from(".")]).unwrap();

        assert_eq!(
            files.keys().cloned().collect::<Vec<_>>(),
            vec![FilePath::from("src/main.py")]
        );
    }

    #[test]
    fn gitignore_directory_pattern_excludes_nested_files() {
        let temp = TempDir::new().unwrap();
        let workspace_root = fs::canonicalize(temp.path()).unwrap();
        fs::create_dir_all(workspace_root.join("src")).unwrap();
        fs::create_dir_all(workspace_root.join("tmp/cache")).unwrap();
        fs::write(workspace_root.join(".gitignore"), "tmp/\n").unwrap();
        fs::write(workspace_root.join("src/main.py"), "print('keep')\n").unwrap();
        fs::write(
            workspace_root.join("tmp/scratch.py"),
            "print('exclude')\n",
        )
        .unwrap();
        fs::write(
            workspace_root.join("tmp/cache/data.py"),
            "print('exclude')\n",
        )
        .unwrap();

        let collector = FileCollector::new(
            &RealFileSystem,
            &workspace_root,
            &[".py", ".ts", ".tsx", ".rs", ".go"],
            &[],
        );

        let files = collector.collect(&[FilePath::from(".")]).unwrap();

        assert_eq!(files.len(), 1, "only src/main.py should remain, got: {files:?}");
        assert!(files.contains_key(&FilePath::from("src/main.py")));
    }

    #[test]
    fn exclude_matcher_edge_cases() {
        assert!(!matches_pattern("logs/debug.log", ""));
        assert!(matches_pattern(
            "node_modules/foo/bar.ts",
            "node_modules/**"
        ));
        assert!(matches_pattern("logs/debug.log", "*.log"));

        let temp = NamedTempFile::new().unwrap();
        let exact_name = temp
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(matches_pattern(exact_name.as_str(), exact_name.as_str()));
        assert!(!matches_pattern(
            &format!("other/{}.bak", exact_name),
            exact_name.as_str()
        ));
    }
}
