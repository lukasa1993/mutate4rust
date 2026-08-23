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
    let mut cargo = fs::read_to_string("Cargo.toml").unwrap();
    replace_once(&mut cargo, "version = \"2.0.3\"", "version = \"2.0.4\"", "version");
    fs::write("Cargo.toml", cargo).unwrap();

    let mut scope = fs::read_to_string("src/scope.rs").unwrap();
    replace_once(
        &mut scope,
        r###"struct Package {
    id: String,
    features: HashMap<String, Vec<String>>,
    targets: Vec<Target>,
}

#[derive(Deserialize)]
struct Target {
    kind: Vec<String>,
    src_path: PathBuf,
}"###,
        r###"struct Package {
    id: String,
    manifest_path: PathBuf,
    targets: Vec<Target>,
}

#[derive(Deserialize)]
struct Target {
    name: String,
    kind: Vec<String>,
    src_path: PathBuf,
}"###,
        "metadata structs",
    );
    replace_once(
        &mut scope,
        r###"    fn with_features(&self, features: impl IntoIterator<Item = String>) -> Self {
        let mut value = self.clone();
        value.features = features.into_iter().collect();
        value
    }

"###,
        "",
        "manual features",
    );

    let cfg_code = r###"fn parse_cfg_output(text: &str, include_tests: bool) -> CfgContext {
    let mut context = CfgContext::synthetic(include_tests);
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Some((key, value)) = line.split_once('=') {
            let value = value.trim_matches('"').to_string();
            if key == "feature" {
                context.features.insert(value.clone());
            }
            context.values.entry(key.to_string()).or_default().insert(value);
        } else {
            context.names.insert(line.to_string());
        }
    }
    context
}

fn cargo_cfg(root: &Path, package: &Package, target: &Target, include_tests: bool) -> Result<CfgContext, String> {
    let mut command = Command::new("cargo");
    command.arg("rustc").arg("--manifest-path").arg(&package.manifest_path).arg("--all-features");
    if target.kind.iter().any(|kind| kind == "bin") {
        command.arg("--bin").arg(&target.name);
    } else {
        command.arg("--lib");
    }
    let output = command
        .args(["--", "--print", "cfg"])
        .current_dir(root)
        .output()
        .map_err(|error| format!("cannot run cargo rustc -- --print cfg: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo rustc cfg discovery failed for {} with exit code {:?}: {}",
            package.manifest_path.display(),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|error| format!("cargo rustc cfg output is invalid UTF-8: {error}"))?;
    Ok(parse_cfg_output(&text, include_tests))
}

"###;
    replace_between(&mut scope, "fn rustc_cfg(", "fn cargo_metadata(", cfg_code, "cfg discovery");

    replace_once(
        &mut scope,
        r###"    let syntax = syn::parse_file(&source)
        .map_err(|error| format!("Rust parse error in {}: {error}", canonical.display()))?;
    output.push(ActiveFile {
        path: canonical.clone(),
        cfg: context.clone(),
    });"###,
        r###"    let syntax = syn::parse_file(&source)
        .map_err(|error| format!("Rust parse error in {}: {error}", canonical.display()))?;
    if !context.attrs_active(&syntax.attrs) {
        return Ok(());
    }
    output.push(ActiveFile {
        path: canonical.clone(),
        cfg: context.clone(),
    });"###,
        "file cfg",
    );

    replace_once(
        &mut scope,
        r###"    let metadata = cargo_metadata(root)?;
    let base = rustc_cfg(root, include_tests)?;
    let workspace: HashSet<_> = metadata.workspace_members.into_iter().collect();
    let mut output = Vec::new();
    let mut visited = HashSet::new();
    for package in metadata
        .packages
        .into_iter()
        .filter(|package| workspace.contains(&package.id))
    {
        let context = base.with_features(package.features.keys().cloned());
        for target in package
            .targets
            .into_iter()
            .filter(|target| target_in_scope(&target.kind, include_tests))
        {
            visit_file(&target.src_path, true, &context, &mut visited, &mut output)?;
        }
    }"###,
        r###"    let metadata = cargo_metadata(root)?;
    let workspace: HashSet<_> = metadata.workspace_members.into_iter().collect();
    let mut output = Vec::new();
    let mut visited = HashSet::new();
    for package in metadata
        .packages
        .into_iter()
        .filter(|package| workspace.contains(&package.id))
    {
        let targets: Vec<_> = package
            .targets
            .iter()
            .filter(|target| target_in_scope(&target.kind, include_tests))
            .collect();
        let Some(cfg_target) = targets.first().copied() else {
            continue;
        };
        let context = cargo_cfg(root, &package, cfg_target, include_tests)?;
        for target in targets {
            visit_file(&target.src_path, true, &context, &mut visited, &mut output)?;
        }
    }"###,
        "scope discovery",
    );

    let anchor = "    #[test]\n    fn cargo_scope_excludes_inactive_external_modules() {";
    let test = r###"    #[test]
    fn cargo_cfg_includes_build_script_values_and_inner_file_cfg() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname='cfg-mutation-fixture'\nversion='0.1.0'\nedition='2021'\nbuild='build.rs'\n").unwrap();
        fs::write(dir.path().join("build.rs"), "fn main() { println!(\"cargo::rustc-check-cfg=cfg(tool_probe)\"); println!(\"cargo::rustc-cfg=tool_probe\"); }\n").unwrap();
        fs::write(dir.path().join("src/lib.rs"), "#[cfg(tool_probe)] mod active;\nmod inner_disabled;\n#[cfg(not(tool_probe))] mod impossible;\n").unwrap();
        fs::write(dir.path().join("src/active.rs"), "pub fn active() -> bool { true }\n").unwrap();
        fs::write(dir.path().join("src/inner_disabled.rs"), "#![cfg(not(tool_probe))]\npub fn disabled() -> bool { false }\n").unwrap();
        let files = discover(dir.path(), false, &[]).unwrap();
        let names: HashSet<_> = files.iter().filter_map(|file| file.path.file_name().and_then(|name| name.to_str())).collect();
        assert!(names.contains("lib.rs"));
        assert!(names.contains("active.rs"));
        assert!(!names.contains("inner_disabled.rs"));
        assert!(!names.contains("impossible.rs"));
    }

"###;
    let index = scope.find(anchor).expect("missing test anchor");
    scope.insert_str(index, test);
    fs::write("src/scope.rs", scope).unwrap();
}
