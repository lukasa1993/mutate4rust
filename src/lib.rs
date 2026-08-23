mod scope;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use proc_macro2::{Span, TokenStream, TokenTree};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
use std::io::Read;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use syn::parse::{ParseStream, Parser};
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Attribute, BinOp, Expr, ExprBinary, ExprLit, ForeignItem, ImplItem, Item, Lit, LitStr, Token,
    TraitItem,
};
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
pub enum Status {
    Killed,
    Survived,
    Timeout,
    Invalid,
    CompileError,
}

#[derive(Debug)]
pub struct CommandResult {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration: Duration,
    pub output: String,
}

#[derive(Serialize, Deserialize)]
struct JournalRecord {
    file: String,
    content: String,
}

fn ignored(entry: &DirEntry) -> bool {
    matches!(
        entry.file_name().to_str(),
        Some(".git" | "target" | "vendor" | "node_modules" | ".venv" | "venv" | "build" | "dist")
    )
}

fn is_test_path(path: &Path, root: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    relative
        .components()
        .any(|part| part.as_os_str() == "tests")
        || relative
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("_test.rs"))
}

fn is_auxiliary_path(path: &Path, root: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    if relative.file_name().and_then(|name| name.to_str()) == Some("build.rs") {
        return true;
    }
    relative.components().any(|part| {
        matches!(
            part.as_os_str().to_str(),
            Some("examples" | "benches" | "fuzz")
        )
    })
}

pub fn discover_files(root: &Path, include_tests: bool, filters: &[String]) -> Vec<PathBuf> {
    let mut files: Vec<_> = WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| !ignored(entry))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("rs"))
        .filter(|path| !is_auxiliary_path(path, root))
        .filter(|path| include_tests || !is_test_path(path, root))
        .filter(|path| {
            if filters.is_empty() {
                true
            } else {
                let relative = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
                filters.iter().any(|filter| relative.contains(filter))
            }
        })
        .collect();
    files.sort();
    files
}

#[derive(Clone, Copy)]
struct CfgPossibility {
    can_be_true_without_test: bool,
    can_be_false_without_test: bool,
}

impl CfgPossibility {
    const UNKNOWN: Self = Self {
        can_be_true_without_test: true,
        can_be_false_without_test: true,
    };
}

fn split_cfg_arguments(stream: TokenStream) -> Vec<TokenStream> {
    let mut output = Vec::new();
    let mut current = TokenStream::new();
    for token in stream {
        let separator = matches!(&token, TokenTree::Punct(value) if value.as_char() == ',');
        if separator {
            if !current.is_empty() {
                output.push(current);
                current = TokenStream::new();
            }
        } else {
            current.extend(std::iter::once(token));
        }
    }
    if !current.is_empty() {
        output.push(current);
    }
    output
}

fn cfg_possibility(stream: TokenStream) -> CfgPossibility {
    let tokens: Vec<_> = stream.into_iter().collect();
    if tokens.len() == 1 {
        return match &tokens[0] {
            TokenTree::Ident(value) if value == "test" => CfgPossibility {
                can_be_true_without_test: false,
                can_be_false_without_test: true,
            },
            TokenTree::Group(group) => cfg_possibility(group.stream()),
            _ => CfgPossibility::UNKNOWN,
        };
    }
    if tokens.len() != 2 {
        return CfgPossibility::UNKNOWN;
    }
    let (TokenTree::Ident(operation), TokenTree::Group(group)) = (&tokens[0], &tokens[1]) else {
        return CfgPossibility::UNKNOWN;
    };
    let arguments = split_cfg_arguments(group.stream());
    let possibilities: Vec<_> = arguments.into_iter().map(cfg_possibility).collect();
    match operation.to_string().as_str() {
        "all" => CfgPossibility {
            can_be_true_without_test: possibilities
                .iter()
                .all(|value| value.can_be_true_without_test),
            can_be_false_without_test: possibilities
                .iter()
                .any(|value| value.can_be_false_without_test),
        },
        "any" => CfgPossibility {
            can_be_true_without_test: possibilities
                .iter()
                .any(|value| value.can_be_true_without_test),
            can_be_false_without_test: possibilities
                .iter()
                .all(|value| value.can_be_false_without_test),
        },
        "not" if possibilities.len() == 1 => CfgPossibility {
            can_be_true_without_test: possibilities[0].can_be_false_without_test,
            can_be_false_without_test: possibilities[0].can_be_true_without_test,
        },
        _ => CfgPossibility::UNKNOWN,
    }
}

fn attrs_are_test_only(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attribute| {
        if attribute.path().is_ident("test") {
            return true;
        }
        match &attribute.meta {
            syn::Meta::List(list) if list.path.is_ident("cfg") => {
                !cfg_possibility(list.tokens.clone()).can_be_true_without_test
            }
            _ => false,
        }
    })
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(value) => &value.attrs,
        Item::Enum(value) => &value.attrs,
        Item::ExternCrate(value) => &value.attrs,
        Item::Fn(value) => &value.attrs,
        Item::ForeignMod(value) => &value.attrs,
        Item::Impl(value) => &value.attrs,
        Item::Macro(value) => &value.attrs,
        Item::Mod(value) => &value.attrs,
        Item::Static(value) => &value.attrs,
        Item::Struct(value) => &value.attrs,
        Item::Trait(value) => &value.attrs,
        Item::TraitAlias(value) => &value.attrs,
        Item::Type(value) => &value.attrs,
        Item::Union(value) => &value.attrs,
        Item::Use(value) => &value.attrs,
        _ => &[],
    }
}

fn collect_test_only_ranges(items: &[Item], output: &mut Vec<Range<usize>>) {
    for item in items {
        if attrs_are_test_only(item_attrs(item)) {
            output.push(item.span().byte_range());
            continue;
        }
        match item {
            Item::Mod(module) => {
                if let Some((_, nested)) = &module.content {
                    collect_test_only_ranges(nested, output);
                }
            }
            Item::Impl(implementation) => {
                for member in &implementation.items {
                    let attrs = match member {
                        syn::ImplItem::Const(value) => &value.attrs[..],
                        syn::ImplItem::Fn(value) => &value.attrs[..],
                        syn::ImplItem::Type(value) => &value.attrs[..],
                        syn::ImplItem::Macro(value) => &value.attrs[..],
                        _ => &[],
                    };
                    if attrs_are_test_only(attrs) {
                        output.push(member.span().byte_range());
                    }
                }
            }
            Item::Trait(trait_item) => {
                for member in &trait_item.items {
                    let attrs = match member {
                        syn::TraitItem::Const(value) => &value.attrs[..],
                        syn::TraitItem::Fn(value) => &value.attrs[..],
                        syn::TraitItem::Type(value) => &value.attrs[..],
                        syn::TraitItem::Macro(value) => &value.attrs[..],
                        _ => &[],
                    };
                    if attrs_are_test_only(attrs) {
                        output.push(member.span().byte_range());
                    }
                }
            }
            _ => {}
        }
    }
}

fn impl_item_attrs(item: &ImplItem) -> &[Attribute] {
    match item {
        ImplItem::Const(value) => &value.attrs,
        ImplItem::Fn(value) => &value.attrs,
        ImplItem::Type(value) => &value.attrs,
        ImplItem::Macro(value) => &value.attrs,
        _ => &[],
    }
}

fn trait_item_attrs(item: &TraitItem) -> &[Attribute] {
    match item {
        TraitItem::Const(value) => &value.attrs,
        TraitItem::Fn(value) => &value.attrs,
        TraitItem::Type(value) => &value.attrs,
        TraitItem::Macro(value) => &value.attrs,
        _ => &[],
    }
}

fn foreign_item_attrs(item: &ForeignItem) -> &[Attribute] {
    match item {
        ForeignItem::Fn(value) => &value.attrs,
        ForeignItem::Static(value) => &value.attrs,
        ForeignItem::Type(value) => &value.attrs,
        ForeignItem::Macro(value) => &value.attrs,
        _ => &[],
    }
}

fn expr_attrs(expr: &Expr) -> &[Attribute] {
    match expr {
        Expr::Array(value) => &value.attrs,
        Expr::Assign(value) => &value.attrs,
        Expr::Async(value) => &value.attrs,
        Expr::Await(value) => &value.attrs,
        Expr::Binary(value) => &value.attrs,
        Expr::Block(value) => &value.attrs,
        Expr::Break(value) => &value.attrs,
        Expr::Call(value) => &value.attrs,
        Expr::Cast(value) => &value.attrs,
        Expr::Closure(value) => &value.attrs,
        Expr::Const(value) => &value.attrs,
        Expr::Continue(value) => &value.attrs,
        Expr::Field(value) => &value.attrs,
        Expr::ForLoop(value) => &value.attrs,
        Expr::Group(value) => &value.attrs,
        Expr::If(value) => &value.attrs,
        Expr::Index(value) => &value.attrs,
        Expr::Infer(value) => &value.attrs,
        Expr::Let(value) => &value.attrs,
        Expr::Lit(value) => &value.attrs,
        Expr::Loop(value) => &value.attrs,
        Expr::Macro(value) => &value.attrs,
        Expr::Match(value) => &value.attrs,
        Expr::MethodCall(value) => &value.attrs,
        Expr::Paren(value) => &value.attrs,
        Expr::Path(value) => &value.attrs,
        Expr::Range(value) => &value.attrs,
        Expr::RawAddr(value) => &value.attrs,
        Expr::Reference(value) => &value.attrs,
        Expr::Repeat(value) => &value.attrs,
        Expr::Return(value) => &value.attrs,
        Expr::Struct(value) => &value.attrs,
        Expr::Try(value) => &value.attrs,
        Expr::TryBlock(value) => &value.attrs,
        Expr::Tuple(value) => &value.attrs,
        Expr::Unary(value) => &value.attrs,
        Expr::Unsafe(value) => &value.attrs,
        Expr::While(value) => &value.attrs,
        Expr::Yield(value) => &value.attrs,
        _ => &[],
    }
}

fn range_with_attrs(attrs: &[Attribute], node: &impl Spanned) -> Range<usize> {
    let range = node.span().byte_range();
    let start = attrs
        .first()
        .map(|attribute| attribute.span().byte_range().start)
        .unwrap_or(range.start);
    start..range.end
}

struct InactiveRangeVisitor<'a> {
    cfg: &'a scope::CfgContext,
    ranges: Vec<Range<usize>>,
}

impl InactiveRangeVisitor<'_> {
    fn inactive(&mut self, attrs: &[Attribute], node: &impl Spanned) -> bool {
        if self.cfg.attrs_active(attrs) {
            false
        } else {
            self.ranges.push(range_with_attrs(attrs, node));
            true
        }
    }
}

impl<'ast> Visit<'ast> for InactiveRangeVisitor<'_> {
    fn visit_item(&mut self, node: &'ast Item) {
        if self.inactive(item_attrs(node), node) {
            return;
        }
        visit::visit_item(self, node);
    }

    fn visit_impl_item(&mut self, node: &'ast ImplItem) {
        if self.inactive(impl_item_attrs(node), node) {
            return;
        }
        visit::visit_impl_item(self, node);
    }

    fn visit_trait_item(&mut self, node: &'ast TraitItem) {
        if self.inactive(trait_item_attrs(node), node) {
            return;
        }
        visit::visit_trait_item(self, node);
    }

    fn visit_foreign_item(&mut self, node: &'ast ForeignItem) {
        if self.inactive(foreign_item_attrs(node), node) {
            return;
        }
        visit::visit_foreign_item(self, node);
    }

    fn visit_local(&mut self, node: &'ast syn::Local) {
        if self.inactive(&node.attrs, node) {
            return;
        }
        visit::visit_local(self, node);
    }

    fn visit_arm(&mut self, node: &'ast syn::Arm) {
        if self.inactive(&node.attrs, node) {
            return;
        }
        visit::visit_arm(self, node);
    }

    fn visit_field(&mut self, node: &'ast syn::Field) {
        if self.inactive(&node.attrs, node) {
            return;
        }
        visit::visit_field(self, node);
    }

    fn visit_field_value(&mut self, node: &'ast syn::FieldValue) {
        if self.inactive(&node.attrs, node) {
            return;
        }
        visit::visit_field_value(self, node);
    }

    fn visit_variant(&mut self, node: &'ast syn::Variant) {
        if self.inactive(&node.attrs, node) {
            return;
        }
        visit::visit_variant(self, node);
    }

    fn visit_stmt_macro(&mut self, node: &'ast syn::StmtMacro) {
        if self.inactive(&node.attrs, node) {
            return;
        }
        visit::visit_stmt_macro(self, node);
    }

    fn visit_expr(&mut self, node: &'ast Expr) {
        if self.inactive(expr_attrs(node), node) {
            return;
        }
        visit::visit_expr(self, node);
    }
}

fn inactive_file_ranges(file: &syn::File, cfg: &scope::CfgContext) -> Vec<Range<usize>> {
    let mut visitor = InactiveRangeVisitor {
        cfg,
        ranges: Vec::new(),
    };
    visitor.visit_file(file);
    visitor.ranges.sort_by_key(|range| (range.start, range.end));
    visitor.ranges
}

fn inactive_expr_ranges(expr: &Expr, cfg: &scope::CfgContext) -> Vec<Range<usize>> {
    let mut visitor = InactiveRangeVisitor {
        cfg,
        ranges: Vec::new(),
    };
    visitor.visit_expr(expr);
    visitor.ranges.sort_by_key(|range| (range.start, range.end));
    visitor.ranges
}

fn replacement_for_binop(op: &BinOp) -> Option<&'static str> {
    match op {
        BinOp::Add(_) => Some("-"),
        BinOp::Sub(_) => Some("+"),
        BinOp::Mul(_) => Some("/"),
        BinOp::Div(_) => Some("*"),
        BinOp::Rem(_) => Some("*"),
        BinOp::And(_) => Some("||"),
        BinOp::Or(_) => Some("&&"),
        BinOp::Eq(_) => Some("!="),
        BinOp::Ne(_) => Some("=="),
        BinOp::Lt(_) => Some(">="),
        BinOp::Le(_) => Some(">"),
        BinOp::Gt(_) => Some("<="),
        BinOp::Ge(_) => Some("<"),
        _ => None,
    }
}

fn line_col(span: Span) -> (usize, usize) {
    let start = span.start();
    (start.line.max(1), start.column + 1)
}

struct MutationVisitor<'a> {
    source: &'a str,
    file: &'a str,
    excluded_ranges: &'a [Range<usize>],
    candidates: Vec<Mutation>,
}

impl MutationVisitor<'_> {
    fn add_span(&mut self, span: Span, replacement: &str) {
        let range = span.byte_range();
        if self
            .excluded_ranges
            .iter()
            .any(|excluded| excluded.start <= range.start && range.end <= excluded.end)
        {
            return;
        }
        let Some(original) = self.source.get(range.clone()) else {
            return;
        };
        if original == replacement || original.is_empty() {
            return;
        }
        let (line, column) = line_col(span);
        self.candidates.push(Mutation {
            id: 0,
            file: self.file.to_string(),
            line,
            column,
            original: original.to_string(),
            replacement: replacement.to_string(),
            start: range.start,
            end: range.end,
        });
    }
}

impl<'ast> Visit<'ast> for MutationVisitor<'_> {
    fn visit_expr_binary(&mut self, node: &'ast ExprBinary) {
        if let Some(replacement) = replacement_for_binop(&node.op) {
            self.add_span(node.op.span(), replacement);
        }
        visit::visit_expr_binary(self, node);
    }

    fn visit_expr_lit(&mut self, node: &'ast ExprLit) {
        if let Lit::Bool(value) = &node.lit {
            self.add_span(value.span(), if value.value { "false" } else { "true" });
        }
        visit::visit_expr_lit(self, node);
    }
}

fn enumerate_mutations_with_tests(
    path: &Path,
    root: &Path,
    start_id: usize,
    include_tests: bool,
) -> Result<Vec<Mutation>, Error> {
    let source = fs::read_to_string(path)?;
    let syntax = syn::parse_file(&source).map_err(|source_error| Error::Parse {
        path: path.to_path_buf(),
        source: source_error,
    })?;
    let file = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let mut excluded_ranges = Vec::new();
    if !include_tests {
        collect_test_only_ranges(&syntax.items, &mut excluded_ranges);
    }
    let mut visitor = MutationVisitor {
        source: &source,
        file: &file,
        excluded_ranges: &excluded_ranges,
        candidates: Vec::new(),
    };
    visitor.visit_file(&syntax);
    visitor
        .candidates
        .sort_by_key(|item| (item.start, item.end));
    visitor
        .candidates
        .dedup_by_key(|item| (item.start, item.end));
    for (offset, item) in visitor.candidates.iter_mut().enumerate() {
        item.id = start_id + offset;
    }
    Ok(visitor.candidates)
}

pub fn enumerate_mutations(
    path: &Path,
    root: &Path,
    start_id: usize,
) -> Result<Vec<Mutation>, Error> {
    enumerate_mutations_with_tests(path, root, start_id, false)
}

fn enumerate_active_mutations(
    active: &scope::ActiveFile,
    root: &Path,
    start_id: usize,
) -> Result<Vec<Mutation>, Error> {
    let source = fs::read_to_string(&active.path)?;
    let syntax = syn::parse_file(&source).map_err(|source_error| Error::Parse {
        path: active.path.clone(),
        source: source_error,
    })?;
    let file = active
        .path
        .strip_prefix(root)
        .unwrap_or(&active.path)
        .to_string_lossy()
        .replace('\\', "/");
    let excluded_ranges = inactive_file_ranges(&syntax, &active.cfg);
    let mut visitor = MutationVisitor {
        source: &source,
        file: &file,
        excluded_ranges: &excluded_ranges,
        candidates: Vec::new(),
    };
    visitor.visit_file(&syntax);
    visitor
        .candidates
        .sort_by_key(|item| (item.start, item.end));
    visitor
        .candidates
        .dedup_by_key(|item| (item.start, item.end));
    for (offset, item) in visitor.candidates.iter_mut().enumerate() {
        item.id = start_id + offset;
    }
    Ok(visitor.candidates)
}

fn include_literal(tokens: TokenStream) -> Option<LitStr> {
    let parser = |input: ParseStream<'_>| {
        let literal: LitStr = input.parse()?;
        if input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
        }
        if !input.is_empty() {
            return Err(input.error("include! expects one string literal"));
        }
        Ok(literal)
    };
    parser.parse2(tokens).ok()
}

fn expression_include_path(node: &syn::ExprMacro, source_dir: &Path) -> Option<PathBuf> {
    if !node.mac.path.is_ident("include") {
        return None;
    }
    let literal = include_literal(node.mac.tokens.clone())?;
    let path = PathBuf::from(literal.value());
    if path.extension().and_then(|value| value.to_str()) != Some("rs") {
        return None;
    }
    Some(if path.is_absolute() {
        path
    } else {
        source_dir.join(path)
    })
}

struct ExpressionIncludeVisitor<'a> {
    source_dir: &'a Path,
    excluded_ranges: &'a [Range<usize>],
    paths: Vec<PathBuf>,
}

impl<'ast> Visit<'ast> for ExpressionIncludeVisitor<'_> {
    fn visit_expr_macro(&mut self, node: &'ast syn::ExprMacro) {
        let range = node.span().byte_range();
        let excluded = self
            .excluded_ranges
            .iter()
            .any(|item| item.start <= range.start && range.end <= item.end);
        if !excluded {
            if let Some(path) = expression_include_path(node, self.source_dir) {
                self.paths.push(path);
            }
            visit::visit_expr_macro(self, node);
        }
    }
}

fn expression_includes_from_file(
    file: &syn::File,
    path: &Path,
    cfg: &scope::CfgContext,
) -> Vec<PathBuf> {
    let ranges = inactive_file_ranges(file, cfg);
    let source_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut visitor = ExpressionIncludeVisitor {
        source_dir,
        excluded_ranges: &ranges,
        paths: Vec::new(),
    };
    visitor.visit_file(file);
    visitor.paths
}

fn expression_includes_from_expr(
    expr: &Expr,
    path: &Path,
    cfg: &scope::CfgContext,
) -> Vec<PathBuf> {
    let ranges = inactive_expr_ranges(expr, cfg);
    let source_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut visitor = ExpressionIncludeVisitor {
        source_dir,
        excluded_ranges: &ranges,
        paths: Vec::new(),
    };
    visitor.visit_expr(expr);
    visitor.paths
}

fn enumerate_expression_mutations(
    path: &Path,
    root: &Path,
    cfg: &scope::CfgContext,
    start_id: usize,
) -> Result<(Vec<Mutation>, Vec<PathBuf>), Error> {
    let canonical = path.canonicalize()?;
    let canonical_root = root.canonicalize()?;
    let relative = canonical.strip_prefix(&canonical_root).map_err(|_| {
        Error::Mutation(format!(
            "refusing to mutate included Rust source outside the project root: {}",
            canonical.display()
        ))
    })?;
    if relative.components().any(|component| {
        matches!(
            component,
            Component::Normal(value)
                if matches!(value.to_str(), Some(".git" | "target" | "vendor" | "build" | "dist"))
        )
    }) {
        return Err(Error::Mutation(format!(
            "refusing to mutate generated or vendored included source: {}",
            canonical.display()
        )));
    }
    let source = fs::read_to_string(&canonical)?;
    let syntax = syn::parse_str::<Expr>(&source).map_err(|source_error| Error::Parse {
        path: canonical.clone(),
        source: source_error,
    })?;
    let excluded_ranges = inactive_expr_ranges(&syntax, cfg);
    let file = relative.to_string_lossy().replace('\\', "/");
    let mut visitor = MutationVisitor {
        source: &source,
        file: &file,
        excluded_ranges: &excluded_ranges,
        candidates: Vec::new(),
    };
    visitor.visit_expr(&syntax);
    visitor
        .candidates
        .sort_by_key(|item| (item.start, item.end));
    visitor
        .candidates
        .dedup_by_key(|item| (item.start, item.end));
    for (offset, item) in visitor.candidates.iter_mut().enumerate() {
        item.id = start_id + offset;
    }
    let includes = expression_includes_from_expr(&syntax, &canonical, cfg);
    Ok((visitor.candidates, includes))
}

pub fn collect_mutations(
    root: &Path,
    include_tests: bool,
    filters: &[String],
) -> Result<Vec<Mutation>, Error> {
    let active = scope::discover(root, include_tests, filters).map_err(Error::Mutation)?;
    let mut mutations = Vec::new();
    let mut queue = VecDeque::new();
    for file in &active {
        let source = fs::read_to_string(&file.path)?;
        let syntax = syn::parse_file(&source).map_err(|source_error| Error::Parse {
            path: file.path.clone(),
            source: source_error,
        })?;
        let next = mutations.len() + 1;
        mutations.extend(enumerate_active_mutations(file, root, next)?);
        for path in expression_includes_from_file(&syntax, &file.path, &file.cfg) {
            queue.push_back((path, file.cfg.clone()));
        }
    }
    let mut visited = std::collections::HashSet::new();
    while let Some((path, cfg)) = queue.pop_front() {
        let canonical = path.canonicalize()?;
        if !visited.insert(canonical.clone()) {
            continue;
        }
        let next = mutations.len() + 1;
        let (found, nested) = enumerate_expression_mutations(&canonical, root, &cfg, next)?;
        mutations.extend(found);
        for path in nested {
            queue.push_back((path, cfg.clone()));
        }
    }
    Ok(mutations)
}

fn spawn_shell(command: &str, root: &Path) -> Result<Child, std::io::Error> {
    #[cfg(windows)]
    let mut shell = {
        let mut value = Command::new("cmd");
        value.args(["/C", command]);
        value
    };
    #[cfg(not(windows))]
    let mut shell = {
        let mut value = Command::new("sh");
        value.args(["-c", command]);
        value
    };
    shell
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        shell.process_group(0);
    }
    shell.spawn()
}

fn terminate_process_tree(child: &mut Child) {
    #[cfg(unix)]
    if let Ok(group) = i32::try_from(child.id()) {
        // The child starts a new process group, so killing the group also stops
        // Cargo/rustc/test descendants before the source guard restores files.
        unsafe {
            libc::killpg(group, libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

const MAX_CAPTURE_BYTES: usize = 2 * 1024 * 1024;

type OutputReader = thread::JoinHandle<std::io::Result<Vec<u8>>>;

fn drain_output<R: Read + Send + 'static>(mut reader: R) -> OutputReader {
    thread::spawn(move || {
        let mut captured = VecDeque::with_capacity(MAX_CAPTURE_BYTES);
        let mut buffer = [0_u8; 8192];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            let incoming = &buffer[..count];
            if incoming.len() >= MAX_CAPTURE_BYTES {
                captured.clear();
                captured.extend(
                    incoming[incoming.len() - MAX_CAPTURE_BYTES..]
                        .iter()
                        .copied(),
                );
                continue;
            }
            let excess = captured
                .len()
                .saturating_add(incoming.len())
                .saturating_sub(MAX_CAPTURE_BYTES);
            if excess > 0 {
                captured.drain(..excess);
            }
            captured.extend(incoming.iter().copied());
        }
        Ok(captured.into_iter().collect())
    })
}

fn join_output(reader: OutputReader) -> Result<Vec<u8>, Error> {
    let output = reader
        .join()
        .map_err(|_| Error::Mutation("command output reader thread panicked".into()))??;
    Ok(output)
}

pub fn run_shell(command: &str, root: &Path, timeout: Duration) -> Result<CommandResult, Error> {
    let started = Instant::now();
    let mut child = spawn_shell(command, root)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Mutation("child stdout pipe is unavailable".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::Mutation("child stderr pipe is unavailable".into()))?;
    let stdout_reader = drain_output(stdout);
    let stderr_reader = drain_output(stderr);
    let (status, timed_out) = match child.wait_timeout(timeout)? {
        Some(status) => (status, false),
        None => {
            terminate_process_tree(&mut child);
            (child.wait()?, true)
        }
    };
    let stdout = join_output(stdout_reader)?;
    let stderr = join_output(stderr_reader)?;
    let mut combined = String::from_utf8_lossy(&stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&stderr));
    Ok(CommandResult {
        exit_code: if timed_out { None } else { status.code() },
        timed_out,
        duration: started.elapsed(),
        output: combined,
    })
}

fn resolve_source_path(root: &Path, file: &str) -> Result<PathBuf, Error> {
    let relative = Path::new(file);
    let unsafe_component = relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    });
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || unsafe_component
        || relative.extension().and_then(|value| value.to_str()) != Some("rs")
    {
        return Err(Error::Mutation(format!(
            "unsafe mutation source path: {file}"
        )));
    }
    let canonical_root = root.canonicalize()?;
    let target = root.join(relative).canonicalize()?;
    if !target.starts_with(&canonical_root) {
        return Err(Error::Mutation(format!(
            "mutation source escapes repository root: {file}"
        )));
    }
    Ok(target)
}

pub fn recover_active(root: &Path) -> Result<bool, Error> {
    let journal = root.join(JOURNAL);
    if !journal.exists() {
        return Ok(false);
    }
    let record: JournalRecord = serde_json::from_slice(&fs::read(&journal)?)?;
    let bytes = BASE64
        .decode(record.content)
        .map_err(|error| Error::Mutation(format!("invalid mutation journal: {error}")))?;
    let path = resolve_source_path(root, &record.file)?;
    fs::write(path, bytes)?;
    fs::remove_file(journal)?;
    Ok(true)
}

struct SourceGuard {
    path: PathBuf,
    original: Vec<u8>,
    journal: PathBuf,
}

impl SourceGuard {
    fn apply(root: &Path, mutation: &Mutation) -> Result<Self, Error> {
        let path = resolve_source_path(root, &mutation.file)?;
        let original = fs::read(&path)?;
        let journal = root.join(JOURNAL);
        if let Some(parent) = journal.parent() {
            fs::create_dir_all(parent)?;
        }
        let record = JournalRecord {
            file: mutation.file.clone(),
            content: BASE64.encode(&original),
        };
        fs::write(&journal, serde_json::to_vec_pretty(&record)?)?;
        let mut changed = original.clone();
        changed.splice(
            mutation.start..mutation.end,
            mutation.replacement.as_bytes().iter().copied(),
        );
        fs::write(&path, changed)?;
        Ok(Self {
            path,
            original,
            journal,
        })
    }
}

impl Drop for SourceGuard {
    fn drop(&mut self) {
        if fs::write(&self.path, &self.original).is_ok() {
            let _ = fs::remove_file(&self.journal);
        }
    }
}

fn source_parses(path: &Path) -> bool {
    fs::read_to_string(path)
        .ok()
        .and_then(|source| syn::parse_file(&source).ok())
        .is_some()
}

pub fn run_mutations(
    root: &Path,
    mutations: &[Mutation],
    test_command: &str,
    validate_command: Option<&str>,
    timeout: Duration,
    max_mutants: Option<usize>,
) -> Result<Vec<MutationResult>, Error> {
    recover_active(root)?;
    let limit = max_mutants.unwrap_or(mutations.len()).min(mutations.len());
    let mut results = Vec::with_capacity(limit);
    for mutation in mutations.iter().take(limit) {
        let guard = SourceGuard::apply(root, mutation)?;
        if !source_parses(&guard.path) {
            results.push(MutationResult {
                mutation: mutation.clone(),
                status: Status::Invalid,
                exit_code: None,
                duration_seconds: 0.0,
                detail: Some("mutated source does not parse".into()),
            });
            drop(guard);
            continue;
        }
        if let Some(command) = validate_command {
            let validation = run_shell(command, root, timeout)?;
            if validation.timed_out {
                results.push(MutationResult {
                    mutation: mutation.clone(),
                    status: Status::Timeout,
                    exit_code: None,
                    duration_seconds: validation.duration.as_secs_f64(),
                    detail: Some("validation timed out".into()),
                });
                drop(guard);
                continue;
            }
            if validation.exit_code != Some(0) {
                let detail: String = validation
                    .output
                    .chars()
                    .rev()
                    .take(2000)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                results.push(MutationResult {
                    mutation: mutation.clone(),
                    status: Status::CompileError,
                    exit_code: validation.exit_code,
                    duration_seconds: validation.duration.as_secs_f64(),
                    detail: Some(detail),
                });
                drop(guard);
                continue;
            }
        }
        let execution = run_shell(test_command, root, timeout)?;
        let (status, detail) = if execution.timed_out {
            (Status::Timeout, Some("test command timed out".into()))
        } else if execution.exit_code == Some(0) {
            (Status::Survived, None)
        } else {
            (Status::Killed, None)
        };
        results.push(MutationResult {
            mutation: mutation.clone(),
            status,
            exit_code: execution.exit_code,
            duration_seconds: execution.duration.as_secs_f64(),
            detail,
        });
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
        fs::write(
            &path,
            "fn choose(a: bool, b: bool) -> bool { let _s = \"== && true\"; /* != */ a && b }\n",
        )
        .unwrap();
        let mutations = enumerate_mutations(&path, dir.path(), 1).unwrap();
        assert_eq!(
            mutations
                .iter()
                .filter(|item| item.original == "&&")
                .count(),
            1
        );
        assert!(!mutations.iter().any(|item| item.original == "=="));
    }

    #[test]
    fn inline_test_modules_are_not_mutated_by_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sample.rs");
        fs::write(
            &path,
            "fn production(a: bool, b: bool) -> bool { a && b }\n#[cfg(test)]\nmod tests {\n #[test]\n fn test_logic() { assert!(true && false); }\n}\n",
        )
        .unwrap();
        let mutations = enumerate_mutations(&path, dir.path(), 1).unwrap();
        assert_eq!(
            mutations
                .iter()
                .filter(|item| item.original == "&&")
                .count(),
            1
        );
        assert!(!mutations
            .iter()
            .any(|item| matches!(item.original.as_str(), "true" | "false")));
    }

    #[test]
    fn cfg_not_test_code_is_still_mutated() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sample.rs");
        fs::write(
            &path,
            "#[cfg(not(test))]\nfn production() -> bool { true }\n",
        )
        .unwrap();
        let mutations = enumerate_mutations(&path, dir.path(), 1).unwrap();
        assert!(mutations.iter().any(|item| item.original == "true"));
    }

    #[test]
    fn discovery_excludes_build_scripts_and_auxiliary_targets() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::create_dir_all(dir.path().join("examples")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "fn production() {}\n").unwrap();
        fs::write(dir.path().join("build.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.path().join("examples/demo.rs"), "fn main() {}\n").unwrap();
        let files = discover_files(dir.path(), false, &[]);
        assert_eq!(files, vec![dir.path().join("src/lib.rs")]);
    }

    #[test]
    fn source_guard_restores_source() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sample.rs");
        let original = "fn yes() -> bool { true }\n";
        fs::write(&path, original).unwrap();
        let mutation = enumerate_mutations(&path, dir.path(), 1).unwrap().remove(0);
        {
            let _guard = SourceGuard::apply(dir.path(), &mutation).unwrap();
            assert_ne!(fs::read_to_string(&path).unwrap(), original);
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert!(!dir.path().join(JOURNAL).exists());
    }

    #[test]
    fn recovery_rejects_parent_path_escape() {
        let dir = tempdir().unwrap();
        let parent = dir.path().parent().unwrap();
        let outside_name = format!(
            "{}-escape.rs",
            dir.path().file_name().unwrap().to_string_lossy()
        );
        let outside = parent.join(&outside_name);
        fs::write(&outside, "outside\n").unwrap();
        let journal = dir.path().join(JOURNAL);
        fs::create_dir_all(journal.parent().unwrap()).unwrap();
        let record = JournalRecord {
            file: format!("../{outside_name}"),
            content: BASE64.encode(b"overwritten\n"),
        };
        fs::write(&journal, serde_json::to_vec_pretty(&record).unwrap()).unwrap();

        let result = recover_active(dir.path());

        assert!(matches!(result, Err(Error::Mutation(_))));
        assert_eq!(fs::read_to_string(&outside).unwrap(), "outside\n");
        assert!(journal.exists());
        let _ = fs::remove_file(outside);
    }

    #[cfg(unix)]
    #[test]
    fn recovery_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("outside.rs");
        fs::write(&outside_file, "outside\n").unwrap();
        symlink(&outside_file, dir.path().join("linked.rs")).unwrap();
        let journal = dir.path().join(JOURNAL);
        fs::create_dir_all(journal.parent().unwrap()).unwrap();
        let record = JournalRecord {
            file: "linked.rs".into(),
            content: BASE64.encode(b"overwritten\n"),
        };
        fs::write(&journal, serde_json::to_vec_pretty(&record).unwrap()).unwrap();

        let result = recover_active(dir.path());

        assert!(matches!(result, Err(Error::Mutation(_))));
        assert_eq!(fs::read_to_string(&outside_file).unwrap(), "outside\n");
        assert!(journal.exists());
    }

    #[test]
    fn source_guard_keeps_journal_when_restore_fails() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let path = src.join("sample.rs");
        fs::write(&path, "fn yes() -> bool { true }\n").unwrap();
        let mutation = enumerate_mutations(&path, dir.path(), 1).unwrap().remove(0);
        let guard = SourceGuard::apply(dir.path(), &mutation).unwrap();
        let journal = dir.path().join(JOURNAL);
        fs::remove_file(&path).unwrap();
        fs::remove_dir(&src).unwrap();
        fs::write(&src, "not a directory").unwrap();

        drop(guard);

        assert!(journal.exists());
    }

    #[test]
    fn large_output_is_drained_without_false_timeout() {
        let dir = tempdir().unwrap();
        #[cfg(windows)]
        let command = "for /L %i in (1,1,10000) do @echo 0123456789";
        #[cfg(not(windows))]
        let command =
            "i=0; while [ \"$i\" -lt 10000 ]; do printf '0123456789\\n'; i=$((i + 1)); done";
        let result = run_shell(command, dir.path(), Duration::from_secs(20)).unwrap();
        assert!(!result.timed_out);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.output.len() >= 100_000);
        assert!(result.output.len() <= MAX_CAPTURE_BYTES);
    }

    #[test]
    fn repository_collection_excludes_inactive_platform_modules() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='mutation-scope-fixture'\nversion='0.1.0'\nedition='2021'\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            "#[cfg(unix)] mod unix_only;\n#[cfg(windows)] mod windows_only;\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/unix_only.rs"),
            "pub fn enabled() -> bool { true }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/windows_only.rs"),
            "pub fn disabled() -> bool { false }\n",
        )
        .unwrap();

        let mutations = collect_mutations(dir.path(), false, &[]).unwrap();
        assert!(!mutations.is_empty());
        if cfg!(unix) {
            assert!(mutations
                .iter()
                .any(|item| item.file.ends_with("unix_only.rs")));
            assert!(!mutations
                .iter()
                .any(|item| item.file.ends_with("windows_only.rs")));
        } else if cfg!(windows) {
            assert!(mutations
                .iter()
                .any(|item| item.file.ends_with("windows_only.rs")));
            assert!(!mutations
                .iter()
                .any(|item| item.file.ends_with("unix_only.rs")));
        }
    }

    #[test]
    fn expression_position_include_contributes_mutations() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='mutation-expression-include-fixture'\nversion='0.1.0'\nedition='2021'\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            "pub const VALUE: bool = include!(\"value.rs\",);\n",
        )
        .unwrap();
        fs::write(dir.path().join("src/value.rs"), "true\n").unwrap();
        let mutations = collect_mutations(dir.path(), false, &[]).unwrap();
        assert!(mutations.iter().any(|mutation| {
            mutation.file.ends_with("src/value.rs")
                && mutation.original == "true"
                && mutation.replacement == "false"
        }));
    }

    #[test]
    fn cfg_disabled_struct_expression_field_is_not_mutated() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='mutation-field-value-cfg-fixture'\nversion='0.1.0'\nedition='2021'\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            "pub struct S { pub a: bool, #[cfg(any())] pub hidden: bool } pub fn make() -> S { S { a: true, #[cfg(any())] hidden: false } }\n",
        )
        .unwrap();
        let mutations = collect_mutations(dir.path(), false, &[]).unwrap();
        assert_eq!(
            mutations
                .iter()
                .filter(|mutation| matches!(mutation.original.as_str(), "true" | "false"))
                .count(),
            1
        );
        assert!(mutations.iter().any(|mutation| mutation.original == "true"));
    }

    #[test]
    fn timeout_is_not_killed() {
        let dir = tempdir().unwrap();
        let result = run_shell("sleep 1", dir.path(), Duration::from_millis(20)).unwrap();
        assert!(result.timed_out);
        assert_eq!(result.exit_code, None);
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_descendant_processes() {
        let dir = tempdir().unwrap();
        let marker = dir.path().join("leaked.txt");
        let command = "(sleep 0.2; printf leaked > leaked.txt) & wait";
        let result = run_shell(command, dir.path(), Duration::from_millis(20)).unwrap();
        assert!(result.timed_out);
        std::thread::sleep(Duration::from_millis(350));
        assert!(!marker.exists());
    }
}
