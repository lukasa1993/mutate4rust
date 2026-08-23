use std::fs;

fn replace_once(text: &mut String, old: &str, new: &str, label: &str) {
    let start = text.find(old).unwrap_or_else(|| panic!("missing anchor: {label}"));
    assert!(text[start + old.len()..].find(old).is_none(), "duplicate anchor: {label}");
    text.replace_range(start..start + old.len(), new);
}

fn replace_between(text: &mut String, start: &str, end: &str, new: &str, label: &str) {
    let from = text.find(start).unwrap_or_else(|| panic!("missing start anchor: {label}"));
    let relative = text[from..].find(end).unwrap_or_else(|| panic!("missing end anchor: {label}"));
    text.replace_range(from..from + relative, new);
}

fn main() {
    let mut scope = fs::read_to_string("src/scope.rs").unwrap();
    if !scope.contains("fn include_literal(") {
        replace_once(&mut scope, "use syn::parse::Parser;", "use syn::parse::{ParseStream, Parser};", "scope parser import");
        replace_between(&mut scope, "fn static_include_path(", "fn item_attrs(", r####"fn include_literal(tokens: TokenStream) -> Option<LitStr> {
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

fn static_include_path(item: &syn::ItemMacro, source_dir: &Path) -> Option<PathBuf> {
    if !item.mac.path.is_ident("include") {
        return None;
    }
    let literal = include_literal(item.mac.tokens.clone())?;
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

"####, "scope include parser");
        replace_once(&mut scope, "fn static_include_is_part_of_active_source_graph()", "fn static_include_with_trailing_comma_is_part_of_active_source_graph()", "scope include test name");
        replace_once(&mut scope, r####""include!(\"shared.rs\");\n""####, r####""include!(\"shared.rs\",);\n""####, "scope include fixture");
    }
    fs::write("src/scope.rs", scope).unwrap();

    let mut library = fs::read_to_string("src/lib.rs").unwrap();
    if !library.contains("struct InactiveRangeVisitor") {
        replace_once(
            &mut library,
            "use syn::spanned::Spanned;\nuse syn::visit::{self, Visit};\nuse syn::{Attribute, BinOp, ExprBinary, ExprLit, Item, Lit};",
            "use syn::parse::{ParseStream, Parser};\nuse syn::spanned::Spanned;\nuse syn::visit::{self, Visit};\nuse syn::{Attribute, BinOp, Expr, ExprBinary, ExprLit, ForeignItem, ImplItem, Item, Lit, LitStr, Token, TraitItem};",
            "library imports",
        );
        replace_between(&mut library, "fn collect_inactive_ranges(", "fn replacement_for_binop", r####"fn impl_item_attrs(item: &ImplItem) -> &[Attribute] {
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

"####, "inactive syntax visitor");
        replace_once(
            &mut library,
            "    let mut excluded_ranges = Vec::new();\n    collect_inactive_ranges(&syntax.items, &active.cfg, &mut excluded_ranges);",
            "    let excluded_ranges = inactive_file_ranges(&syntax, &active.cfg);",
            "active mutation ranges",
        );
    }
    if !library.contains("fn enumerate_expression_mutations(") {
        let marker = "pub fn collect_mutations(";
        let index = library.find(marker).expect("missing collect_mutations");
        library.insert_str(index, r####"fn include_literal(tokens: TokenStream) -> Option<LitStr> {
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

"####);
        replace_between(&mut library, "pub fn collect_mutations(", "fn spawn_shell(", r####"pub fn collect_mutations(
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

"####, "expression-aware collection");
    }
    if !library.contains("fn expression_position_include_contributes_mutations()") {
        let marker = "    #[test]\n    fn timeout_is_not_killed()";
        let index = library.find(marker).expect("missing test insertion point");
        library.insert_str(index, r####"    #[test]
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

"####);
    }
    fs::write("src/lib.rs", library).unwrap();
}
