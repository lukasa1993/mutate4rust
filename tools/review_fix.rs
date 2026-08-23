use std::fs;

fn replace_once(text: &mut String, old: &str, new: &str, label: &str) {
    let start = text
        .find(old)
        .unwrap_or_else(|| panic!("missing anchor: {}", label));
    assert!(
        text[start + old.len()..].find(old).is_none(),
        "duplicate anchor: {}",
        label
    );
    text.replace_range(start..start + old.len(), new);
}

fn replace_between(text: &mut String, start: &str, end: &str, new: &str, label: &str) {
    let from = text
        .find(start)
        .unwrap_or_else(|| panic!("missing start anchor: {}", label));
    let relative = text[from..]
        .find(end)
        .unwrap_or_else(|| panic!("missing end anchor: {}", label));
    text.replace_range(from..from + relative, new);
}

fn insert_before(text: &mut String, marker: &str, addition: &str, label: &str) {
    let index = text
        .find(marker)
        .unwrap_or_else(|| panic!("missing insertion anchor: {}", label));
    text.insert_str(index, addition);
}

fn patch_library() {
    let mut source = fs::read_to_string("src/lib.rs").unwrap();

    replace_once(
        &mut source,
        "    pub replacement: String,\n    #[serde(skip)]\n    pub start: usize,",
        "    pub replacement: String,\n    #[serde(default)]\n    pub expression_fragment: bool,\n    #[serde(default)]\n    pub allow_extensionless: bool,\n    #[serde(skip)]\n    pub start: usize,",
        "mutation source metadata",
    );

    replace_once(
        &mut source,
        "            replacement: replacement.to_string(),\n            start: range.start,",
        "            replacement: replacement.to_string(),\n            expression_fragment: false,\n            allow_extensionless: false,\n            start: range.start,",
        "mutation defaults",
    );

    let active_function = r###"fn enumerate_active_mutations(
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
    let allow_extensionless = active.path.extension().and_then(|value| value.to_str()) != Some("rs");
    for (offset, item) in visitor.candidates.iter_mut().enumerate() {
        item.id = start_id + offset;
        item.allow_extensionless = allow_extensionless;
    }
    Ok(visitor.candidates)
}

"###;
    replace_between(
        &mut source,
        "fn enumerate_active_mutations(",
        "fn include_literal(",
        active_function,
        "active mutation enumeration",
    );

    let expression_function = r###"fn enumerate_expression_mutations(
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
        item.expression_fragment = true;
        item.allow_extensionless = true;
    }
    let includes = expression_includes_from_expr(&syntax, &canonical, cfg);
    Ok((visitor.candidates, includes))
}

fn path_matches_filters(root: &Path, path: &Path, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }
    let relative = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    filters.iter().any(|filter| relative.contains(filter))
}

pub fn collect_mutations(
    root: &Path,
    include_tests: bool,
    filters: &[String],
) -> Result<Vec<Mutation>, Error> {
    let active = scope::discover(root, include_tests, &[]).map_err(Error::Mutation)?;
    let mut mutations = Vec::new();
    let mut queue = VecDeque::new();
    for file in &active {
        let source = fs::read_to_string(&file.path)?;
        let syntax = syn::parse_file(&source).map_err(|source_error| Error::Parse {
            path: file.path.clone(),
            source: source_error,
        })?;
        if path_matches_filters(root, &file.path, filters) {
            let next = mutations.len() + 1;
            mutations.extend(enumerate_active_mutations(file, root, next)?);
        }
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
        if path_matches_filters(root, &canonical, filters) {
            mutations.extend(found);
        }
        for path in nested {
            queue.push_back((path, cfg.clone()));
        }
    }
    for (index, mutation) in mutations.iter_mut().enumerate() {
        mutation.id = index + 1;
    }
    Ok(mutations)
}

"###;
    replace_between(
        &mut source,
        "fn enumerate_expression_mutations(",
        "fn spawn_shell(",
        expression_function,
        "expression mutation collection",
    );

    replace_once(
        &mut source,
        "struct JournalRecord {\n    file: String,\n    content: String,\n}",
        "struct JournalRecord {\n    file: String,\n    content: String,\n    #[serde(default)]\n    allow_extensionless: bool,\n}",
        "journal source metadata",
    );

    replace_once(
        &mut source,
        "fn resolve_source_path(root: &Path, file: &str) -> Result<PathBuf, Error> {",
        "fn resolve_source_path(\n    root: &Path,\n    file: &str,\n    allow_extensionless: bool,\n) -> Result<PathBuf, Error> {",
        "source resolver signature",
    );

    replace_once(
        &mut source,
        "        || unsafe_component\n        || relative.extension().and_then(|value| value.to_str()) != Some(\"rs\")",
        "        || unsafe_component\n        || (!allow_extensionless\n            && relative.extension().and_then(|value| value.to_str()) != Some(\"rs\"))",
        "source extension policy",
    );

    replace_once(
        &mut source,
        "    let path = resolve_source_path(root, &record.file)?;",
        "    let path = resolve_source_path(root, &record.file, record.allow_extensionless)?;",
        "journal recovery path",
    );

    replace_once(
        &mut source,
        "        let path = resolve_source_path(root, &mutation.file)?;",
        "        let path = resolve_source_path(root, &mutation.file, mutation.allow_extensionless)?;",
        "guard source path",
    );

    replace_once(
        &mut source,
        "            file: mutation.file.clone(),\n            content: BASE64.encode(&original),",
        "            file: mutation.file.clone(),\n            content: BASE64.encode(&original),\n            allow_extensionless: mutation.allow_extensionless,",
        "journal record source metadata",
    );

    replace_once(
        &mut source,
        "fn source_parses(path: &Path) -> bool {\n    fs::read_to_string(path)\n        .ok()\n        .and_then(|source| syn::parse_file(&source).ok())\n        .is_some()\n}",
        "fn source_parses(path: &Path, expression_fragment: bool) -> bool {\n    let Ok(source) = fs::read_to_string(path) else {\n        return false;\n    };\n    if expression_fragment {\n        syn::parse_str::<Expr>(&source).is_ok()\n    } else {\n        syn::parse_file(&source).is_ok()\n    }\n}",
        "fragment-aware source parser",
    );

    replace_once(
        &mut source,
        "        if !source_parses(&guard.path) {",
        "        if !source_parses(&guard.path, mutation.expression_fragment) {",
        "fragment-aware mutation validation",
    );

    replace_once(
        &mut source,
        "            file: format!(\"../{outside_name}\"),\n            content: BASE64.encode(b\"overwritten\\n\"),",
        "            file: format!(\"../{outside_name}\"),\n            content: BASE64.encode(b\"overwritten\\n\"),\n            allow_extensionless: false,",
        "parent escape journal fixture",
    );

    replace_once(
        &mut source,
        "            file: \"linked.rs\".into(),\n            content: BASE64.encode(b\"overwritten\\n\"),",
        "            file: \"linked.rs\".into(),\n            content: BASE64.encode(b\"overwritten\\n\"),\n            allow_extensionless: false,",
        "symlink escape journal fixture",
    );

    let expression_tests = r###"    #[test]
    fn expression_position_include_mutation_is_executed_as_an_expression() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='mutation-expression-include-fixture'\nversion='0.1.0'\nedition='2021'\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            "pub const VALUE: bool = include!(\"value.rs\",);\n#[cfg(test)] mod tests { use super::VALUE; #[test] fn value_is_true() { assert!(VALUE); } }\n",
        )
        .unwrap();
        fs::write(dir.path().join("src/value.rs"), "true\n").unwrap();
        let mutations = collect_mutations(dir.path(), false, &[]).unwrap();
        let mutation = mutations
            .into_iter()
            .find(|mutation| mutation.file.ends_with("src/value.rs"))
            .expect("expression mutation");
        assert!(mutation.expression_fragment);
        let results = run_mutations(
            dir.path(),
            &[mutation],
            "cargo test --quiet",
            Some("cargo check --quiet"),
            Duration::from_secs(30),
            None,
        )
        .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, Status::Killed);
        assert_eq!(fs::read_to_string(dir.path().join("src/value.rs")).unwrap(), "true\n");
    }

    #[test]
    fn expression_include_can_be_selected_by_its_own_path_filter() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='mutation-filtered-include-fixture'\nversion='0.1.0'\nedition='2021'\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            "pub const VALUE: bool = include!(\"value.rs\");\n",
        )
        .unwrap();
        fs::write(dir.path().join("src/value.rs"), "true\n").unwrap();
        let mutations = collect_mutations(
            dir.path(),
            false,
            &["src/value.rs".to_string()],
        )
        .unwrap();
        assert_eq!(mutations.len(), 1);
        assert!(mutations[0].file.ends_with("src/value.rs"));
    }

    #[test]
    fn qualified_extensionless_item_include_is_mutatable() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='mutation-extensionless-include-fixture'\nversion='0.1.0'\nedition='2021'\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            "core::include!(\"shared\",);\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/shared"),
            "pub fn shared() -> bool { true }\n",
        )
        .unwrap();
        let mutations = collect_mutations(dir.path(), false, &[]).unwrap();
        let mutation = mutations
            .iter()
            .find(|mutation| mutation.file.ends_with("src/shared"))
            .expect("extensionless included mutation");
        assert!(mutation.allow_extensionless);
        assert_eq!(mutation.original, "true");
    }

"###;
    replace_between(
        &mut source,
        "    #[test]\n    fn expression_position_include_contributes_mutations()",
        "    #[test]\n    fn cfg_disabled_struct_expression_field_is_not_mutated()",
        expression_tests,
        "expression include regression tests",
    );

    fs::write("src/lib.rs", source).unwrap();
}

fn patch_scope() {
    let mut source = fs::read_to_string("src/scope.rs").unwrap();

    replace_once(
        &mut source,
        "use std::path::{Path, PathBuf};",
        "use std::path::{Component, Path, PathBuf};",
        "scope path imports",
    );

    let include_function = r###"fn built_in_include(path: &syn::Path) -> bool {
    let segments: Vec<_> = path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    matches!(segments.as_slice(), [name] if name == "include")
        || matches!(segments.as_slice(), [prefix, name]
            if matches!(prefix.as_str(), "std" | "core") && name == "include")
}

fn static_include_path(item: &syn::ItemMacro, source_dir: &Path) -> Option<PathBuf> {
    if !built_in_include(&item.mac.path) {
        return None;
    }
    let literal = include_literal(item.mac.tokens.clone())?;
    let path = PathBuf::from(literal.value());
    Some(if path.is_absolute() {
        path
    } else {
        source_dir.join(path)
    })
}

"###;
    replace_between(
        &mut source,
        "fn static_include_path(",
        "fn item_attrs(",
        include_function,
        "scope include parser",
    );

    let traversal = r###"fn mutatable_source_path(root: &Path, path: &Path) -> Result<Option<PathBuf>, String> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("cannot resolve Rust source {}: {error}", path.display()))?;
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve project root {}: {error}", root.display()))?;
    let Ok(relative) = canonical.strip_prefix(&canonical_root) else {
        return Ok(None);
    };
    if relative.components().any(|component| {
        matches!(
            component,
            Component::Normal(value)
                if matches!(value.to_str(), Some(".git" | "target" | "vendor" | "build" | "dist"))
        )
    }) {
        return Ok(None);
    }
    Ok(Some(canonical))
}

fn walk_items(
    root: &Path,
    items: &[syn::Item],
    module_dir: &Path,
    source_dir: &Path,
    context: &SingleCfgContext,
    visited: &mut HashSet<PathBuf>,
    output: &mut Vec<(PathBuf, SingleCfgContext)>,
) -> Result<(), String> {
    for item in items {
        if !context.attrs_active(item_attrs(item)) {
            continue;
        }
        match item {
            syn::Item::Mod(module) => {
                if let Some((_, nested)) = &module.content {
                    let nested_dir = module_dir.join(module.ident.to_string());
                    walk_items(
                        root,
                        nested,
                        &nested_dir,
                        source_dir,
                        context,
                        visited,
                        output,
                    )?;
                } else {
                    let resolved = resolve_module(module, module_dir, context)?;
                    visit_file(
                        root,
                        &resolved.path,
                        &resolved.descendant_dir,
                        context,
                        visited,
                        output,
                    )?;
                }
            }
            syn::Item::Macro(item_macro) => {
                if let Some(path) = static_include_path(item_macro, source_dir) {
                    if path.is_file() {
                        let include_dir = path.parent().unwrap_or(source_dir).to_path_buf();
                        visit_file(root, &path, &include_dir, context, visited, output)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn item_attrs(item: &syn::Item) -> &[Attribute] {
    match item {
        syn::Item::Const(value) => &value.attrs,
        syn::Item::Enum(value) => &value.attrs,
        syn::Item::ExternCrate(value) => &value.attrs,
        syn::Item::Fn(value) => &value.attrs,
        syn::Item::ForeignMod(value) => &value.attrs,
        syn::Item::Impl(value) => &value.attrs,
        syn::Item::Macro(value) => &value.attrs,
        syn::Item::Mod(value) => &value.attrs,
        syn::Item::Static(value) => &value.attrs,
        syn::Item::Struct(value) => &value.attrs,
        syn::Item::Trait(value) => &value.attrs,
        syn::Item::TraitAlias(value) => &value.attrs,
        syn::Item::Type(value) => &value.attrs,
        syn::Item::Union(value) => &value.attrs,
        syn::Item::Use(value) => &value.attrs,
        _ => &[],
    }
}

fn visit_file(
    root: &Path,
    path: &Path,
    module_dir: &Path,
    context: &SingleCfgContext,
    visited: &mut HashSet<PathBuf>,
    output: &mut Vec<(PathBuf, SingleCfgContext)>,
) -> Result<(), String> {
    let lexical_source_dir = path.parent().unwrap_or(module_dir).to_path_buf();
    let Some(canonical) = mutatable_source_path(root, path)? else {
        return Ok(());
    };
    if !visited.insert(canonical.clone()) {
        return Ok(());
    }
    let source = fs::read_to_string(&canonical)
        .map_err(|error| format!("cannot read Rust source {}: {error}", canonical.display()))?;
    let syntax = syn::parse_file(&source)
        .map_err(|error| format!("Rust parse error in {}: {error}", canonical.display()))?;
    if !context.attrs_active(&syntax.attrs) {
        return Ok(());
    }
    output.push((canonical.clone(), context.clone()));
    walk_items(
        root,
        &syntax.items,
        module_dir,
        &lexical_source_dir,
        context,
        visited,
        output,
    )
}

"###;
    replace_between(
        &mut source,
        "fn walk_items(",
        "fn ignored(",
        traversal,
        "safe source traversal",
    );

    replace_once(
        &mut source,
        "            visit_file(\n                &target.src_path,",
        "            visit_file(\n                root,\n                &target.src_path,",
        "root source traversal",
    );

    let tests = r###"    #[test]
    fn item_include_outside_project_root_is_not_mutated() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='mutation-outside-include-fixture'\nversion='0.1.0'\nedition='2021'\n",
        )
        .unwrap();
        let shared = outside.path().join("shared.rs");
        fs::write(&shared, "pub fn outside() -> bool { true }\n").unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            format!("include!({:?});\n", shared.to_string_lossy().as_ref()),
        )
        .unwrap();
        let files = discover(dir.path(), false, &[]).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].path.ends_with("src/lib.rs"));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_module_include_uses_lexical_source_directory() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::create_dir_all(dir.path().join("shared")).unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='mutation-symlink-include-fixture'\nversion='0.1.0'\nedition='2021'\n",
        )
        .unwrap();
        fs::write(dir.path().join("src/lib.rs"), "mod foo;\n").unwrap();
        fs::write(
            dir.path().join("shared/foo.rs"),
            "include!(\"part.rs\");\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/part.rs"),
            "pub fn lexical() -> bool { true }\n",
        )
        .unwrap();
        symlink("../shared/foo.rs", dir.path().join("src/foo.rs")).unwrap();
        let files = discover(dir.path(), false, &[]).unwrap();
        assert!(files.iter().any(|file| file.path.ends_with("src/part.rs")));
    }

"###;
    insert_before(
        &mut source,
        "    #[test]\n    fn static_include_with_trailing_comma_is_part_of_active_source_graph()",
        tests,
        "scope safety tests",
    );

    fs::write("src/scope.rs", source).unwrap();
}

fn main() {
    patch_library();
    patch_scope();
}
