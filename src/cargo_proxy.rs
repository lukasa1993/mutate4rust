use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

const REAL_CARGO_ENV: &str = "RUST_QUALITY_REAL_CARGO";

pub struct Guard {
    previous_path: Option<OsString>,
    previous_real_cargo: Option<OsString>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        match &self.previous_path {
            Some(value) => env::set_var("PATH", value),
            None => env::remove_var("PATH"),
        }
        match &self.previous_real_cargo {
            Some(value) => env::set_var(REAL_CARGO_ENV, value),
            None => env::remove_var(REAL_CARGO_ENV),
        }
    }
}

pub fn feature_args(
    features: &[String],
    all_features: bool,
    no_default_features: bool,
) -> Vec<String> {
    if all_features {
        return vec!["--all-features".into()];
    }
    let mut args = Vec::new();
    if no_default_features {
        args.push("--no-default-features".into());
    }
    if !features.is_empty() {
        args.push("--features".into());
        args.push(features.join(","));
    }
    args
}

fn executable_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn find_on_path(name: &str) -> io::Result<PathBuf> {
    let path = env::var_os("PATH")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "PATH is not set"))?;
    let executable = executable_name(name);
    for directory in env::split_paths(&path) {
        let candidate = directory.join(&executable);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("cannot find {name} on PATH"),
    ))
}

fn proxy_source(extra: &[String]) -> String {
    let encoded = extra
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"use std::env;
use std::ffi::OsStr;
use std::process::{{Command, exit}};

const EXTRA: &[&str] = &[{encoded}];

fn accepts_feature_args(value: &OsStr) -> bool {{
    value.to_str().is_some_and(|name| matches!(
        name,
        "rustc" | "metadata" | "test" | "check" | "build" | "clippy" |
        "doc" | "run" | "bench" | "fix" | "llvm-cov"
    ))
}}

fn main() {{
    let Some(real_cargo) = env::var_os("{REAL_CARGO_ENV}") else {{
        eprintln!("cargo proxy: {REAL_CARGO_ENV} is not set");
        exit(1);
    }};
    let mut input = env::args_os().skip(1);
    let first = input.next();
    let mut command = Command::new(real_cargo);
    if let Some(subcommand) = first {{
        command.arg(&subcommand);
        if accepts_feature_args(&subcommand) {{
            command.args(EXTRA);
        }}
    }}
    command.args(input);
    match command.status() {{
        Ok(status) => exit(status.code().unwrap_or(1)),
        Err(error) => {{
            eprintln!("cargo proxy: cannot execute Cargo: {{error}}");
            exit(1);
        }}
    }}
}}
"#
    )
}

pub fn install(root: &Path, tool: &str, extra: &[String]) -> io::Result<Option<Guard>> {
    if extra.is_empty() {
        return Ok(None);
    }

    let real_cargo = env::var_os(REAL_CARGO_ENV)
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| find_on_path("cargo"))?;
    let rustc = find_on_path("rustc")?;
    let directory = root.join("target").join(tool).join("cargo-proxy");
    fs::create_dir_all(&directory)?;
    let source_path = directory.join("cargo_proxy.rs");
    let executable = directory.join(executable_name("cargo"));
    let temporary = directory.join(if cfg!(windows) {
        format!("cargo-{}.exe", std::process::id())
    } else {
        format!("cargo-{}", std::process::id())
    });
    fs::write(&source_path, proxy_source(extra))?;
    let status = Command::new(rustc)
        .arg("--edition=2021")
        .arg(&source_path)
        .arg("-O")
        .arg("-o")
        .arg(&temporary)
        .status()?;
    if !status.success() {
        let _ = fs::remove_file(&temporary);
        return Err(io::Error::other(format!(
            "rustc failed to build Cargo feature proxy: {status}"
        )));
    }
    if executable.exists() {
        fs::remove_file(&executable)?;
    }
    fs::rename(&temporary, &executable)?;

    let previous_path = env::var_os("PATH");
    let previous_real_cargo = env::var_os(REAL_CARGO_ENV);
    let mut paths = vec![directory];
    if let Some(value) = &previous_path {
        paths.extend(env::split_paths(value));
    }
    let joined = env::join_paths(paths).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot construct Cargo proxy PATH: {error}"),
        )
    })?;
    env::set_var(REAL_CARGO_ENV, real_cargo);
    env::set_var("PATH", joined);
    Ok(Some(Guard {
        previous_path,
        previous_real_cargo,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use tempfile::tempdir;

    fn compile(source: &str, output: &Path) {
        let dir = output.parent().unwrap();
        fs::create_dir_all(dir).unwrap();
        let input = dir.join(format!("source-{}.rs", std::process::id()));
        fs::write(&input, source).unwrap();
        let status = Command::new(find_on_path("rustc").unwrap())
            .arg("--edition=2021")
            .arg(&input)
            .arg("-o")
            .arg(output)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .status()
            .unwrap();
        assert!(status.success());
        fs::remove_file(input).unwrap();
    }

    #[test]
    fn feature_arguments_follow_cargo_rules() {
        assert_eq!(
            feature_args(&["alpha".into(), "beta".into()], false, true),
            vec!["--no-default-features", "--features", "alpha,beta"]
        );
        assert_eq!(
            feature_args(&["ignored".into()], true, false),
            vec!["--all-features"]
        );
        assert!(feature_args(&[], false, false).is_empty());
    }

    #[test]
    fn compiled_proxy_injects_only_supported_cargo_subcommands() {
        let dir = tempdir().unwrap();
        let fake = dir.path().join(executable_name("fake-cargo"));
        compile(
            r#"fn main() {
    println!("{}", std::env::args().skip(1).collect::<Vec<_>>().join("|"));
}"#,
            &fake,
        );
        let proxy = dir.path().join(executable_name("cargo-proxy"));
        compile(
            &proxy_source(&["--features".into(), "extra".into()]),
            &proxy,
        );

        let output = Command::new(&proxy)
            .env(REAL_CARGO_ENV, &fake)
            .args(["rustc", "--manifest-path", "Cargo.toml"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            "rustc|--features|extra|--manifest-path|Cargo.toml"
        );

        let output = Command::new(&proxy)
            .env(REAL_CARGO_ENV, &fake)
            .arg("--version")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            "--version"
        );
    }
}
