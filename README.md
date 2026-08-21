# mutate4rust

`mutate4rust` performs syntax-aware mutation testing for Rust. It verifies the baseline, restores source through a crash-recovery journal, separates timeouts and compile errors from killed mutants, and writes a versioned JSON manifest.

```bash
pipx install git+https://github.com/lukasa1993/mutate4rust.git
mutate4rust --test-command "cargo test --quiet" --validate-command "cargo check --quiet"
```

A timeout, an invalid mutant, or a compile error is not a killed mutant. These states return exit status `1`. A surviving mutant returns exit status `2` by default.

## Development

```bash
python -m pip install -e . pytest
pytest -q
```
