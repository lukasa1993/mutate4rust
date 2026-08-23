# mutate4rust

Native Rust mutation testing. The executable is written in Rust and uses the Rust syntax tree to mutate executable expressions only.

## Requirements

- Rust 1.82 or later.
- No Python, Node, JVM, or other language runtime.

## Install

```bash
cargo install --git https://github.com/lukasa1993/mutate4rust --locked --force
```

The repository commits `Cargo.lock`. Use `--locked` so installation uses the dependency graph tested by CI.

## Source scope

`mutate4rust` asks Cargo for the active compiler `cfg` values of each selected target. It follows active modules, `#[path]` modules, and literal `.rs` files used by `include!`, including expression-position includes and optional trailing commas. It excludes inactive platform, feature, field, match-arm, local-statement, and test-only source.

For safety, expression-included source must stay inside the project root and outside generated, vendored, and build directories. The default feature set is the Cargo default feature set. Use `--features`, `--no-default-features`, or `--all-features` to select another supported configuration.

## Run

```bash
mutate4rust \
  --test-command "cargo test --workspace" \
  --validate-command "cargo check --workspace"
```

The tool runs the unchanged test and validation baselines first, validates each mutant, restores source with a crash-recovery journal, terminates timed-out command process groups before restoration, and keeps `killed`, `survived`, `timeout`, `invalid`, and `compile-error` separate.

Exit status: `0` pass, `1` infrastructure/invalid/compile-error failure, `2` surviving mutants.
