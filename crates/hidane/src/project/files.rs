//! Where a scan reads a project from: a directory on disk ([`Dir`]) or files held in memory
//! ([`Memory`]), such as a repository snapshot a service fetched. Paths are relative to the
//! project's root and separated by `/`.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// Files larger than this are not read.
pub const MAX_FILE: u64 = 8 * 1024 * 1024;

/// A project's files, read-only.
pub trait Tree {
    /// The text of the file at `path`, or `None` when there is none, or it is not UTF-8 text,
    /// or it is larger than [`MAX_FILE`].
    fn read(&self, path: &str) -> Option<String>;
    /// The entries of the directory at `path` (`""` is the root), sorted by name.
    fn list(&self, path: &str) -> Vec<Entry>;
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

/// A directory on disk. Symbolic links are neither listed nor followed.
pub struct Dir {
    root: PathBuf,
}

impl Dir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The path on disk of a project path; `None` for one that leaves the root or goes
    /// through a symbolic link.
    pub fn path(&self, path: &str) -> Option<PathBuf> {
        let normal = normalize(path)?;
        let mut full = self.root.clone();
        for part in normal.split('/').filter(|p| !p.is_empty()) {
            full.push(part);
            if full.symlink_metadata().ok()?.file_type().is_symlink() {
                return None;
            }
        }
        Some(full)
    }
}

impl Tree for Dir {
    fn read(&self, path: &str) -> Option<String> {
        let full = self.path(path)?;
        let meta = full.metadata().ok()?;
        if !meta.is_file() || meta.len() > MAX_FILE {
            return None;
        }
        String::from_utf8(std::fs::read(full).ok()?).ok()
    }

    fn list(&self, path: &str) -> Vec<Entry> {
        let Some(full) = self.path(path) else {
            return Vec::new();
        };
        let Ok(dir) = std::fs::read_dir(full) else {
            return Vec::new();
        };
        let mut entries: Vec<Entry> = dir
            .filter_map(Result::ok)
            .filter_map(|e| {
                let kind = e.file_type().ok()?;
                if kind.is_symlink() {
                    return None;
                }
                Some(Entry {
                    name: e.file_name().into_string().ok()?,
                    is_dir: kind.is_dir(),
                })
            })
            .collect();
        entries.sort();
        entries
    }
}

/// Files in memory, by path.
#[derive(Debug, Clone, Default)]
pub struct Memory {
    files: BTreeMap<String, String>,
}

impl Memory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, path: &str, text: impl Into<String>) {
        if let Some(path) = normalize(path) {
            self.files.insert(path, text.into());
        }
    }
}

impl<P: AsRef<str>, T: Into<String>> FromIterator<(P, T)> for Memory {
    fn from_iter<I: IntoIterator<Item = (P, T)>>(iter: I) -> Self {
        let mut memory = Self::new();
        for (path, text) in iter {
            memory.insert(path.as_ref(), text);
        }
        memory
    }
}

impl Tree for Memory {
    fn read(&self, path: &str) -> Option<String> {
        self.files.get(&normalize(path)?).cloned()
    }

    fn list(&self, path: &str) -> Vec<Entry> {
        let Some(dir) = normalize(path) else {
            return Vec::new();
        };
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let mut entries = BTreeSet::new();
        for file in self.files.keys() {
            if let Some(rest) = file.strip_prefix(&prefix) {
                let entry = match rest.split_once('/') {
                    Some((name, _)) => Entry {
                        name: name.to_owned(),
                        is_dir: true,
                    },
                    None => Entry {
                        name: rest.to_owned(),
                        is_dir: false,
                    },
                };
                entries.insert(entry);
            }
        }
        entries.into_iter().collect()
    }
}

/// `a/./b/../c` as `a/c`; `None` for an absolute path or one that leaves the root.
pub fn normalize(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    if path.starts_with(['/', '\\']) || bytes.get(1) == Some(&b':') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// `dir` joined with the relative `path`, normalized.
pub fn join(dir: &str, path: &str) -> Option<String> {
    if dir.is_empty() {
        normalize(path)
    } else {
        normalize(&format!("{dir}/{path}"))
    }
}

/// The directory part of `path` (`""` for a file at the root).
pub fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The last part of `path`.
pub fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// Directories a scan does not descend into: dependencies, build output, tool state.
pub fn skipped(name: &str) -> bool {
    matches!(
        name,
        "node_modules"
            | "target"
            | "dist"
            | "build"
            | "out"
            | "coverage"
            | "vendor"
            | "venv"
            | "__pycache__"
            | "Pods"
    ) || name.starts_with('.') && name != ".github"
}

/// Every file under the root at most `depth` directories deep, outside [`skipped`]
/// directories, `limit` of them at most, sorted.
pub fn walk(tree: &dyn Tree, root: &str, depth: usize, limit: usize) -> Vec<String> {
    let mut files = Vec::new();
    let mut dirs = vec![(root.to_owned(), 0)];
    while let Some((dir, level)) = dirs.pop() {
        for entry in tree.list(&dir) {
            let path = if dir.is_empty() {
                entry.name.clone()
            } else {
                format!("{dir}/{}", entry.name)
            };
            if entry.is_dir {
                if level < depth && !skipped(&entry.name) {
                    dirs.push((path, level + 1));
                }
            } else if files.len() < limit {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(normalize("a/./b/../c").as_deref(), Some("a/c"));
        assert_eq!(normalize("./package.json").as_deref(), Some("package.json"));
        assert_eq!(normalize("../x"), None);
        assert_eq!(normalize("/etc/passwd"), None);
        assert_eq!(normalize("C:/x"), None);
        assert_eq!(
            join("functions", "../firebase.json").as_deref(),
            Some("firebase.json")
        );
        assert_eq!(parent("a/b/c.json"), "a/b");
        assert_eq!(parent("c.json"), "");
        assert_eq!(file_name("a/b/c.json"), "c.json");
    }

    #[test]
    fn memory_lists_like_a_directory() {
        let tree: Memory = [
            ("package.json", "{}"),
            ("functions/package.json", "{}"),
            ("functions/node_modules/x/package.json", "{}"),
            (".github/workflows/ci.yml", ""),
            (".git/config", ""),
        ]
        .into_iter()
        .collect();
        let names: Vec<_> = tree.list("").into_iter().map(|e| e.name).collect();
        assert_eq!(names, [".git", ".github", "functions", "package.json"]);
        assert_eq!(
            walk(&tree, "", 4, 100),
            [
                ".github/workflows/ci.yml",
                "functions/package.json",
                "package.json"
            ]
        );
    }
}
