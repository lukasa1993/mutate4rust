use clap::Parser;
use mutate4rust::{
    collect_mutations, recover_active, run_mutations, run_shell, Error, MutationResult, Status,
    VERSION,
};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

const DEFAULT_TEST: &str = "cargo test --workspace --all-features";
const DEFAULT_VALIDATE: &str = "cargo check --workspace --all-targets --all-features";

#[derive(Parser, Debug)]
#[command(name = "mutate4rust", version = VERSION, about = "Native syntax-aware mutation testing for Rust")]
struct Args {
    #[arg(value_name = "PATH_FRAGMENT")]
    filters: Vec<String>,
    #[arg(long, default_value = ".")]
    root: PathBuf,
    #[arg(long, default_value = DEFAULT_TEST)]
    test_command: String,
    #[arg(long, default_value = DEFAULT_VALIDATE)]
    validate_command: String,
    #[arg(long, default_value_t = 120)]
    timeout: u64,
    #[arg(long)]
    max_mutants: Option<usize>,
    #[arg(long)]
    list: bool,
    #[arg(long)]
    skip_baseline: bool,
    #[arg(long)]
    no_validate: bool,
    #[arg(long)]
    include_tests: bool,
    #[arg(long, default_value = "target/mutation/results.json")]
    manifest: PathBuf,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    allow_survivors: bool,
    #[arg(long)]
    allow_compile_errors: bool,
}

#[derive(Serialize)]
struct Report<'a> {
    schema_version: u8,
    tool: &'static str,
    version: &'static str,
    root: String,
    summary: Summary,
    mutants: &'a [MutationResult],
}

#[derive(Serialize, Default)]
struct Summary {
    total: usize,
    killed: usize,
    survived: usize,
    timeout: usize,
    invalid: usize,
    compile_error: usize,
}

fn resolve(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn summary(results: &[MutationResult]) -> Summary {
    let mut value = Summary {
        total: results.len(),
        ..Summary::default()
    };
    for result in results {
        match result.status {
            Status::Killed => value.killed += 1,
            Status::Survived => value.survived += 1,
            Status::Timeout => value.timeout += 1,
            Status::Invalid => value.invalid += 1,
            Status::CompileError => value.compile_error += 1,
        }
    }
    value
}

fn require_successful_baseline(
    label: &str,
    command: &str,
    root: &Path,
    timeout: Duration,
) -> Result<(), Error> {
    let result = run_shell(command, root, timeout)?;
    if result.timed_out {
        return Err(Error::Mutation(format!(
            "baseline {label} command timed out"
        )));
    }
    if result.exit_code != Some(0) {
        return Err(Error::Mutation(format!(
            "baseline {label} failed with exit code {:?}",
            result.exit_code
        )));
    }
    Ok(())
}

fn run() -> Result<u8, Error> {
    let args = Args::parse();
    let root = args.root.canonicalize()?;
    recover_active(&root)?;
    let mutations = collect_mutations(&root, args.include_tests, &args.filters)?;
    if args.list {
        if args.json {
            println!("{}", serde_json::to_string_pretty(&mutations)?);
        } else {
            for item in &mutations {
                println!(
                    "{}\t{}:{}:{}\t{} -> {}",
                    item.id, item.file, item.line, item.column, item.original, item.replacement
                );
            }
        }
        return Ok(0);
    }
    if mutations.is_empty() {
        return Err(Error::Mutation("no mutation sites were discovered".into()));
    }
    let timeout = Duration::from_secs(args.timeout);
    let validate = if args.no_validate {
        None
    } else {
        Some(args.validate_command.as_str())
    };
    if !args.skip_baseline {
        if let Some(command) = validate {
            require_successful_baseline("validation", command, &root, timeout)?;
        }
        require_successful_baseline("test", &args.test_command, &root, timeout)?;
    }
    let results = run_mutations(
        &root,
        &mutations,
        &args.test_command,
        validate,
        timeout,
        args.max_mutants,
    )?;
    let totals = summary(&results);
    let report = Report {
        schema_version: 1,
        tool: "mutate4rust",
        version: VERSION,
        root: root.to_string_lossy().to_string(),
        summary: totals,
        mutants: &results,
    };
    let manifest = resolve(&root, &args.manifest);
    if let Some(parent) = manifest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        &manifest,
        format!("{}\n", serde_json::to_string_pretty(&report)?),
    )?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Mutation Report\n===============");
        println!("Total: {}", report.summary.total);
        println!("Killed: {}", report.summary.killed);
        println!("Survived: {}", report.summary.survived);
        println!("Timeout: {}", report.summary.timeout);
        println!("Invalid: {}", report.summary.invalid);
        println!("Compile error: {}", report.summary.compile_error);
        for result in results
            .iter()
            .filter(|item| item.status == Status::Survived)
        {
            println!(
                "SURVIVED {}:{}:{} {} -> {}",
                result.mutation.file,
                result.mutation.line,
                result.mutation.column,
                result.mutation.original,
                result.mutation.replacement
            );
        }
    }
    let infra = report.summary.timeout + report.summary.invalid;
    if infra > 0 || (report.summary.compile_error > 0 && !args.allow_compile_errors) {
        return Ok(1);
    }
    if report.summary.survived > 0 && !args.allow_survivors {
        return Ok(2);
    }
    Ok(0)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("mutate4rust: {error}");
            ExitCode::from(1)
        }
    }
}
