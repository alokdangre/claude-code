```rust
//! Gitingest - A Rust CLI to create a prompt-friendly digest of a GitHub repository.
//!
//! This tool clones a public or private GitHub repository, filters its files based on
//! size and path patterns, and then prints a digest to standard output. The digest
//! includes a summary, a directory tree, and the concatenated content of the
//! filtered files. It is a local implementation of the functionality provided by
//! the gitingest.com web service.

use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_MAX_SIZE_KB: u64 = 50;

#[derive(Debug, PartialEq, Eq)]
enum PatternType {
    Include,
    Exclude,
}

#[derive(Debug)]
struct Args {
    repo: String,
    token: Option<String>,
    pattern_type: PatternType,
    patterns: Vec<String>,
    max_file_size_kb: u64,
}

/// A temporary directory that is automatically deleted when it goes out of scope.
struct TempDir(PathBuf);

impl TempDir {
    /// Creates a new temporary directory with a unique name.
    fn new() -> io::Result<Self> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?
            .as_millis();
        let dir_name = format!("gitingest-{}", timestamp);
        let path = env::temp_dir().join(dir_name);
        fs::create_dir_all(&path)?;
        Ok(TempDir(path))
    }

    /// Returns the path to the temporary directory.
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Attempt to remove the directory and its contents, ignoring errors.
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn print_usage() {
    eprintln!(
        "Gitingest - A tool to create a prompt-friendly text digest of a GitHub repository.

USAGE:
    gitingest <user/repo> [OPTIONS]

ARGS:
    <user/repo>    The GitHub repository to process (e.g., 'rust-lang/rust').

OPTIONS:
    --include <PATTERNS>   Comma-separated patterns of files/directories to include.
                           (e.g., '*.rs,src/').
    --exclude <PATTERNS>   Comma-separated patterns of files/directories to exclude.
                           (e.g., '*.md,dist/,target/').
                           This is the default mode if no patterns are provided.
    --max-size <KB>        Maximum file size in kilobytes to include (default: {}).
    --token <TOKEN>        GitHub Personal Access Token for private repositories.
    -h, --help             Print this help message.
",
        DEFAULT_MAX_SIZE_KB
    );
}

/// Parses command-line arguments from `std::env::args`.
fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut args_iter = env::args().skip(1);
    let mut repo: Option<String> = None;
    let mut token: Option<String> = None;
    let mut pattern_type = PatternType::Exclude;
    let mut patterns: Vec<String> = Vec::new();
    let mut max_file_size_kb = DEFAULT_MAX_SIZE_KB;

    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            "--token" => {
                token = Some(args_iter.next().ok_or("Expected a token after --token")?);
            }
            "--max-size" => {
                let size_str = args_iter
                    .next()
                    .ok_or("Expected a size in KB after --max-size")?;
                max_file_size_kb = size_str.parse()?;
            }
            "--include" => {
                pattern_type = PatternType::Include;
                let pat_str = args_iter.next().ok_or("Expected patterns after --include")?;
                patterns = pat_str.split(',').map(|s| s.trim().to_string()).collect();
            }
            "--exclude" => {
                pattern_type = PatternType::Exclude;
                let pat_str = args_iter.next().ok_or("Expected patterns after --exclude")?;
                patterns = pat_str.split(',').map(|s| s.trim().to_string()).collect();
            }
            _ if repo.is_none() && !arg.starts_with('-') => {
                repo = Some(arg);
            }
            _ => {
                return Err(format!("Unknown or misplaced argument: {}", arg).into());
            }
        }
    }

    Ok(Args {
        repo: repo.ok_or("Missing required repository argument <user/repo>")?,
        token,
        pattern_type,
        patterns,
        max_file_size_kb,
    })
}

/// Clones a Git repository into a destination directory.
fn clone_repo(repo: &str, token: &Option<String>, dest: &Path) -> Result<(), Box<dyn Error>> {
    let repo_url = if let Some(t) = token {
        format!("https://{}@github.com/{}.git", t, repo)
    } else {
        format!("https://github.com/{}.git", repo)
    };

    eprintln!("Cloning {}...", repo);
    let mut command = Command::new("git");
    command
        .env("GIT_TERMINAL_PROMPT", "0") // Disable interactive prompts
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg("--no-tags")
        .arg(&repo_url)
        .arg(dest.as_os_str())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let output = command.spawn()?.wait_with_output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Failed to clone repository: {}", stderr).into());
    }

    eprintln!("Clone successful.");
    Ok(())
}

/// A simple pattern matcher for file paths.
fn path_matches_patterns(relative_path: &Path, patterns: &[String]) -> bool {
    let path_str = relative_path.to_string_lossy();
    for pattern in patterns {
        if pattern.is_empty() {
            continue;
        }

        if pattern.starts_with("*.") {
            // Suffix match, e.g., "*.rs"
            if path_str.ends_with(&pattern[1..]) {
                return true;
            }
        } else if pattern.ends_with('/') {
            // Prefix match, e.g., "src/"
            if path_str.starts_with(pattern) {
                return true;
            }
        } else if pattern.contains('/') {
            // Exact path match, e.g., "src/main.rs"
            if path_str == *pattern {
                return true;
            }
        } else {
            // Component match, e.g., "target" or "Cargo.lock"
            if relative_path
                .components()
                .any(|c| c.as_os_str() == pattern.as_str())
            {
                return true;
            }
        }
    }
    false
}

/// Recursively traverses a directory, collecting files that match the criteria.
fn collect_files_recursive(
    dir: &Path,
    base_dir: &Path,
    args: &Args,
    files: &mut Vec<PathBuf>,
) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let relative_path = path.strip_prefix(base_dir).unwrap();

        if path.is_dir() {
            if relative_path.starts_with(".git") {
                continue;
            }
            // Also check directory patterns for early exit
            let should_exclude_dir = args.pattern_type == PatternType::Exclude
                && path_matches_patterns(relative_path, &args.patterns);

            if !should_exclude_dir {
                collect_files_recursive(&path, base_dir, args, files)?;
            }
        } else if path.is_file() {
            let metadata = entry.metadata()?;
            if metadata.len() > args.max_file_size_kb * 1024 {
                continue;
            }

            let should_keep = if args.patterns.is_empty() {
                // If no patterns are provided, keep file unless it's in an explicitly
                // excluded directory (which is handled above). `include` with no patterns
                // includes nothing. `exclude` with no patterns excludes nothing.
                args.pattern_type == PatternType::Exclude
            } else {
                let matches = path_matches_patterns(relative_path, &args.patterns);
                match args.pattern_type {
                    PatternType::Include => matches,
                    PatternType::Exclude => !matches,
                }
            };

            if should_keep {
                files.push(relative_path.to_path_buf());
            }
        }
    }
    Ok(())
}

/// Represents a node in the directory tree. It's a map of names to child nodes.
/// A newtype struct around a BTreeMap with boxed values is used to break the recursive type cycle.
#[derive(Debug, Default)]
struct TreeNode(BTreeMap<String, Box<TreeNode>>);

/// Builds a `TreeNode` map from a flat list of paths.
fn build_tree(paths: &[PathBuf]) -> TreeNode {
    let mut tree = TreeNode::default();
    for path in paths {
        let mut current_level = &mut tree;
        for component in path.components() {
            if let Component::Normal(name) = component {
                let name = name.to_string_lossy().into_owned();
                // .entry().or_default() gets a &mut Box<TreeNode>
                // which we can assign back to current_level (a &mut TreeNode)
                // thanks to DerefMut coercion.
                current_level = current_level.0.entry(name).or_default();
            }
        }
    }
    tree
}

/// Prints a `TreeNode` structure to a writer with tree-like formatting.
fn print_tree(
    tree: &TreeNode,
    prefix: &str,
    writer: &mut impl Write,
) -> io::Result<()> {
    let entries: Vec<_> = tree.0.iter().collect();
    let last_index = entries.len().saturating_sub(1);

    for (i, (name, child_node)) in entries.iter().enumerate() {
        let is_last = i == last_index;
        let connector = if is_last { "└── " } else { "├── " };
        writeln!(writer, "{}{}{}", prefix, connector, name)?;

        let new_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
        // child_node is &&Box<TreeNode>. It derefs to &TreeNode.
        // A leaf node (file) is a TreeNode with an empty map.
        if !child_node.0.is_empty() {
            print_tree(child_node, &new_prefix, writer)?;
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("Error parsing arguments: {}\n", e);
            print_usage();
            std::process::exit(1);
        }
    };

    let temp_dir = TempDir::new()?;
    clone_repo(&args.repo, &args.token, temp_dir.path())?;

    eprintln!("Processing repository...");
    let mut files_to_process = Vec::new();
    collect_files_recursive(
        temp_dir.path(),
        temp_dir.path(),
        &args,
        &mut files_to_process,
    )?;
    files_to_process.sort();
    eprintln!("Found {} files to include.", files_to_process.len());

    // --- Generate Output ---
    let stdout = io::stdout();
    let mut handle = stdout.lock();

    let total_files = files_to_process.len();
    let total_size_bytes: u64 = files_to_process
        .iter()
        .map(|p| {
            fs::metadata(temp_dir.path().join(p))
                .map(|m| m.len())
                .unwrap_or(0)
        })
        .sum();
    let total_size_kb = total_size_bytes / 1024;

    // 1. Summary Header
    writeln!(handle, "Repository: https://github.com/{}", args.repo)?;
    writeln!(handle, "---")?;
    writeln!(handle, "Summary:")?;
    writeln!(handle, "  - Total files included: {}", total_files)?;
    writeln!(handle, "  - Total size: ~{} KB", total_size_kb)?;
    writeln!(handle, "  - Max file size: {} KB", args.max_file_size_kb)?;
    if !args.patterns.is_empty() {
        writeln!(handle, "  - Filter type: {:?}", args.pattern_type)?;
        writeln!(handle, "  - Filter patterns: {:?}", args.patterns)?;
    }
    writeln!(handle, "\n---")?;

    // 2. Directory Structure
    writeln!(handle, "Directory Structure:")?;
    let tree = build_tree(&files_to_process);
    print_tree(&tree, "", &mut handle)?;
    writeln!(handle, "\n---")?;

    // 3. File Contents
    writeln!(handle, "File Contents:")?;
    for relative_path in &files_to_process {
        let full_path = temp_dir.path().join(relative_path);
        let path_str = relative_path.to_string_lossy();

        writeln!(handle, "\n--- {} ---", path_str)?;

        match fs::read(&full_path) {
            Ok(content_bytes) => {
                let content_str = String::from_utf8_lossy(&content_bytes);
                write!(handle, "{}", content_str)?;
            }
            Err(e) => {
                writeln!(handle, "Error reading file: {}", e)?;
            }
        }
    }

    eprintln!("\nDone. Digest printed to standard output.");
    Ok(())
}
```
