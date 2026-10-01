import importlib.util
import json
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "performance_gate.py"
SPEC = importlib.util.spec_from_file_location("performance_gate", SCRIPT)
PERFORMANCE_GATE = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(PERFORMANCE_GATE)


class PerformanceGateTests(unittest.TestCase):
    def run_comparison(self, baseline, current):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        baseline_path = root / "baseline.json"
        current_path = root / "current.json"
        report_path = root / "report.json"
        baseline_path.write_text(json.dumps(baseline), encoding="utf-8")
        current_path.write_text(json.dumps(current), encoding="utf-8")
        result = PERFORMANCE_GATE.compare(
            Namespace(
                baseline=baseline_path,
                current=current_path,
                report=report_path,
                replace_baseline=False,
                bootstrap_if_missing=False,
            )
        )
        return result, json.loads(report_path.read_text(encoding="utf-8"))

    @staticmethod
    def record(point, low, high):
        estimate = {
            "statistic": "mean",
            "point_estimate_seconds": point,
            "confidence_low_seconds": low,
            "confidence_high_seconds": high,
            "regression_tolerance_percent": 5.0,
        }
        return {
            "metadata": {
                "runner_name": "execlaw-bench",
                "system": "Linux",
                "machine": "x86_64",
                "cpu_model": "fixture-cpu",
                "rustc_sha256": "a" * 64,
            },
            "benchmarks": {"replay": estimate},
        }

    def test_deliberate_regression_fails_the_gate(self):
        result, report = self.run_comparison(
            self.record(0.010, 0.009, 0.011),
            self.record(0.014, 0.013, 0.015),
        )

        self.assertEqual(result, 1)
        self.assertEqual(report["status"], "failed")
        self.assertEqual(report["results"][0]["status"], "regression")

    def test_change_inside_confidence_and_noise_margin_passes(self):
        result, report = self.run_comparison(
            self.record(0.010, 0.009, 0.011),
            self.record(0.0113, 0.0105, 0.012),
        )

        self.assertEqual(result, 0)
        self.assertEqual(report["status"], "passed")

    def test_comparison_rejects_a_different_runner_identity(self):
        baseline = self.record(0.010, 0.009, 0.011)
        current = self.record(0.011, 0.010, 0.012)
        current["metadata"]["cpu_model"] = "different-cpu"
        with self.assertRaisesRegex(ValueError, "identity differs"):
            self.run_comparison(baseline, current)


if __name__ == "__main__":
    unittest.main()
