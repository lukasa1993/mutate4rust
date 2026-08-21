# mutate4rust

Use `mutate4rust` for Rust mutation verification.

1. Run `mutate4rust --help` before first use.
2. Use the complete project test command.
3. Keep build validation enabled.
4. Treat exit `1` as a test, timeout, parse, validation, or restoration failure. Do not report it as a quality pass.
5. Treat exit `2` as surviving-mutant failure.
