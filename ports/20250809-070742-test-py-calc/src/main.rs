use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{self, Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// Configuration for the gitingest tool.
struct Config {
    repo_slug: String,
    token: Option<String>,
    max_size_kb: u64,
    include_patterns: Vec<String>,
    exclude_patterns: Vec<String>,
}

/// Represents a node in the directory tree.
enum Node {
    File,
    Dir(BTreeMap<String, Node>),
}

/// Checks if a given path string matches a single pattern.
///
/// Supports simple glob-like patterns:
/// - dir/: Matches any path inside the dir directory.
/// - *.ext: Matches any file with the .ext extension.
/// - name: Matches any file or directory with the exact name name.
fn matches_one_pattern(path_str: &str, pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }

    if let Some(p) = pattern.strip_suffix('/') {
        return path_str.starts_with(&format!("{}/", p)) || path_str == p;
    }

    if let Some(p) = pattern.strip_prefix('*') {
        return path_str.ends_with(p);
    }

    Path::new(path_str)
        .iter()
        .any(|comp| comp.to_str() == Some(pattern))
}

/// Checks if a path matches any of the provided patterns.
fn check_patterns(path_str: &str, patterns: &[String]) -> bool {
    !patterns.is_empty() && patterns.iter().any(|p| matches_one_pattern(path_str, p))
}

/// A simple heuristic to detect if a file is binary.
/// It checks for the presence of a NUL byte in the first 1024 bytes.
fn is_binary(contents: &[u8]) -> bool {
    const CHECK_LEN: usize = 1024;
    let len = std::cmp::min(contents.len(), CHECK_LEN);
    contents[..len].contains(&0)
}

/// Builds a tree structure from a flat list of file paths.
fn build_tree(paths: &[PathBuf], repo_root: &Path) -> BTreeMap<String, Node> {
    let mut root = BTreeMap::new();
    for path in paths {
        if let Ok(relative_path) = path.strip_prefix(repo_root) {
            let mut current_level = &mut root;
            let components: Vec<_> = relative_path
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();

            for (i, component) in components.iter().enumerate() {
                if i == components.len() - 1 {
                    current_level.insert(component.clone(), Node::File);
                } else {
                    let entry = current_level
                        .entry(component.clone())
                        .or_insert_with(|| Node::Dir(BTreeMap::new()));
                    if let Node::Dir(subdir) = entry {
                        current_level = subdir;
                    }
                }
            }
        }
    }
    root
}

/// Recursively formats the directory tree for printing.
fn format_tree_recursive(tree: &BTreeMap<String, Node>, prefix: &str, output: &mut String) {
    let mut entries = tree.iter().peekable();
    while let Some((name, node)) = entries.next() {
        let is_last = entries.peek().is_none();
        let connector = if is_last { "└── " } else { "├── " };
        let new_prefix = if is_last { "    " } else { "│   " };

        writeln!(output, "{}{}{}", prefix, connector, name).unwrap();
        if let Node::Dir(subdir) = node {
            format_tree_recursive(subdir, &format!("{}{}", prefix, new_prefix), output);
        }
    }
}

/// Generates the complete directory tree string for the output.
fn generate_tree_output(paths: &[PathBuf], repo_root: &Path) -> String {
    let repo_name = repo_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(".");
    let tree = build_tree(paths, repo_root);
    let mut output = String::new();
    writeln!(output, "{}/", repo_name).unwrap();
    format_tree_recursive(&tree, "", &mut output);
    output
}

/// Processes the cloned repository: filters files, generates tree, and concatenates content.
fn process_repository(
    repo_path: &Path,
    max_size_kb: u64,
    include_patterns: &[String],
    exclude_patterns: &[String],
) -> io::Result<()> {
    let mut collected_paths = Vec::new();
    let mut dirs_to_visit = vec
