//! `cargo-context-lint` — Detect double error context from `fn_error_context` + `anyhow`.
//!
//! When a function is annotated with `#[context("...")]` from the `fn_error_context` crate,
//! the function body is automatically wrapped to add context to any error it returns.
//! If the caller *also* adds `.context()` or `.with_context()` from `anyhow::Context`,
//! the error will carry two context layers, which is redundant.
//!
//! This tool detects such "double context" patterns via syntactic analysis.
//!
//! Additionally, it can check that all functions returning `anyhow::Result` have a
//! `#[context]` annotation (the `--unattributed` check).

mod checker;
mod collector;
mod report;
mod unattributed;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;
use walkdir::WalkDir;

/// Lint level for optional checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum LintLevel {
    /// Allow (skip the check).
    Allow,
    /// Deny (flag as a warning, exit non-zero).
    Deny,
}

impl std::fmt::Display for LintLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LintLevel::Allow => write!(f, "allow"),
            LintLevel::Deny => write!(f, "deny"),
        }
    }
}

/// Detect double error context from `fn_error_context` + `anyhow`.
///
/// Finds call sites where a function annotated with `#[context("...")]` is called
/// and the result is additionally wrapped with `.context()` or `.with_context()`.
///
/// Optionally checks that all functions returning `anyhow::Result` have a
/// `#[context]` annotation.
#[derive(Parser, Debug)]
#[command(
    name = "cargo-context-lint",
    bin_name = "cargo context-lint",
    version,
    about
)]
struct Cli {
    // When invoked as `cargo context-lint`, cargo passes "context-lint" as the
    // first argument. We accept and ignore it.
    #[arg(hide = true, default_value = "context-lint")]
    _subcommand: String,

    /// Path to Cargo.toml (defaults to current directory).
    #[arg(long, value_name = "PATH")]
    manifest_path: Option<PathBuf>,

    /// Output format.
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    format: String,

    /// Show verbose output including all annotated functions found.
    #[arg(long)]
    verbose: bool,

    /// Check for functions returning anyhow::Result without #[context].
    #[arg(long, default_value_t = LintLevel::Deny, value_enum)]
    unattributed: LintLevel,
}

fn find_rust_files(dir: &Path) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            // Skip hidden directories, target directories, and common non-source dirs
            if e.file_type().is_dir() {
                return name != "target" && name != ".git" && name != ".hg";
            }
            true
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "rs"))
        .map(|e| e.into_path())
        .collect()
}

/// Discover source directories for the workspace using `cargo_metadata`.
fn discover_source_dirs(manifest_path: Option<&Path>) -> Result<(Vec<PathBuf>, PathBuf)> {
    let mut cmd = cargo_metadata::MetadataCommand::new();
    cmd.no_deps();
    if let Some(path) = manifest_path {
        cmd.manifest_path(path);
    }
    let metadata = cmd.exec().context("Running cargo metadata")?;

    let workspace_root = PathBuf::from(&metadata.workspace_root);

    let mut dirs = Vec::new();
    for package in &metadata.packages {
        // Only include packages that are workspace members
        if !metadata.workspace_members.contains(&package.id) {
            continue;
        }
        let pkg_dir = PathBuf::from(&package.manifest_path)
            .parent()
            .expect("manifest path should have parent")
            .to_path_buf();
        dirs.push(pkg_dir);
    }

    // Deduplicate in case packages share directories
    dirs.sort();
    dirs.dedup();

    Ok((dirs, workspace_root))
}

fn run() -> Result<bool> {
    let cli = Cli::parse();

    let (source_dirs, workspace_root) = discover_source_dirs(cli.manifest_path.as_deref())?;

    // Trailing slash so strip_prefix works cleanly
    let prefix = format!("{}/", workspace_root.display());

    // Collect all Rust files
    let mut all_files: Vec<PathBuf> = Vec::new();
    for dir in &source_dirs {
        all_files.extend(find_rust_files(dir));
    }

    if cli.verbose {
        eprintln!(
            "Scanning {} Rust files across {} package directories",
            all_files.len(),
            source_dirs.len()
        );
    }

    // Pass 1: Collect all #[context]-annotated functions
    let mut all_annotated = Vec::new();
    for file in &all_files {
        let entries = collector::collect_from_file(file)
            .with_context(|| format!("Collecting from {}", file.display()))?;
        all_annotated.extend(entries);
    }

    if cli.verbose {
        eprintln!("Found {} annotated functions", all_annotated.len());
        for entry in &all_annotated {
            let file = entry.file.strip_prefix(&prefix).unwrap_or(&entry.file);
            let kind = if entry.is_method { "method" } else { "fn" };
            eprintln!(
                "  {}:{} — {} {}() #[context(\"{}\")]",
                file, entry.line, kind, entry.name, entry.context_string
            );
        }
    }

    let index = collector::build_index(all_annotated);

    // Pass 2: Check for double-context call sites
    let mut all_double_context = Vec::new();
    for file in &all_files {
        let issues = checker::check_file(file, &index)
            .with_context(|| format!("Checking {}", file.display()))?;
        all_double_context.extend(issues);
    }

    // Sort by file and line for stable output
    all_double_context.sort_by(|a, b| {
        a.call_file
            .cmp(&b.call_file)
            .then(a.call_line.cmp(&b.call_line))
    });

    // Pass 3 (optional): Check for unattributed functions
    let mut all_unattributed = Vec::new();
    if cli.unattributed == LintLevel::Deny {
        for file in &all_files {
            let issues = unattributed::check_file(file)
                .with_context(|| format!("Checking unattributed in {}", file.display()))?;
            all_unattributed.extend(issues);
        }

        // Sort by file and line for stable output
        all_unattributed.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));

        if cli.verbose {
            eprintln!(
                "Found {} unattributed functions returning anyhow::Result",
                all_unattributed.len()
            );
        }
    }

    let found_issues = !all_double_context.is_empty() || !all_unattributed.is_empty();

    // Output results
    let output = match cli.format.as_str() {
        "json" => {
            report::format_combined_json(&all_double_context, &all_unattributed, Some(&prefix))
        }
        _ => report::format_combined_text(&all_double_context, &all_unattributed, Some(&prefix)),
    };

    if !output.is_empty() {
        print!("{output}");
    } else if cli.verbose {
        eprintln!("No issues found.");
    }

    Ok(found_issues)
}

fn main() -> ExitCode {
    match run() {
        Ok(found_issues) => {
            if found_issues {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}
