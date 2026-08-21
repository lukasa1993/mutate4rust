# mutate4rust

`mutate4rust` performs source-level mutation testing for Rust projects. It changes one operator or Boolean value at a time, runs the test command, and restores the original source in a `finally` block.

## Install

```bash
pipx install git+https://github.com/lukasa1993/mutate4rust.git
```

## Run

```bash
mutate4rust --test-command "cargo llvm-cov --json --output-path target/coverage/coverage.json"
```

The command runs the unchanged test suite first. It stops if the baseline fails. Use `--skip-baseline` only when another step already verified the same source.

Useful options:

```bash
mutate4rust --list
mutate4rust --max-mutants 25
mutate4rust --json
```

Results are written to `target/mutation/mutate4rust.json`. Exit status `2` means that one or more mutants survived.

## Development

```bash
python -m pip install -e . pytest
pytest -q
```
