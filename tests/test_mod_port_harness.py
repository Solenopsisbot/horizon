"""Unit coverage for the pure comparison layer of the mod-port probe.

These tests deliberately use in-memory oracle/probe rows, so they exercise the
validation contract without requiring a Minecraft installation or mod jars.
"""

import tempfile
import unittest
from pathlib import Path

from tools import mod_port_harness as harness


SOUND = {
    "volume": 1.0,
    "pitch": 1.0,
    "break": "minecraft:block.stone.break",
    "step": "minecraft:block.stone.step",
    "place": "minecraft:block.stone.place",
    "hit": "minecraft:block.stone.hit",
    "fall": "minecraft:block.stone.fall",
}


def fixture():
    block = "demo:gem_block"
    item = "demo:gem"
    state = {
        "hardness": 3.0,
        "emission": 0,
        "requires_tool": False,
        "friction": 0.6,
        "map_color_id": 3,
        "sound": SOUND,
        "outline": [[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]],
        "collision": [[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]],
        "drops": {"hand": {item: 1}},
    }
    oracle = {
        "blocks": {
            block: {
                "states": {"": state},
                "explosion_resistance": 6.0,
                "emission": 0,
                "requires_tool": False,
                "friction": 0.6,
                "map_color_id": 3,
            }
        },
        "tags": {"block": {"minecraft:mineable/pickaxe": [block]}},
        "recipes": {
            "demo:gem_recipe": {
                "type": "minecraft:crafting",
                "result": item,
                "count": 1,
                "ingredients": [["minecraft:stone"]],
            }
        },
        "items": {
            item: {
                "max_stack_size": 64,
                "components": {"minecraft:max_damage": 0},
                "attribute_modifiers": {"modifiers": []},
                "enchantment_value": 0,
            }
        },
    }
    horizon = {
        "blocks": {
            block: {
                "states": {"": {
                    **state,
                    "explosion_resistance": 6.0,
                    "emission": 0,
                    "requires_tool": False,
                    "friction": 0.6,
                    "map_color_id": 3,
                }}
            }
        },
        "block_tags": {"minecraft:mineable/pickaxe": [block]},
        "recipes": {
            "demo:gem_recipe": {
                "result": item,
                "count": 1,
                "ingredients": [["minecraft:stone"]],
            }
        },
        "items": {
            item: {
                "max_stack_size": 64,
                "max_damage": 0,
                "modifiers": [],
                "tool": None,
                "enchantable": 0,
            }
        },
    }
    carry = {"blocks": {"gem_block": "minecraft:stone"}}
    return oracle, horizon, carry, block, item


class ModPortHarnessTests(unittest.TestCase):
    def test_preflight_reports_all_missing_inputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaises(SystemExit) as raised:
                harness.preflight(
                    root / "work",
                    root / "resources",
                    root / "work" / "oracle" / "facts.json",
                    ["demo"],
                )
        message = str(raised.exception)
        self.assertIn("oracle facts not found", message)
        self.assertIn("prepared resources not found", message)
        self.assertIn("mod work folder not found", message)

    def test_preflight_selects_requested_carried_mod(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            work = root / "work"
            (work / "oracle").mkdir(parents=True)
            (work / "oracle" / "facts.json").write_text("{}")
            (work / "mods" / "demo" / "carry").mkdir(parents=True)
            resources = root / "resources"
            resources.mkdir()
            self.assertEqual(
                harness.preflight(work, resources, work / "oracle" / "facts.json", ["demo"]),
                ["demo"],
            )

    def test_shape_comparison_accepts_equivalent_voxel_boxes(self):
        # Different floating-point box boundaries can still describe the same
        # 1/32-grid collision cells, which is the probe's comparison contract.
        self.assertTrue(
            harness.same_shape(
                [[0.0, 0.0, 0.0, 0.5, 1.0, 1.0]],
                [[0.0, 0.0, 0.0, 0.49999, 1.0, 1.0]],
            )
        )

    def test_complete_fixture_passes_all_comparison_sections(self):
        oracle, horizon, carry, block, item = fixture()
        result = harness.compare_mod("demo", oracle, horizon, carry, {}, set())
        self.assertTrue(result["passed"])
        self.assertEqual(sum(v["failed"] for v in result["checks"].values()), 0)
        self.assertGreater(result["checks"]["blocks"]["passed"], 0)
        self.assertGreater(result["checks"]["recipes"]["passed"], 0)
        self.assertGreater(result["checks"]["item_stats"]["passed"], 0)
        self.assertEqual(block, "demo:gem_block")
        self.assertEqual(item, "demo:gem")

    def test_missing_recipe_is_reported_as_a_probe_failure(self):
        oracle, horizon, carry, _, _ = fixture()
        del horizon["recipes"]["demo:gem_recipe"]
        result = harness.compare_mod("demo", oracle, horizon, carry, {}, set())
        self.assertFalse(result["passed"])
        self.assertEqual(result["checks"]["recipes"]["failed"], 1)
        self.assertIn("not loaded", result["checks"]["recipes"]["examples"][0])

    def test_item_stats_covers_tool_and_food_components(self):
        oracle_item = {
            "components": {
                "minecraft:max_damage": 32,
                "minecraft:tool": {
                    "rules": [{"blocks": ["minecraft:stone"], "speed": 4.0, "correct_for_drops": True}],
                    "default_mining_speed": 1.0,
                    "damage_per_block": 1,
                },
            },
            "attribute_modifiers": {"modifiers": []},
            "enchantment_value": 7,
            "food": {"nutrition": 4},
        }
        horizon_item = {
            "max_damage": 32,
            "tool": {
                "rules": [{"blocks": ["minecraft:stone"], "speed": 4.0, "correct_for_drops": True}],
                "default_mining_speed": 1.0,
                "damage_per_block": 1,
            },
            "modifiers": [],
            "enchantable": 7,
            "food": {"nutrition": 4},
        }
        checks = list(harness.item_stats(oracle_item, horizon_item))
        self.assertTrue(all(ok for ok, _ in checks), checks)


if __name__ == "__main__":
    unittest.main()
