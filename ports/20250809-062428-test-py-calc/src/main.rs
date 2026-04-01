```rust
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process;

/// Configuration for the ingestion process, parsed from command-line arguments.
struct Config {
    root_path: PathBuf,
    exclude_patterns: Vec<String>,
    include_patterns: Vec<String>,
    max_file_size_kb: u64,
}

impl Config {
    /// Parses command-line arguments into a `Config` struct.
    /// Provides default exclusion patterns and settings.
    fn from_args(args: &[String]) -> Result<Config, String> {
        if args.len() < 2 || args.get(1).map_or(false, |a| a == "--help" || a == "-h") {
            let program_name = args.get(0).map_or("ingest", |s| s.as_str());
            let usage = format!(
                "Usage: {} <PATH> [OPTIONS]\n\n\
                 A tool to consolidate a repository's text files for LLM context.\n\n\
                 Arguments:\n  \
                 <PATH>              Path to the local repository directory.\n\n\
                 Options:\n  \
                 --exclude <PATTERNS> Comma-separated list of patterns to exclude (e.g., \"*.log,target/*\").\n  \
                 --include <PATTERNS> Comma-separated list of patterns to include. If specified, only matching files are considered.\n  \
                 --max-size <KB>      Maximum file size in kilobytes (default: 1024).\n  \
                 -h, --help           Show this help message.",
                program_name
            );
            return Err(usage);
        }

        let root_path = PathBuf::from(&args[1]);
        if !root_path.exists() {
            return Err("Provided path does not exist or is not accessible.".to_string());
        }
        if !root_path.is_dir() {
            return Err("Provided path is not a directory.".to_string());
        }

        let mut exclude_patterns = vec
