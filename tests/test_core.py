from pathlib import Path

from mutate4rust.core import collect_mutations, enumerate_mutations, run_mutations


def test_ignores_comments_and_finds_code(tmp_path: Path) -> None:
    source = tmp_path / 'sample.rs'
    source.write_text('fn ok(value: i32) -> bool { true && value == 1 } // false == true\n', encoding="utf-8")
    mutations = enumerate_mutations(source, tmp_path)
    assert mutations
    assert all(mutation.line == 1 for mutation in mutations)


def test_restores_source(tmp_path: Path) -> None:
    source = tmp_path / 'sample.rs'
    original = 'fn ok(value: i32) -> bool { true && value == 1 } // false == true\n'
    source.write_text(original, encoding="utf-8")
    mutation = enumerate_mutations(source, tmp_path)[0]
    results = run_mutations(tmp_path, [mutation], "python -c 'raise SystemExit(1)'", 5, run_baseline=False)
    assert results[0].status == "killed"
    assert source.read_text(encoding="utf-8") == original


def test_collects_non_test_source(tmp_path: Path) -> None:
    (tmp_path / 'sample.rs').write_text('fn ok(value: i32) -> bool { true && value == 1 } // false == true\n', encoding="utf-8")
    assert collect_mutations(tmp_path)
