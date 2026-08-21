from __future__ import annotations

import json
import os
import re
import subprocess
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Iterable, Sequence

LANGUAGE = 'rust'
EXTENSIONS = tuple(['.rs'])
EXCLUDED_DIRS = {".git", ".hg", ".idea", ".pytest_cache", ".tox", ".venv", "build", "coverage", "dist", "node_modules", "target", "vendor", "venv", ".build"}

REPLACEMENTS = {
    "typescript": [(r"\btrue\b", "false"), (r"\bfalse\b", "true"), (r"!==", "==="), (r"===", "!=="), (r"!=", "=="), (r"==", "!="), (r"&&", "||"), (r"\|\|", "&&"), (r"<=", ">"), (r">=", "<"), (r"(?<![+])-", "+"), (r"\+", "-"), (r"\*", "/")],
    "rust": [(r"\btrue\b", "false"), (r"\bfalse\b", "true"), (r"!=", "=="), (r"==", "!="), (r"&&", "||"), (r"\|\|", "&&"), (r"<=", ">"), (r">=", "<"), (r"(?<![+])-", "+"), (r"\+", "-"), (r"\*", "/")],
    "swift": [(r"\btrue\b", "false"), (r"\bfalse\b", "true"), (r"!=", "=="), (r"==", "!="), (r"&&", "||"), (r"\|\|", "&&"), (r"<=", ">"), (r">=", "<"), (r"(?<![+])-", "+"), (r"\+", "-"), (r"\*", "/")],
    "objective-c": [(r"\bYES\b", "NO"), (r"\bNO\b", "YES"), (r"\btrue\b", "false"), (r"\bfalse\b", "true"), (r"!=", "=="), (r"==", "!="), (r"&&", "||"), (r"\|\|", "&&"), (r"<=", ">"), (r">=", "<"), (r"(?<![+])-", "+"), (r"\+", "-"), (r"\*", "/")],
    "bash": [(r"\btrue\b", "false"), (r"\bfalse\b", "true"), (r"&&", "||"), (r"\|\|", "&&"), (r"-eq\b", "-ne"), (r"-ne\b", "-eq"), (r"-lt\b", "-ge"), (r"-le\b", "-gt"), (r"-gt\b", "-le"), (r"-ge\b", "-lt"), (r"!=", "=="), (r"==", "!=")],
}[LANGUAGE]


@dataclass(frozen=True)
class Mutation:
    id: int
    file: str
    line: int
    column: int
    original: str
    replacement: str
    start: int
    end: int

    def public_dict(self) -> dict[str, object]:
        value = asdict(self)
        value.pop("start")
        value.pop("end")
        return value


@dataclass(frozen=True)
class MutationResult:
    mutation: Mutation
    status: str
    exit_code: int | None

    def to_dict(self) -> dict[str, object]:
        return {**self.mutation.public_dict(), "status": self.status, "exit_code": self.exit_code}


def _is_test(relative: str) -> bool:
    lowered = relative.lower()
    name = Path(relative).name.lower()
    if LANGUAGE == "typescript":
        return name.endswith((".test.ts", ".test.tsx", ".spec.ts", ".spec.tsx", ".d.ts")) or "/tests/" in f"/{lowered}/"
    if LANGUAGE == "rust":
        return "/tests/" in f"/{lowered}/" or name.endswith("_test.rs")
    if LANGUAGE == "swift":
        return "/tests/" in f"/{lowered}/" or name.endswith("tests.swift")
    if LANGUAGE == "objective-c":
        return "/tests/" in f"/{lowered}/" or name.endswith(("tests.m", "tests.mm", "test.m", "test.mm"))
    return "/test/" in f"/{lowered}/" or "/tests/" in f"/{lowered}/" or name.endswith(".bats")


def discover_files(root: Path, filters: Sequence[str] = ()) -> list[Path]:
    files: list[Path] = []
    for directory, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(name for name in dirnames if name not in EXCLUDED_DIRS)
        for filename in sorted(filenames):
            if not filename.endswith(EXTENSIONS):
                continue
            path = Path(directory, filename)
            relative = path.relative_to(root).as_posix()
            if _is_test(relative):
                continue
            if filters and not any(fragment in relative for fragment in filters):
                continue
            files.append(path)
    return files


def mask_non_code(text: str) -> str:
    out = list(text)
    index = 0
    state = "code"
    quote = ""
    while index < len(text):
        char = text[index]
        next_char = text[index + 1] if index + 1 < len(text) else ""
        if state == "code":
            if LANGUAGE == "bash" and char == "#":
                state = "line"
                out[index] = " "
            elif LANGUAGE != "bash" and char == "/" and next_char == "/":
                state = "line"
                out[index] = out[index + 1] = " "
                index += 1
            elif LANGUAGE != "bash" and char == "/" and next_char == "*":
                state = "block"
                out[index] = out[index + 1] = " "
                index += 1
            elif char in {'"', "'", "`"} and (LANGUAGE == "typescript" or char != "`"): 
                state = "string"
                quote = char
                out[index] = " "
            else:
                out[index] = char
        elif state == "line":
            if char == "\n":
                state = "code"
                out[index] = "\n"
            else:
                out[index] = " "
        elif state == "block":
            out[index] = "\n" if char == "\n" else " "
            if char == "*" and next_char == "/":
                out[index + 1] = " "
                index += 1
                state = "code"
        else:
            out[index] = "\n" if char == "\n" else " "
            if char == "\\" and quote != "'" and index + 1 < len(text):
                out[index + 1] = " "
                index += 1
            elif char == quote:
                state = "code"
        index += 1
    return "".join(out)


def enumerate_mutations(path: Path, root: Path, start_id: int = 1) -> list[Mutation]:
    text = path.read_text(encoding="utf-8", errors="replace")
    masked = mask_non_code(text)
    candidates: list[tuple[int, int, str, str]] = []
    for pattern, replacement in REPLACEMENTS:
        for match in re.finditer(pattern, masked):
            original = text[match.start() : match.end()]
            if original == replacement:
                continue
            candidates.append((match.start(), match.end(), original, replacement))
    candidates.sort(key=lambda value: (value[0], -(value[1] - value[0])))
    selected: list[tuple[int, int, str, str]] = []
    last_end = -1
    for candidate in candidates:
        if candidate[0] < last_end:
            continue
        selected.append(candidate)
        last_end = candidate[1]
    out: list[Mutation] = []
    relative = path.relative_to(root).as_posix()
    for index, (start, end, original, replacement) in enumerate(selected, start_id):
        line = text.count("\n", 0, start) + 1
        line_start = text.rfind("\n", 0, start) + 1
        out.append(Mutation(index, relative, line, start - line_start + 1, original, replacement, start, end))
    return out


def collect_mutations(root: Path, filters: Sequence[str] = ()) -> list[Mutation]:
    out: list[Mutation] = []
    for path in discover_files(root, filters):
        out.extend(enumerate_mutations(path, root, len(out) + 1))
    return out


def _run(command: str, root: Path, timeout_seconds: float) -> tuple[int | None, bool]:
    try:
        completed = subprocess.run(command, cwd=root, shell=True, check=False, timeout=timeout_seconds, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return completed.returncode, False
    except subprocess.TimeoutExpired:
        return None, True


def run_mutations(root: Path, mutations: Iterable[Mutation], test_command: str, timeout_seconds: float, max_mutants: int | None = None, run_baseline: bool = True) -> list[MutationResult]:
    if run_baseline:
        baseline, timed_out = _run(test_command, root, timeout_seconds)
        if timed_out or baseline != 0:
            raise RuntimeError("baseline tests did not pass")
    results: list[MutationResult] = []
    for mutation in mutations:
        if max_mutants is not None and len(results) >= max_mutants:
            break
        path = root / mutation.file
        original_text = path.read_text(encoding="utf-8", errors="replace")
        mutated = original_text[: mutation.start] + mutation.replacement + original_text[mutation.end :]
        path.write_text(mutated, encoding="utf-8")
        try:
            exit_code, timed_out = _run(test_command, root, timeout_seconds)
            status = "killed" if timed_out or exit_code != 0 else "survived"
        finally:
            path.write_text(original_text, encoding="utf-8")
        results.append(MutationResult(mutation, status, exit_code))
    return results


def write_manifest(path: Path, results: Iterable[MutationResult]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps([result.to_dict() for result in results], indent=2, sort_keys=True) + "\n", encoding="utf-8")
