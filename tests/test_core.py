import sys
from pathlib import Path

from mutate4rust.core import collect_mutations, run_mutations


def test_target_language_mutations_skip_non_code(tmp_path: Path) -> None:
    path = tmp_path / 'sample.rs'
    original = 'pub fn choose(a: bool, b: bool) -> i32 {\n    if a && b { 1 } else { 0 }\n}\n'
    path.write_text(original + '\n// == && true\n', encoding="utf-8")
    mutations = collect_mutations(tmp_path)
    assert mutations
    comment_line = original.count("\n") + 2
    assert all(mutation.line < comment_line for mutation in mutations)


def test_timeout_is_not_killed_and_source_is_restored(tmp_path: Path) -> None:
    path = tmp_path / 'sample.rs'
    original = 'pub fn choose(a: bool, b: bool) -> i32 {\n    if a && b { 1 } else { 0 }\n}\n'
    path.write_text(original, encoding="utf-8")
    mutations = collect_mutations(tmp_path)
    assert mutations
    command = f'{sys.executable} -c "import time; time.sleep(2)"'
    results = run_mutations(tmp_path, mutations[:1], command, 0.05, None, 1)
    assert results[0].status == "timeout"
    assert path.read_text(encoding="utf-8") == original
