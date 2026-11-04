import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from tools.native_java_perf import (
    CACHE_COLD,
    CACHE_WARM,
    MAX_METHODS,
    MAX_RUNS,
    MAX_WARMUP,
    backend_label,
    build_parser,
    main,
    method_cache_labels,
    parse_method,
    run_benchmark,
    summarize,
)

ROOT = Path(__file__).resolve().parents[1]


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
            "n": 0, "mean_ms": None, "p50_ms": None, "p95_ms": None, "p99_ms": None,
            "min_ms": None, "max_ms": None,
        })

    def test_run_limit_is_finite(self):
        self.assertEqual(MAX_RUNS, 100)
        self.assertEqual(MAX_METHODS, 32)
        self.assertEqual(MAX_WARMUP, 50)

    def test_percentiles_include_p99_for_long_samples(self):
        report = summarize([float(value) for value in range(1, 101)])
        self.assertEqual(report["n"], 100)
        self.assertEqual(report["p50_ms"], 50.0)
        self.assertEqual(report["p95_ms"], 95.0)
        self.assertEqual(report["p99_ms"], 99.0)

    def test_p99_is_bounded_by_the_sample_count(self):
        report = summarize([10.0])
        self.assertEqual(report["p99_ms"], 10.0)
        self.assertEqual(report["p95_ms"], 10.0)


class ParseMethodTests(unittest.TestCase):
    def test_class_only_names_the_default_entrypoint(self):
        self.assertEqual(parse_method("demo.Main"), {"class": "demo.Main", "method": None, "label": "demo.Main"})

    def test_named_method_keeps_class_and_name(self):
        self.assertEqual(
            parse_method("demo.Main#onInitialize"),
            {"class": "demo.Main", "method": "onInitialize", "label": "demo.Main#onInitialize"},
        )

    def test_overloads_stay_distinct_through_the_descriptor(self):
        one = parse_method("demo.Main#run(I)V")
        two = parse_method("demo.Main#run(Ljava/lang/String;)V")
        self.assertNotEqual(one["label"], two["label"])
        self.assertEqual(one["class"], "demo.Main")
        self.assertEqual(one["method"], "run")

    def test_inner_class_and_constructor_forms_are_accepted(self):
        self.assertEqual(parse_method("demo.Main$Inner#<init>")["method"], "<init>")

    def test_rejects_empty_and_malformed_values(self):
        for raw in ("", "   ", "#name", "demo.Main#", "demo Main#x", "demo.Main#run("):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                parse_method(raw)


class BackendAndCacheLabelTests(unittest.TestCase):
    def test_backend_labels_name_the_launch_path(self):
        self.assertEqual(backend_label(None, ["cargo", "run", "--quiet", "-p", "x", "--"]), "cargo-run")
        self.assertEqual(backend_label(None, ["/repo/target/fast/horizon-native-java"]), "target-fast")
        self.assertEqual(backend_label(None, ["/repo/target/debug/horizon-native-java"]), "target-debug")
        self.assertEqual(backend_label(None, ["/repo/target/release/horizon-native-java"]), "target-release")
        self.assertEqual(backend_label(None, ["/usr/local/bin/probe"]), "external")
        self.assertEqual(backend_label("/opt/probe", ["/opt/probe"]), "explicit")

    def test_first_method_is_cold_and_the_rest_warm(self):
        methods = [{"label": "a.B"}, {"label": "a.C"}, {"label": "a.D"}]
        self.assertEqual(
            method_cache_labels(methods),
            [
                {"label": "a.B", "cache": CACHE_COLD},
                {"label": "a.C", "cache": CACHE_WARM},
                {"label": "a.D", "cache": CACHE_WARM},
            ],
        )


class BenchmarkTests(unittest.TestCase):
    """Drive the benchmark with a fake runner so no jar or cargo is needed."""

    def fake_runner(self, *args, **kwargs):
        # Record the class the harness hands the probe; every call succeeds.
        self.seen.append(args[4] if len(args) > 4 else kwargs.get("class_name"))
        return 1.0, {"status": "ok"}, "ok"

    def setUp(self):
        self.seen = []

    def test_report_separates_warmup_from_measured_runs(self):
        methods = [parse_method("demo.Main#tick")]
        report = run_benchmark(
            ["/repo/target/fast/horizon-native-java"], Path("/nonexistent/mod.jar"), None,
            methods, warmup=2, runs=4, timeout=5.0, backend="target-fast",
            runner=self.fake_runner, stash=False,
        )
        self.assertEqual(report["backend"], "target-fast")
        self.assertEqual(report["warmup_runs"], 2)
        self.assertEqual(report["measured_runs"], 4)
        row = report["methods"][0]
        self.assertEqual(row["warmup"], {"n": 2, "mean_ms": 1.0})
        self.assertEqual(row["summary"]["n"], 4)
        self.assertEqual(row["outcomes"], ["ok"])
        json.dumps(report)
        # The warmup runs happen first, then the measured ones.
        self.assertEqual(len(self.seen), 6)

    def test_each_method_is_passed_to_the_probe_as_an_entrypoint_class(self):
        methods = [parse_method("demo.A"), parse_method("demo.B#run")]
        run_benchmark(
            ["/repo/target/fast/horizon-native-java"], Path("/nonexistent/mod.jar"), None,
            methods, warmup=0, runs=2, timeout=5.0, runner=self.fake_runner, stash=False,
        )
        self.assertEqual(self.seen, ["demo.A", "demo.A", "demo.B", "demo.B"])

    def test_cache_labels_follow_the_run_order(self):
        methods = [parse_method("demo.A#x"), parse_method("demo.B#y")]
        report = run_benchmark(
            ["probe"], Path("/nonexistent/mod.jar"), None, methods,
            warmup=0, runs=1, timeout=5.0, runner=self.fake_runner, stash=False,
        )
        self.assertEqual([row["cache"] for row in report["methods"]], [CACHE_COLD, CACHE_WARM])
        self.assertEqual([row["label"] for row in report["methods"]], ["demo.A#x", "demo.B#y"])

    def test_zero_warmup_reports_no_warmup_mean(self):
        methods = [parse_method("demo.A")]
        report = run_benchmark(
            ["probe"], Path("/nonexistent/mod.jar"), None, methods,
            warmup=0, runs=1, timeout=5.0, runner=self.fake_runner, stash=False,
        )
        self.assertEqual(report["methods"][0]["warmup"], {"n": 0, "mean_ms": None})

    def test_a_failing_method_keeps_its_outcomes_visible(self):
        def failing_runner(*args, **kwargs):
            return 0.5, {"status": "error"}, "error"

        report = run_benchmark(
            ["probe"], Path("/nonexistent/mod.jar"), None, [parse_method("demo.A")],
            warmup=0, runs=2, timeout=5.0, runner=failing_runner, stash=False,
        )
        self.assertEqual(report["methods"][0]["outcomes"], ["error"])


class CliTests(unittest.TestCase):
    def test_parser_accepts_repeatable_methods_and_warmup(self):
        args = build_parser().parse_args(
            ["--jar", "mod.jar", "--method", "demo.A", "--method", "demo.B#run", "--warmup", "0"]
        )
        self.assertEqual(args.method, ["demo.A", "demo.B#run"])
        self.assertEqual(args.warmup, 0)

    def test_cli_rejects_malformed_method_before_running_anything(self):
        with tempfile.NamedTemporaryFile(suffix=".jar") as handle:
            result = subprocess.run(
                [sys.executable, str(ROOT / "tools" / "native_java_perf.py"),
                 "--jar", handle.name, "--method", "not a method"],
                text=True, capture_output=True, timeout=60, check=False,
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid method", result.stderr)

    def test_cli_rejects_non_finite_timeout(self):
        with tempfile.NamedTemporaryFile(suffix=".jar") as handle:
            result = subprocess.run(
                [sys.executable, str(ROOT / "tools" / "native_java_perf.py"),
                 "--jar", handle.name, "--timeout", "nan"],
                text=True, capture_output=True, timeout=60, check=False,
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("finite", result.stderr)

    def test_cli_bounds_warmup(self):
        with tempfile.NamedTemporaryFile(suffix=".jar") as handle:
            result = subprocess.run(
                [sys.executable, str(ROOT / "tools" / "native_java_perf.py"),
                 "--jar", handle.name, "--warmup", str(MAX_WARMUP + 1)],
                text=True, capture_output=True, timeout=60, check=False,
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--warmup", result.stderr)


if __name__ == "__main__":
    unittest.main()
