import importlib.util
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("native_java_abi", ROOT / "tools/native_java_abi.py")
assert SPEC and SPEC.loader
ABI = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ABI)


class NativeJavaAbiProfilesTest(unittest.TestCase):
    def test_profile_jar_classifies_data_only(self):
        row = ABI.profile_jar({"file": "assets.jar", "mod_id": "assets", "classes": 0})
        self.assertEqual(row["tier"], "data-only")
        self.assertEqual(row["reasons"], ["no class files"])

    def test_profile_jar_classifies_static_code(self):
        row = ABI.profile_jar({
            "file": "simple.jar", "mod_id": "simple", "classes": 3,
            "static_code": True, "dynamic": [], "body_errors": 0,
        })
        self.assertEqual(row["tier"], "native-safe")
        self.assertEqual(row["reasons"], ["static code with no bounded census gaps"])

    def test_profile_jar_explains_native_fallback(self):
        row = ABI.profile_jar({
            "file": "dynamic.jar", "mod_id": "dynamic", "classes": 3,
            "static_code": False, "dynamic": ["Class.forName", "ServiceLoader"],
            "body_errors": 2, "mixin_config_targets": ["net/minecraft/Block"],
        })
        self.assertEqual(row["tier"], "native-fallback")
        self.assertEqual(row["reasons"], [
            "Mixin config targets report-only: 1",
            "body scan errors: 2",
            "dynamic: Class.forName",
            "dynamic: ServiceLoader",
        ])

    def test_profile_jar_classifies_unreadable_metadata(self):
        row = ABI.profile_jar({
            "file": "broken.jar", "mod_id": "broken", "classes": 4,
            "error": "invalid fabric.mod.json",
        })
        self.assertEqual(row["tier"], "report-only")
        self.assertEqual(row["reasons"], ["metadata: invalid fabric.mod.json"])

    def test_profiles_are_stable_and_include_zero_counts(self):
        result = ABI.profiles([
            {"file": "z.jar", "mod_id": "z", "classes": 1, "static_code": True},
            {"file": "a.jar", "mod_id": "a", "classes": 0},
        ])
        self.assertEqual(result["counts"], {
            "data-only": 1, "native-safe": 1, "native-fallback": 0, "report-only": 0,
        })
        self.assertEqual([row["file"] for row in result["jars"]], ["a.jar", "z.jar"])


if __name__ == "__main__":
    unittest.main()
