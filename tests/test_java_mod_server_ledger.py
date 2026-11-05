import os
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).parents[1]
sys.path.insert(0, str(ROOT / "tools"))
import java_mod_server_ledger as LEDGER


class JavaModServerLedgerTests(unittest.TestCase):
    def test_parse_success_and_counts(self):
        log = (
            "Horizon: dedicated Java compatibility: Horizon: Java compatibility /mods/apple.jar: "
            "role=dedicated-server engine=native-qualified loader=fabric id=appleskin version=3.0\n"
            "Horizon: Java compatibility: mixin weave: 0 woven, 11 skipped across 1 jar(s)\n"
            "Done (1.234s)! For help, type 'help'"
        )
        row = LEDGER.parse_log(log, "apple.jar", seconds=2.5)
        self.assertEqual((row.verdict, row.engine, row.id, row.version), ("ran", "native-qualified", "appleskin", "3.0"))
        self.assertEqual((row.woven, row.skipped), (0, 11))

    def test_parse_refusal_bounds_reason(self):
        row = LEDGER.parse_log(
            "Horizon: dedicated Java compatibility: ... engine=refused loader=fabric id=cupboard version=1.2\n"
            "dedicated Java mod startup failed: " + "x" * 1000,
            "cupboard.jar", seconds=1.0, exit_code=1)
        self.assertEqual(row.verdict, "aborted")
        self.assertEqual(row.engine, "refused")
        self.assertLessEqual(len(row.reason), LEDGER.MAX_REASON)

    def test_timeout_and_summary(self):
        row = LEDGER.parse_log("starting", "slow.jar", timed_out=True, seconds=45.0)
        self.assertEqual(row.verdict, "timeout")
        self.assertEqual(LEDGER.summary([row, LEDGER.Row("ok.jar", verdict="ran")]),
                         {"total": 2, "ran": 1, "aborted": 0, "timeout": 1})

    def test_integration_requires_prepared_resources(self):
        if os.environ.get("HORIZON_LEDGER_INTEGRATION") != "1":
            self.skipTest("real-server integration is opt-in")
        self.assertTrue((ROOT / ".local/horizon-client/26.3").is_dir())


if __name__ == "__main__":
    unittest.main()
