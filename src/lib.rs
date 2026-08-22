use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use proc_macro2::Span;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{BinOp, ExprBinary, ExprLit, Lit};
use thiserror::Error;
use wait_timeout::ChildExt;
use walkdir::{DirEntry, WalkDir};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const JOURNAL: &str = "target/mutation/active.json";

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Rust parse error in {path}: {source}")]
    Parse { path: PathBuf, source: syn::Error },
    #[error("mutation error: {0}")]
    Mutation(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Mutation {
    pub id: usize,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub original: String,
    pub replacement: String,
    #[serde(skip)]
    pub start: usize,
    #[serde(skip)]
    pub end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MutationResult {
    #[serde(flatten)]
    pub mutation: Mutation,
    pub status: Status,
    pub exit_code: Option<i32>,
    pub duration_seconds: f64,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Status { Killed, Survived, Timeout, Invalid, CompileError }

#[derive(Debug)]
pub struct CommandResult {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration: Duration,
    pub output: String,
}

#[derive(Serialize, Deserialize)]
struct JournalRecord { file: String, content: String }

fn ignored(entry: &DirEntry) -> bool {
    matches!(entry.file_name().to_str(), Some(".git" | "target" | "vendor" | "node_modules" | ".venv" | "venv" | "build" | "dist"))
}

fn is_test_path(path: &Path, root: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else { return false };
    relative.components().any(|part| part.as_os_str() == "tests")
        || relative.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.ends_with("_test.rs"))
}

pub fn discover_files(root: &Path, include_tests: bool, filters: &[String]) -> Vec<PathBuf> {
    let mut files: Vec<_> = WalkDir::new(root).into_iter().filter_entry(|entry| !ignored(entry)).filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file()).map(|entry| entry.into_path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("rs"))
        .filter(|path| include_tests || !is_test_path(path, root))
        .filter(|path| { if filters.is_empty() { true } else { let relative = path.strip_prefix(root).unwrap_or(path).to_string_lossy(); filters.iter().any(|filter| relative.contains(filter)) } })
        .collect();
    files.sort();
    files
}

fn replacement_for_binop(op: &BinOp) -> Option<&'static str> {
    match op {
        BinOp::Add(_) => Some("-"), BinOp::Sub(_) => Some("+"), BinOp::Mul(_) => Some("/"), BinOp::Div(_) => Some("*"), BinOp::Rem(_) => Some("*"),
        BinOp::And(_) => Some("||"), BinOp::Or(_) => Some("&&"), BinOp::Eq(_) => Some("!="), BinOp::Ne(_) => Some("=="), BinOp::Lt(_) => Some(">="),
        BinOp::Le(_) => Some(">"), BinOp::Gt(_) => Some("<="), BinOp::Ge(_) => Some("<"), _ => None,
    }
}

fn line_col(span: Span) -> (usize, usize) { let start = span.start(); (start.line.max(1), start.column + 1) }

struct MutationVisitor<'a> { source: &'a str, file: &'a str, candidates: Vec<Mutation> }

impl MutationVisitor<'_> {
    fn add_span(&mut self, span: Span, replacement: &str) {
        let range = span.byte_range();
        let Some(original) = self.source.get(range.clone()) else { return };
        if original == replacement || original.is_empty() { return; }
        let (line, column) = line_col(span);
        self.candidates.push(Mutation { id: 0, file: self.file.to_string(), line, column, original: original.to_string(), replacement: replacement.to_string(), start: range.start, end: range.end });
    }
}

impl<'ast> Visit<'ast> for MutationVisitor<'_> {
    fn visit_expr_binary(&mut self, node: &'ast ExprBinary) {
        if let Some(replacement) = replacement_for_binop(&node.op) { self.add_span(node.op.span(), replacement); }
        visit::visit_expr_binary(self, node);
    }
    fn visit_expr_lit(&mut self, node: &'ast ExprLit) {
        if let Lit::Bool(value) = &node.lit { self.add_span(value.span(), if value.value { "false" } else { "true" }); }
        visit::visit_expr_lit(self, node);
    }
}

pub fn enumerate_mutations(path: &Path, root: &Path, start_id: usize) -> Result<Vec<Mutation>, Error> {
    let source = fs::read_to_string(path)?;
    let syntax = syn::parse_file(&source).map_err(|source_error| Error::Parse { path: path.to_path_buf(), source: source_error })?;
    let file = path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/");
    let mut visitor = MutationVisitor { source: &source, file: &file, candidates: Vec::new() };
    visitor.visit_file(&syntax);
    visitor.candidates.sort_by_key(|item| (item.start, item.end));
    visitor.candidates.dedup_by_key(|item| (item.start, item.end));
    for (offset, item) in visitor.candidates.iter_mut().enumerate() { item.id = start_id + offset; }
    Ok(visitor.candidates)
}

pub fn collect_mutations(root: &Path, include_tests: bool, filters: &[String]) -> Result<Vec<Mutation>, Error> {
    let mut mutations = Vec::new();
    for path in discover_files(root, include_tests, filters) { let next = mutations.len() + 1; mutations.extend(enumerate_mutations(&path, root, next)?); }
    Ok(mutations)
}

pub fn run_shell(command: &str, root: &Path, timeout: Duration) -> Result<CommandResult, Error> {
    let started = Instant::now();
    #[cfg(windows)]
    let mut child = Command::new("cmd").args(["/C", command]).current_dir(root).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    #[cfg(not(windows))]
    let mut child = Command::new("sh").args(["-c", command]).current_dir(root).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let status = child.wait_timeout(timeout)?;
    if status.is_none() { let _ = child.kill(); }
    let output = child.wait_with_output()?;
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(CommandResult { exit_code: if status.is_none() { None } else { output.status.code() }, timed_out: status.is_none(), duration: started.elapsed(), output: combined })
}

pub fn recover_active(root: &Path) -> Result<bool, Error> {
    let journal = root.join(JOURNAL);
    if !journal.exists() { return Ok(false); }
    let record: JournalRecord = serde_json::from_slice(&fs::read(&journal)?)?;
    let bytes = BASE64.decode(record.content).map_err(|error| Error::Mutation(format!("invalid mutation journal: {error}")))?;
    fs::write(root.join(record.file), bytes)?;
    fs::remove_file(journal)?;
    Ok(true)
}

struct SourceGuard { path: PathBuf, original: Vec<u8>, journal: PathBuf }

impl SourceGuard {
    fn apply(root: &Path, mutation: &Mutation) -> Result<Self, Error> {
        let path = root.join(&mutation.file);
        let original = fs::read(&path)?;
        let journal = root.join(JOURNAL);
        if let Some(parent) = journal.parent() { fs::create_dir_all(parent)?; }
        let record = JournalRecord { file: mutation.file.clone(), content: BASE64.encode(&original) };
        fs::write(&journal, serde_json::to_vec_pretty(&record)?)?;
        let mut changed = original.clone();
        changed.splice(mutation.start..mutation.end, mutation.replacement.as_bytes().iter().copied());
        fs::write(&path, changed)?;
        Ok(Self { path, original, journal })
    }
}

impl Drop for SourceGuard {
    fn drop(&mut self) { let _ = fs::write(&self.path, &self.original); let _ = fs::remove_file(&self.journal); }
}

fn source_parses(path: &Path) -> bool { fs::read_to_string(path).ok().and_then(|source| syn::parse_file(&source).ok()).is_some() }

pub fn run_mutations(root: &Path, mutations: &[Mutation], test_command: &str, validate_command: Option<&str>, timeout: Duration, max_mutants: Option<usize>) -> Result<Vec<MutationResult>, Error> {
    recover_active(root)?;
    let limit = max_mutants.unwrap_or(mutations.len()).min(mutations.len());
    let mut results = Vec::with_capacity(limit);
    for mutation in mutations.iter().take(limit) {
        let guard = SourceGuard::apply(root, mutation)?;
        if !source_parses(&guard.path) {
            results.push(MutationResult { mutation: mutation.clone(), status: Status::Invalid, exit_code: None, duration_seconds: 0.0, detail: Some("mutated source does not parse".into()) });
            drop(guard); continue;
        }
        if let Some(command) = validate_command {
            let validation = run_shell(command, root, timeout)?;
            if validation.timed_out {
                results.push(MutationResult { mutation: mutation.clone(), status: Status::Timeout, exit_code: None, duration_seconds: validation.duration.as_secs_f64(), detail: Some("validation timed out".into()) });
                drop(guard); continue;
            }
            if validation.exit_code != Some(0) {
                let detail: String = validation.output.chars().rev().take(2000).collect::<String>().chars().rev().collect();
                results.push(MutationResult { mutation: mutation.clone(), status: Status::CompileError, exit_code: validation.exit_code, duration_seconds: validation.duration.as_secs_f64(), detail: Some(detail) });
                drop(guard); continue;
            }
        }
        let execution = run_shell(test_command, root, timeout)?;
        let (status, detail) = if execution.timed_out { (Status::Timeout, Some("test command timed out".into())) } else if execution.exit_code == Some(0) { (Status::Survived, None) } else { (Status::Killed, None) };
        results.push(MutationResult { mutation: mutation.clone(), status, exit_code: execution.exit_code, duration_seconds: execution.duration.as_secs_f64(), detail });
        drop(guard);
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn mutations_come_from_rust_expressions_not_comments_or_strings() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sample.rs");
        fs::write(&path, "fn choose(a: bool, b: bool) -> bool { let _s = \"== && true\"; /* != */ a && b }\n").unwrap();
        let mutations = enumerate_mutations(&path, dir.path(), 1).unwrap();
        assert_eq!(mutations.iter().filter(|item| item.original == "&&").count(), 1);
        assert!(!mutations.iter().any(|item| item.original == "=="));
    }

    #[test]
    fn source_guard_restores_source() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sample.rs");
        let original = "fn yes() -> bool { true }\n";
        fs::write(&path, original).unwrap();
        let mutation = enumerate_mutations(&path, dir.path(), 1).unwrap().remove(0);
        { let _guard = SourceGuard::apply(dir.path(), &mutation).unwrap(); assert_ne!(fs::read_to_string(&path).unwrap(), original); }
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert!(!dir.path().join(JOURNAL).exists());
    }

    #[test]
    fn timeout_is_not_killed() {
        let dir = tempdir().unwrap();
        let result = run_shell("sleep 1", dir.path(), Duration::from_millis(20)).unwrap();
        assert!(result.timed_out);
        assert_eq!(result.exit_code, None);
    }
}
