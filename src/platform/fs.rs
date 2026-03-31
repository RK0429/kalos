use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

pub trait FileSystem: Send + Sync {
    fn read_dir_recursive(
        &self,
        root: &Path,
        extensions: &[&str],
    ) -> Result<Vec<PathBuf>, io::Error>;
    fn read_to_string(&self, path: &Path) -> Result<String, io::Error>;
    fn create_dir_all(&self, path: &Path) -> Result<(), io::Error>;
}

pub fn path_to_forward_slashes(path: &Path) -> String {
    let parts = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();

    if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    }
}

#[derive(Clone, Debug, Default)]
pub struct RealFileSystem;

impl FileSystem for RealFileSystem {
    fn read_dir_recursive(
        &self,
        root: &Path,
        extensions: &[&str],
    ) -> Result<Vec<PathBuf>, io::Error> {
        let metadata = fs::metadata(root)?;
        let mut paths = Vec::new();

        if metadata.is_file() {
            if matches_extension(root, extensions) {
                paths.push(root.to_path_buf());
            }
            return Ok(paths);
        }

        collect_dir_entries(root, extensions, &mut paths)?;
        paths.sort();
        Ok(paths)
    }

    fn read_to_string(&self, path: &Path) -> Result<String, io::Error> {
        fs::read_to_string(path)
    }

    fn create_dir_all(&self, path: &Path) -> Result<(), io::Error> {
        fs::create_dir_all(path)
    }
}

fn collect_dir_entries(
    root: &Path,
    extensions: &[&str],
    paths: &mut Vec<PathBuf>,
) -> Result<(), io::Error> {
    let mut entries = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_dir_entries(&path, extensions, paths)?;
        } else if file_type.is_file() && matches_extension(&path, extensions) {
            paths.push(path);
        }
    }

    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct InMemoryFileSystem {
    files: BTreeMap<PathBuf, String>,
    created_dirs: Arc<Mutex<Vec<PathBuf>>>,
}

impl InMemoryFileSystem {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, path: impl Into<PathBuf>, contents: impl Into<String>) {
        self.files.insert(path.into(), contents.into());
    }

    pub fn created_dirs(&self) -> Vec<PathBuf> {
        self.created_dirs
            .lock()
            .expect("in-memory file system state should be available")
            .clone()
    }
}

impl FileSystem for InMemoryFileSystem {
    fn read_dir_recursive(
        &self,
        root: &Path,
        extensions: &[&str],
    ) -> Result<Vec<PathBuf>, io::Error> {
        let mut paths = self
            .files
            .keys()
            .filter(|path| is_under_root(path, root))
            .filter(|path| matches_extension(path, extensions))
            .cloned()
            .collect::<Vec<_>>();

        if paths.is_empty()
            && !self.files.contains_key(root)
            && !contains_descendant(&self.files, root)
        {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("path `{}` does not exist", root.display()),
            ));
        }

        paths.sort();
        Ok(paths)
    }

    fn read_to_string(&self, path: &Path) -> Result<String, io::Error> {
        self.files.get(path).cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("path `{}` does not exist", path.display()),
            )
        })
    }

    fn create_dir_all(&self, path: &Path) -> Result<(), io::Error> {
        let mut created_dirs = self
            .created_dirs
            .lock()
            .map_err(|_| io::Error::other("in-memory file system state should be available"))?;
        created_dirs.push(path.to_path_buf());
        Ok(())
    }
}

fn contains_descendant(files: &BTreeMap<PathBuf, String>, root: &Path) -> bool {
    files.keys().any(|path| path.starts_with(root))
}

fn is_under_root(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn matches_extension(path: &Path, extensions: &[&str]) -> bool {
    let Some(actual_extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };

    extensions
        .iter()
        .map(|extension| extension.trim_start_matches('.'))
        .any(|extension| actual_extension == extension)
}

#[cfg(test)]
mod tests {
    use super::{FileSystem, InMemoryFileSystem};

    #[test]
    fn in_memory_file_system_filters_by_extension() {
        let mut fs = InMemoryFileSystem::new();
        fs.insert("/workspace/src/lib.rs", "fn main() {}");
        fs.insert("/workspace/src/lib.txt", "ignore");

        let files = fs
            .read_dir_recursive("/workspace".as_ref(), &[".rs"])
            .unwrap();

        assert_eq!(
            files,
            vec![std::path::PathBuf::from("/workspace/src/lib.rs")]
        );
    }

    #[test]
    fn in_memory_file_system_excludes_misleading_suffixes() {
        let mut fs = InMemoryFileSystem::new();
        fs.insert("/workspace/src/main.py", "print('ok')");
        fs.insert("/workspace/src/backup.spy", "print('skip')");
        fs.insert("/workspace/src/not_a_test.gors", "package skip");

        let files = fs
            .read_dir_recursive("/workspace".as_ref(), &[".py", ".go"])
            .unwrap();

        assert_eq!(
            files,
            vec![std::path::PathBuf::from("/workspace/src/main.py")]
        );
    }

    #[test]
    fn in_memory_file_system_reads_file_contents() {
        let mut fs = InMemoryFileSystem::new();
        fs.insert("/workspace/.gitignore", "target/");

        let contents = fs.read_to_string("/workspace/.gitignore".as_ref()).unwrap();

        assert_eq!(contents, "target/");
    }

    #[test]
    fn in_memory_file_system_records_created_directories() {
        let fs = InMemoryFileSystem::new();

        fs.create_dir_all("/workspace/.kalos/codeql".as_ref())
            .unwrap();

        assert_eq!(
            fs.created_dirs(),
            vec![std::path::PathBuf::from("/workspace/.kalos/codeql")]
        );
    }
}
