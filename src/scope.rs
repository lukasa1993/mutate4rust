use proc_macro2::TokenStream;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{Attribute, Expr, Lit, Meta, Token};
use walkdir::{DirEntry, WalkDir};

#[derive(Clone, Debug)]
pub(crate) struct CfgContext {
    names: HashSet<String>,
    values: HashMap<String, HashSet<String>>,
    features: HashSet<String>,
    include_tests: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ActiveFile {
    pub path: PathBuf,
    pub cfg: CfgContext,
}

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    manifest_path: PathBuf,
    targets: Vec<Target>,
}

#[derive(Deserialize)]
struct Target {
    name: String,
    kind: Vec<String>,
    src_path: PathBuf,
}

fn parse_meta_list(tokens: TokenStream) -> Option<Vec<Meta>> {
    Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(tokens)
        .ok()
        .map(|items| items.into_iter().collect())
}

fn literal_string(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Lit(value) => match &value.lit {
            Lit::Str(value) => Some(value.value()),
            _ => None,
        },
        _ => None,
    }
}

impl CfgContext {
    fn synthetic(include_tests: bool) -> Self {
        Self {
            names: HashSet::new(),
            values: HashMap::new(),
            features: HashSet::new(),
            include_tests,
        }
    }

    fn eval(&self, meta: &Meta) -> bool {
        match meta {
            Meta::Path(path) => {
                if path.is_ident("test") {
                    return self.include_tests;
                }
                path.get_ident()
                    .is_some_and(|name| self.names.contains(&name.to_string()))
            }
            Meta::NameValue(value) => {
                let Some(key) = value.path.get_ident().map(ToString::to_string) else {
                    return false;
                };
                let Some(value) = literal_string(&value.value) else {
                    return false;
                };
                if key == "feature" {
                    self.features.contains(&value)
                } else {
                    self.values
                        .get(&key)
                        .is_some_and(|values| values.contains(&value))
                }
            }
            Meta::List(list) if list.path.is_ident("all") => parse_meta_list(list.tokens.clone())
                .is_some_and(|items| items.iter().all(|item| self.eval(item))),
            Meta::List(list) if list.path.is_ident("any") => parse_meta_list(list.tokens.clone())
                .is_some_and(|items| items.iter().any(|item| self.eval(item))),
            Meta::List(list) if list.path.is_ident("not") => parse_meta_list(list.tokens.clone())
                .is_some_and(|items| items.len() == 1 && !self.eval(&items[0])),
            Meta::List(_) => false,
        }
    }

    fn meta_attribute_active(&self, meta: &Meta) -> bool {
        match meta {
            Meta::Path(path) if path.is_ident("test") => self.include_tests,
            Meta::List(list) if list.path.is_ident("cfg") => {
                syn::parse2::<Meta>(list.tokens.clone())
                    .ok()
                    .is_none_or(|predicate| self.eval(&predicate))
            }
            Meta::List(list) if list.path.is_ident("cfg_attr") => {
                let Some(items) = parse_meta_list(list.tokens.clone()) else {
                    return true;
                };
                let Some((predicate, nested)) = items.split_first() else {
                    return true;
                };
                !self.eval(predicate)
                    || nested
                        .iter()
                        .all(|attribute| self.meta_attribute_active(attribute))
            }
            _ => true,
        }
    }

    pub(crate) fn attrs_active(&self, attrs: &[Attribute]) -> bool {
        attrs.iter().all(|attribute| {
            if attribute.path().is_ident("test") {
                return self.include_tests;
            }
            match &attribute.meta {
                Meta::List(list) if list.path.is_ident("cfg") => {
                    syn::parse2::<Meta>(list.tokens.clone())
                        .ok()
                        .is_none_or(|predicate| self.eval(&predicate))
                }
                Meta::List(list) if list.path.is_ident("cfg_attr") => {
                    self.meta_attribute_active(&attribute.meta)
                }
                _ => true,
            }
        })
    }

    fn path_override(&self, attrs: &[Attribute]) -> Option<PathBuf> {
        fn from_meta(context: &CfgContext, meta: &Meta) -> Option<PathBuf> {
            match meta {
                Meta::NameValue(value) if value.path.is_ident("path") => {
                    literal_string(&value.value).map(PathBuf::from)
                }
                Meta::List(list) if list.path.is_ident("cfg_attr") => {
                    let items = parse_meta_list(list.tokens.clone())?;
                    let (predicate, nested) = items.split_first()?;
                    if context.eval(predicate) {
                        nested.iter().find_map(|meta| from_meta(context, meta))
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
        attrs
            .iter()
            .find_map(|attribute| from_meta(self, &attribute.meta))
    }
}

fn parse_cfg_output(text: &str, include_tests: bool) -> CfgContext {
    let mut context = CfgContext::synthetic(include_tests);
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Some((key, value)) = line.split_once('=') {
            let value = value.trim_matches('"').to_string();
            if key == "feature" {
                context.features.insert(value.clone());
            }
            context
                .values
                .entry(key.to_string())
                .or_default()
                .insert(value);
        } else {
            context.names.insert(line.to_string());
        }
    }
    context
}

fn cargo_cfg(
    root: &Path,
    package: &Package,
    target: &Target,
    include_tests: bool,
) -> Result<CfgContext, String> {
    let mut command = Command::new("cargo");
    command
        .arg("rustc")
        .arg("--manifest-path")
        .arg(&package.manifest_path)
        .arg("--all-features");
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

fn cargo_metadata(root: &Path) -> Result<Metadata, String> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(root)
        .output()
        .map_err(|error| format!("cannot run cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed with exit code {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("cannot decode cargo metadata: {error}"))
}

fn target_in_scope(kind: &[String], include_tests: bool) -> bool {
    if kind
        .iter()
        .any(|value| value == "custom-build" || value == "example" || value == "bench")
    {
        return false;
    }
    include_tests || !kind.iter().any(|value| value == "test")
}

fn module_directory(path: &Path, crate_root: bool) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if crate_root || path.file_name().and_then(|name| name.to_str()) == Some("mod.rs") {
        parent.to_path_buf()
    } else {
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        parent.join(stem)
    }
}

fn resolve_module(
    module: &syn::ItemMod,
    module_dir: &Path,
    context: &CfgContext,
) -> Result<PathBuf, String> {
    if let Some(relative) = context.path_override(&module.attrs) {
        let path = module_dir.join(relative);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "active #[path] module {} does not exist: {}",
            module.ident,
            path.display()
        ));
    }
    let direct = module_dir.join(format!("{}.rs", module.ident));
    let nested = module_dir.join(module.ident.to_string()).join("mod.rs");
    match (direct.is_file(), nested.is_file()) {
        (true, false) => Ok(direct),
        (false, true) => Ok(nested),
        (true, true) => Err(format!(
            "module {} is ambiguous: both {} and {} exist",
            module.ident,
            direct.display(),
            nested.display()
        )),
        (false, false) => Err(format!(
            "active module {} cannot be resolved below {}",
            module.ident,
            module_dir.display()
        )),
    }
}

fn walk_modules(
    items: &[syn::Item],
    module_dir: &Path,
    context: &CfgContext,
    visited: &mut HashSet<PathBuf>,
    output: &mut Vec<ActiveFile>,
) -> Result<(), String> {
    for item in items {
        let syn::Item::Mod(module) = item else {
            continue;
        };
        if !context.attrs_active(&module.attrs) {
            continue;
        }
        if let Some((_, nested)) = &module.content {
            let nested_dir = module_dir.join(module.ident.to_string());
            walk_modules(nested, &nested_dir, context, visited, output)?;
            continue;
        }
        let child = resolve_module(module, module_dir, context)?;
        visit_file(&child, false, context, visited, output)?;
    }
    Ok(())
}

fn visit_file(
    path: &Path,
    crate_root: bool,
    context: &CfgContext,
    visited: &mut HashSet<PathBuf>,
    output: &mut Vec<ActiveFile>,
) -> Result<(), String> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("cannot resolve Rust source {}: {error}", path.display()))?;
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
    output.push(ActiveFile {
        path: canonical.clone(),
        cfg: context.clone(),
    });
    let module_dir = module_directory(&canonical, crate_root);
    walk_modules(&syntax.items, &module_dir, context, visited, output)
}

fn ignored(entry: &DirEntry) -> bool {
    matches!(
        entry.file_name().to_str(),
        Some(".git" | "target" | "vendor" | "node_modules" | ".venv" | "venv" | "build" | "dist")
    )
}

fn fallback_files(root: &Path, include_tests: bool) -> Vec<ActiveFile> {
    let context = CfgContext::synthetic(include_tests);
    let mut files: Vec<_> = WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| !ignored(entry))
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("rs"))
        .filter(|path| {
            let relative = path.strip_prefix(root).unwrap_or(path);
            !relative.components().any(|part| {
                matches!(
                    part.as_os_str().to_str(),
                    Some("examples" | "benches" | "fuzz")
                )
            })
        })
        .filter(|path| path.file_name().and_then(|value| value.to_str()) != Some("build.rs"))
        .filter(|path| {
            include_tests
                || !path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(|name| name.ends_with("_test.rs"))
        })
        .filter(|path| {
            include_tests
                || !path
                    .strip_prefix(root)
                    .unwrap_or(path)
                    .components()
                    .any(|part| part.as_os_str() == "tests")
        })
        .map(|path| ActiveFile {
            path,
            cfg: context.clone(),
        })
        .collect();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files
}

pub(crate) fn discover(
    root: &Path,
    include_tests: bool,
    filters: &[String],
) -> Result<Vec<ActiveFile>, String> {
    if !root.join("Cargo.toml").is_file() {
        return Ok(fallback_files(root, include_tests));
    }
    let metadata = cargo_metadata(root)?;
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
    }
    output.retain(|file| {
        if filters.is_empty() {
            return true;
        }
        let relative = file
            .path
            .strip_prefix(root)
            .unwrap_or(&file.path)
            .to_string_lossy();
        filters.iter().any(|filter| relative.contains(filter))
    });
    output.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn cfg_evaluation_handles_target_and_feature_predicates() {
        let mut context = CfgContext::synthetic(false);
        context.names.insert("unix".into());
        context
            .values
            .entry("target_os".into())
            .or_default()
            .insert("linux".into());
        context.features.insert("extra".into());
        for text in [
            "unix",
            "target_os = \"linux\"",
            "feature = \"extra\"",
            "all(unix, feature = \"extra\")",
            "not(windows)",
        ] {
            let meta: Meta = syn::parse_str(text).unwrap();
            assert!(context.eval(&meta), "{text} should be active");
        }
    }

    #[test]
    fn cargo_cfg_includes_build_script_values_and_inner_file_cfg() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname='cfg-mutation-fixture'\nversion='0.1.0'\nedition='2021'\nbuild='build.rs'\n").unwrap();
        fs::write(dir.path().join("build.rs"), "fn main() { println!(\"cargo::rustc-check-cfg=cfg(tool_probe)\"); println!(\"cargo::rustc-cfg=tool_probe\"); }\n").unwrap();
        fs::write(dir.path().join("src/lib.rs"), "#[cfg(tool_probe)] mod active;\nmod inner_disabled;\n#[cfg(not(tool_probe))] mod impossible;\n").unwrap();
        fs::write(
            dir.path().join("src/active.rs"),
            "pub fn active() -> bool { true }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/inner_disabled.rs"),
            "#![cfg(not(tool_probe))]\npub fn disabled() -> bool { false }\n",
        )
        .unwrap();
        let files = discover(dir.path(), false, &[]).unwrap();
        let names: HashSet<_> = files
            .iter()
            .filter_map(|file| file.path.file_name().and_then(|name| name.to_str()))
            .collect();
        assert!(names.contains("lib.rs"));
        assert!(names.contains("active.rs"));
        assert!(!names.contains("inner_disabled.rs"));
        assert!(!names.contains("impossible.rs"));
    }

    #[test]
    fn cargo_scope_excludes_inactive_external_modules() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='scope-fixture'\nversion='0.1.0'\nedition='2021'\n[features]\nextra=[]\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            "#[cfg(unix)] mod unix_only;\n#[cfg(windows)] mod windows_only;\n#[cfg(feature=\"extra\")] mod feature_only;\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/unix_only.rs"),
            "pub fn unix_fn() { let _ = true; }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/windows_only.rs"),
            "pub fn windows_fn() { let _ = false; }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/feature_only.rs"),
            "pub fn feature_fn() { let _ = true; }\n",
        )
        .unwrap();
        let files = discover(dir.path(), false, &[]).unwrap();
        let names: HashSet<_> = files
            .iter()
            .filter_map(|file| file.path.file_name().and_then(|name| name.to_str()))
            .collect();
        assert!(names.contains("lib.rs"));
        assert!(names.contains("feature_only.rs"));
        if cfg!(unix) {
            assert!(names.contains("unix_only.rs"));
            assert!(!names.contains("windows_only.rs"));
        } else if cfg!(windows) {
            assert!(names.contains("windows_only.rs"));
            assert!(!names.contains("unix_only.rs"));
        }
    }
}
