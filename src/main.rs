mod cargo_proxy;

use clap::Parser;
use mutate4rust::{
    collect_mutations, recover_active, run_mutations, run_shell, Error, MutationResult, Status,
    VERSION,
};
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

const DEFAULT_TEST: &str = "cargo test --workspace";
const DEFAULT_VALIDATE: &str = "cargo check --workspace";
const RUN_LOCK: &str = "target/mutation/run.lock";

#[derive(Parser, Debug)]
#[command(name = "mutate4rust", version = VERSION, about = "Native syntax-aware mutation testing for Rust")]
struct Args {
    #[arg(value_name = "PATH_FRAGMENT")]
    filters: Vec<String>,
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Cargo features to enable for mutation scope and built-in Cargo commands.
    #[arg(long, value_delimiter = ',', conflicts_with = "all_features")]
    features: Vec<String>,
    /// Disable Cargo default features. May be combined with --features.
    #[arg(long, conflicts_with = "all_features")]
    no_default_features: bool,
    /// Enable every Cargo feature. Fails normally if the project forbids that combination.
    #[arg(long)]
    all_features: bool,
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

struct RunLock {
    file: File,
}

impl RunLock {
    fn acquire(root: &Path) -> Result<Self, Error> {
        let path = root.join(RUN_LOCK);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)?;
        if !try_lock_file(&file)? {
            return Err(Error::Mutation(format!(
                "another mutate4rust process holds {}",
                path.display()
            )));
        }
        Ok(Self { file })
    }
}

impl Drop for RunLock {
    fn drop(&mut self) {
        unlock_file(&self.file);
    }
}

#[cfg(unix)]
fn try_lock_file(file: &File) -> io::Result<bool> {
    use std::os::fd::AsRawFd;

    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(unix)]
fn unlock_file(file: &File) {
    use std::os::fd::AsRawFd;

    let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
}

#[cfg(windows)]
#[repr(C)]
struct WinOverlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: *mut std::ffi::c_void,
}

#[cfg(windows)]
impl WinOverlapped {
    fn zeroed() -> Self {
        Self {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            event: std::ptr::null_mut(),
        }
    }
}

#[cfg(windows)]
#[link(name = "Kernel32")]
extern "system" {
    #[link_name = "LockFileEx"]
    fn lock_file_ex(
        file: *mut std::ffi::c_void,
        flags: u32,
        reserved: u32,
        bytes_low: u32,
        bytes_high: u32,
        overlapped: *mut WinOverlapped,
    ) -> i32;
    #[link_name = "UnlockFileEx"]
    fn unlock_file_ex(
        file: *mut std::ffi::c_void,
        reserved: u32,
        bytes_low: u32,
        bytes_high: u32,
        overlapped: *mut WinOverlapped,
    ) -> i32;
    #[link_name = "GetLastError"]
    fn get_last_error() -> u32;
}

#[cfg(windows)]
fn try_lock_file(file: &File) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;

    const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x0000_0001;
    const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x0000_0002;
    const ERROR_LOCK_VIOLATION: u32 = 33;

    let mut overlapped = WinOverlapped::zeroed();
    let result = unsafe {
        lock_file_ex(
            file.as_raw_handle(),
            LOCKFILE_FAIL_IMMEDIATELY | LOCKFILE_EXCLUSIVE_LOCK,
            0,
            1,
            0,
            &mut overlapped,
        )
    };
    if result != 0 {
        return Ok(true);
    }
    let code = unsafe { get_last_error() };
    if code == ERROR_LOCK_VIOLATION {
        Ok(false)
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

#[cfg(windows)]
fn unlock_file(file: &File) {
    use std::os::windows::io::AsRawHandle;

    let mut overlapped = WinOverlapped::zeroed();
    let _ = unsafe { unlock_file_ex(file.as_raw_handle(), 0, 1, 0, &mut overlapped) };
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
    let _run_lock = RunLock::acquire(&root)?;
    let cargo_args =
        cargo_proxy::feature_args(&args.features, args.all_features, args.no_default_features);
    let _cargo_proxy = cargo_proxy::install(&root, "mutate4rust", &cargo_args)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn second_mutation_process_lock_is_rejected_until_release() {
        let dir = tempdir().unwrap();
        let first = RunLock::acquire(dir.path()).unwrap();
        let second = RunLock::acquire(dir.path());
        assert!(matches!(second, Err(Error::Mutation(_))));
        drop(first);
        RunLock::acquire(dir.path()).unwrap();
    }
}
