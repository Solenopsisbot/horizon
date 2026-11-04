import json
import unittest

from tools.native_java_perf import MAX_RUNS, summarize


class NativeJavaPerfTests(unittest.TestCase):
    def test_summary_is_deterministic_and_bounded(self):
        report = summarize([4.0, 1.0, 3.0, 2.0])
        self.assertEqual(report["n"], 4)
        self.assertEqual(report["mean_ms"], 2.5)
        self.assertEqual(report["p50_ms"], 2.0)
        self.assertEqual(report["p95_ms"], 3.0)
        json.dumps(report)

    def test_empty_summary_has_null_metrics(self):
        self.assertEqual(summarize([]), {
            "n": 0, "mean_ms": None, "p50_ms": None, "p95_ms": None,
            "min_ms": None, "max_ms": None,
        })

    def test_run_limit_is_finite(self):
        self.assertEqual(MAX_RUNS, 100)


if __name__ == "__main__":
    unittest.main()
