from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from . import __version__
from .core import (
    MutationError,
    collect_mutations,
    detect_test_command,
    detect_validation_command,
    recover_active,
    run_command,
    run_mutations,
    write_manifest,
)


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser(description='Mutation testing for Rust source.')
    value.add_argument("filters", nargs="*", help="Only mutate source paths that contain one of these fragments.")
    value.add_argument("--root", type=Path, default=Path("."))
    value.add_argument("--test-command", help="Test command. The tool detects a safe default when possible.")
    value.add_argument("--validate-command", help="Build/type-check command run before tests for every mutant.")
    value.add_argument("--no-validate", action="store_true", help="Disable build validation. Unsafe for compiled languages.")
    value.add_argument("--timeout", type=float, default=120.0, help="Timeout for each command in seconds.")
    value.add_argument("--max-mutants", type=int)
    value.add_argument("--list", action="store_true", help="List mutation sites without changing files.")
    value.add_argument("--skip-baseline", action="store_true")
    value.add_argument("--include-tests", action="store_true")
    value.add_argument("--manifest", type=Path, default=Path("target/mutation/results.json"))
    value.add_argument("--json", action="store_true", dest="json_output")
    value.add_argument("--fail-on-survivors", action="store_true", default=True)
    value.add_argument("--allow-survivors", action="store_false", dest="fail_on_survivors")
    value.add_argument("--allow-compile-errors", action="store_true", help="Exclude compile-error mutants instead of failing the run.")
    value.add_argument("--version", action="version", version=f"%(prog)s {__version__}")
    return value


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    root = args.root.resolve()
    try:
        recover_active(root)
        mutations = collect_mutations(root, args.filters, args.include_tests)
        if args.list:
            payload = {"schema_version": 1, "tool": 'mutate4rust', "version": __version__,
                       "summary": {"total": len(mutations)},
                       "mutants": [mutation.public_dict() for mutation in mutations]}
            if args.json_output:
                print(json.dumps(payload, indent=2, sort_keys=True))
            else:
                for mutation in mutations:
                    print(f"{mutation.id}\t{mutation.file}:{mutation.line}:{mutation.column}\t{mutation.original} -> {mutation.replacement}")
            return 0
        if not mutations:
            raise MutationError("no mutation sites were discovered")
        test_command = args.test_command or detect_test_command(root)
        if not test_command:
            raise MutationError("--test-command is required because no project test command was detected")
        validation_command = None if args.no_validate else (args.validate_command or detect_validation_command(root))
        if 'rust' in {"c", "cpp", "objc"} and not args.no_validate and not validation_command:
            raise MutationError("--validate-command is required for compiled C-family projects when no build command is detected")
        if not args.skip_baseline:
            baseline = run_command(test_command, root, args.timeout)
            if baseline.timed_out:
                raise MutationError("baseline test command timed out")
            if baseline.returncode != 0:
                raise MutationError(f"baseline tests failed with exit code {baseline.returncode}")
        results = run_mutations(root, mutations, test_command, args.timeout, validation_command, args.max_mutants)
        manifest = args.manifest if args.manifest.is_absolute() else root / args.manifest
        write_manifest(manifest, 'mutate4rust', __version__, root, results)
    except (OSError, ValueError, MutationError, json.JSONDecodeError) as error:
        print(f"mutate4rust: {error}", file=sys.stderr)
        return 1

    infra = [result for result in results if result.status in {"invalid", "timeout", "error"}]
    compile_errors = [result for result in results if result.status == "compile-error"]
    survivors = [result for result in results if result.status == "survived"]
    counts: dict[str, int] = {}
    for result in results:
        counts[result.status] = counts.get(result.status, 0) + 1
    if args.json_output:
        payload = {"schema_version": 1, "tool": 'mutate4rust', "version": __version__,
                   "root": root.as_posix(), "summary": {"total": len(results), **counts},
                   "mutants": [result.to_dict() for result in results]}
        print(json.dumps(payload, indent=2, sort_keys=True))
    else:
        print("Mutation Report")
        print("===============")
        print(f"Total: {len(results)}")
        for status in sorted(counts):
            print(f"{status}: {counts[status]}")
        for result in survivors:
            mutation = result.mutation
            print(f"SURVIVED {mutation.file}:{mutation.line}:{mutation.column} {mutation.original} -> {mutation.replacement}")
    if infra or (compile_errors and not args.allow_compile_errors):
        return 1
    if survivors and args.fail_on_survivors:
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
