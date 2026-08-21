from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from . import __version__
from .core import collect_mutations, run_mutations, write_manifest

DEFAULT_TEST_COMMAND = 'cargo llvm-cov --json --output-path target/coverage/coverage.json'
DEFAULT_MANIFEST = Path("target/mutation/mutate4rust.json")


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser(description='Mutation testing for Rust projects')
    value.add_argument("filters", nargs="*", help="Only mutate source paths that contain one of these fragments.")
    value.add_argument("--root", type=Path, default=Path("."), help="Project root.")
    value.add_argument("--test-command", default=DEFAULT_TEST_COMMAND, help="Command run for the baseline and each mutant.")
    value.add_argument("--timeout", type=float, default=60.0, help="Seconds allowed for each test run.")
    value.add_argument("--max-mutants", type=int, default=None, help="Stop after this number of mutants.")
    value.add_argument("--skip-baseline", action="store_true", help="Do not verify the unchanged source before mutation.")
    value.add_argument("--list", action="store_true", dest="list_only", help="List mutation candidates without running tests.")
    value.add_argument("--json", action="store_true", dest="json_output", help="Write JSON output.")
    value.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST, help="Result manifest path.")
    value.add_argument("--version", action="version", version=f"%(prog)s {__version__}")
    return value


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    root = args.root.resolve()
    try:
        mutations = collect_mutations(root, args.filters)
        if args.list_only:
            rows = [mutation.public_dict() for mutation in mutations]
            if args.json_output:
                print(json.dumps(rows, indent=2, sort_keys=True))
            else:
                for mutation in mutations:
                    print(f"{mutation.id:5d} {mutation.file}:{mutation.line}:{mutation.column} {mutation.original} -> {mutation.replacement}")
            return 0
        results = run_mutations(root, mutations, args.test_command, args.timeout, args.max_mutants, not args.skip_baseline)
        manifest = args.manifest if args.manifest.is_absolute() else root / args.manifest
        write_manifest(manifest, results)
    except (OSError, ValueError, RuntimeError) as error:
        print(f"mutate4rust: {error}", file=sys.stderr)
        return 1

    if args.json_output:
        print(json.dumps([result.to_dict() for result in results], indent=2, sort_keys=True))
    else:
        for result in results:
            mutation = result.mutation
            print(f"{result.status.upper():8} {mutation.file}:{mutation.line}:{mutation.column} {mutation.original} -> {mutation.replacement}")
        killed = sum(result.status == "killed" for result in results)
        survived = sum(result.status == "survived" for result in results)
        print(f"\nMutants: {len(results)}  Killed: {killed}  Survived: {survived}")
    return 2 if any(result.status == "survived" for result in results) else 0
