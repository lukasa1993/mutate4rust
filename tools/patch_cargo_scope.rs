use std::fs;

fn replace_once(text: &mut String, old: &str, new: &str, label: &str) {
    let start = text.find(old).unwrap_or_else(|| panic!("missing anchor: {label}"));
    assert!(
        text[start + old.len()..].find(old).is_none(),
        "duplicate anchor: {label}"
    );
    text.replace_range(start..start + old.len(), new);
}

fn main() {
    let mut cargo = fs::read_to_string("Cargo.toml").unwrap();
    replace_once(
        &mut cargo,
        "version = \"2.0.2\"",
        "version = \"2.0.3\"",
        "package version",
    );
    fs::write("Cargo.toml", cargo).unwrap();

    let mut source = fs::read_to_string("src/lib.rs").unwrap();
    replace_once(
        &mut source,
        "use base64::engine::general_purpose::STANDARD as BASE64;",
        "mod scope;\n\nuse base64::engine::general_purpose::STANDARD as BASE64;",
        "scope module",
    );

    let anchor = r###"fn replacement_for_binop(op: &BinOp) -> Option<&'static str> {"###;
    let helper = r###"fn collect_inactive_ranges(
    items: &[Item],
    cfg: &scope::CfgContext,
    output: &mut Vec<Range<usize>>,
) {
    for item in items {
        if !cfg.attrs_active(item_attrs(item)) {
            output.push(item.span().byte_range());
            continue;
        }
        match item {
            Item::Mod(module) => {
                if let Some((_, nested)) = &module.content {
                    collect_inactive_ranges(nested, cfg, output);
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
                    if !cfg.attrs_active(attrs) {
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
                    if !cfg.attrs_active(attrs) {
                        output.push(member.span().byte_range());
                    }
                }
            }
            _ => {}
        }
    }
}

"###;
    let index = source.find(anchor).expect("missing replacement function anchor");
    source.insert_str(index, helper);

    let collect_old = r###"pub fn collect_mutations(
    root: &Path,
    include_tests: bool,
    filters: &[String],
) -> Result<Vec<Mutation>, Error> {
    let mut mutations = Vec::new();
    for path in discover_files(root, include_tests, filters) {
        let next = mutations.len() + 1;
        mutations.extend(enumerate_mutations_with_tests(
            &path,
            root,
            next,
            include_tests,
        )?);
    }
    Ok(mutations)
}"###;
    let collect_new = r###"fn enumerate_active_mutations(
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
    let mut excluded_ranges = Vec::new();
    collect_inactive_ranges(&syntax.items, &active.cfg, &mut excluded_ranges);
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

pub fn collect_mutations(
    root: &Path,
    include_tests: bool,
    filters: &[String],
) -> Result<Vec<Mutation>, Error> {
    let active = scope::discover(root, include_tests, filters).map_err(Error::Mutation)?;
    let mut mutations = Vec::new();
    for file in &active {
        let next = mutations.len() + 1;
        mutations.extend(enumerate_active_mutations(file, root, next)?);
    }
    Ok(mutations)
}"###;
    replace_once(&mut source, collect_old, collect_new, "repository mutation collection");

    let test_anchor = r###"    #[test]
    fn timeout_is_not_killed() {"###;
    let test = r###"    #[test]
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
            assert!(mutations.iter().any(|item| item.file.ends_with("unix_only.rs")));
            assert!(!mutations.iter().any(|item| item.file.ends_with("windows_only.rs")));
        } else if cfg!(windows) {
            assert!(mutations.iter().any(|item| item.file.ends_with("windows_only.rs")));
            assert!(!mutations.iter().any(|item| item.file.ends_with("unix_only.rs")));
        }
    }

"###;
    let index = source.find(test_anchor).expect("missing test anchor");
    source.insert_str(index, test);

    fs::write("src/lib.rs", source).unwrap();
}
