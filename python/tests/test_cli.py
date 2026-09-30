"""``etio-eval run`` must not score RCAEval with a model trained on RCAEval."""

import json

import pytest

import etio
from etio import cli


@pytest.fixture
def captured(monkeypatch, tmp_path):
    seen = {}

    class FakeDataset:
        def __init__(self, _path):
            pass

        def cases(self, _datasets):
            return []

    def fake_run(_ds, _cases, _methods, *, config, **_kw):
        seen["config"] = config
        return []

    def fake_save(_results, _path, meta):
        seen["meta"] = meta

    monkeypatch.setattr(cli, "RcaEval", FakeDataset)
    monkeypatch.setattr(cli.benchmark, "run", fake_run)
    monkeypatch.setattr(cli.benchmark, "save", fake_save)
    monkeypatch.setattr(cli.report, "table", lambda *_a, **_k: "")
    seen["out"] = str(tmp_path / "out.json")
    return seen


def run(captured, *extra):
    assert cli.main(["run", "--methods", "etio", "--out", captured["out"], *extra]) == 0


def test_defaults_to_the_heuristic_model(captured):
    run(captured)
    assert captured["config"]["model"] == json.loads(etio.heuristic_model())
    assert captured["config"]["ensemble"] == []
    assert captured["meta"]["model"] == "heuristic"


def test_bundled_model_is_opt_in_and_flagged(captured, capsys):
    run(captured, "--bundled-model")
    assert "model" not in captured["config"]
    assert captured["meta"]["model"] == "bundled"
    assert "in-sample" in capsys.readouterr().err


def test_explicit_model_file(captured, tmp_path):
    path = tmp_path / "m.json"
    path.write_text(etio.heuristic_model())
    run(captured, "--model", str(path))
    assert captured["meta"]["model"] == str(path)
    assert "ensemble" not in captured["config"]
