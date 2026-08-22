# mutate4rust

Native Rust mutation testing. The executable is written in Rust and uses the Rust syntax tree to mutate executable expressions only.

## Install

```bash
cargo install --git https://github.com/lukasa1993/mutate4rust --force
```

## Run

```bash
mutate4rust \
  --test-command "cargo test --workspace --all-features" \
  --validate-command "cargo check --workspace --all-targets --all-features"
```

The tool runs the baseline first, validates each mutant, restores source with a crash-recovery journal, and keeps `killed`, `survived`, `timeout`, `invalid`, and `compile-error` separate.

Exit status: `0` pass, `1` infrastructure/invalid/compile-error failure, `2` surviving mutants.

No Python, Node, JVM, or other language runtime is required.
