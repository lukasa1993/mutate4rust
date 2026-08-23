use std::fs;

fn replace_once(text: &mut String, old: &str, new: &str, label: &str) {
    let first = text
        .find(old)
        .unwrap_or_else(|| panic!("missing patch anchor: {label}"));
    assert!(
        text[first + old.len()..].find(old).is_none(),
        "duplicate patch anchor: {label}"
    );
    text.replace_range(first..first + old.len(), new);
}

fn main() {
    let mut cargo = fs::read_to_string("Cargo.toml").unwrap();
    replace_once(
        &mut cargo,
        "version = \"2.0.1\"",
        "version = \"2.0.2\"",
        "package version",
    );
    fs::write("Cargo.toml", cargo).unwrap();

    let mut source = fs::read_to_string("src/lib.rs").unwrap();
    replace_once(
        &mut source,
        r###"use serde::{Deserialize, Serialize};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};"###,
        r###"use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
use std::io::Read;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};"###,
        "imports",
    );

    replace_once(
        &mut source,
        r###"pub fn run_shell(command: &str, root: &Path, timeout: Duration) -> Result<CommandResult, Error> {
    let started = Instant::now();
    let mut child = spawn_shell(command, root)?;
    let status = child.wait_timeout(timeout)?;
    let timed_out = status.is_none();
    if timed_out {
        terminate_process_tree(&mut child);
    }
    let output = child.wait_with_output()?;
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(CommandResult {
        exit_code: if timed_out {
            None
        } else {
            output.status.code()
        },
        timed_out,
        duration: started.elapsed(),
        output: combined,
    })
}

pub fn recover_active"###,
        r###"const MAX_CAPTURE_BYTES: usize = 2 * 1024 * 1024;

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
        return Err(Error::Mutation(format!("unsafe mutation source path: {file}")));
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

pub fn recover_active"###,
        "command runner and source resolver",
    );

    replace_once(
        &mut source,
        "    fs::write(root.join(record.file), bytes)?;",
        "    let path = resolve_source_path(root, &record.file)?;\n    fs::write(path, bytes)?;",
        "journal recovery path",
    );
    replace_once(
        &mut source,
        "        let path = root.join(&mutation.file);",
        "        let path = resolve_source_path(root, &mutation.file)?;",
        "mutation source path",
    );
    replace_once(
        &mut source,
        r###"impl Drop for SourceGuard {
    fn drop(&mut self) {
        let _ = fs::write(&self.path, &self.original);
        let _ = fs::remove_file(&self.journal);
    }
}"###,
        r###"impl Drop for SourceGuard {
    fn drop(&mut self) {
        if fs::write(&self.path, &self.original).is_ok() {
            let _ = fs::remove_file(&self.journal);
        }
    }
}"###,
        "source restoration",
    );

    replace_once(
        &mut source,
        r###"    #[test]
    fn timeout_is_not_killed() {"###,
        r###"    #[test]
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
    fn timeout_is_not_killed() {"###,
        "regression tests",
    );

    fs::write("src/lib.rs", source).unwrap();
}
