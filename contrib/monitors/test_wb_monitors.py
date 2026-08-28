#!/usr/bin/env python3
"""Tests for wb-monitors' pure logic. Run: python3 contrib/monitors/test_wb_monitors.py

The live behaviour (does it actually move the outputs?) is only testable against a compositor; what
is testable here is the part that decides WHERE each display goes, which is where the bugs that
matter live: order that depends on plug order, a display that lands on top of another, a config that
blacks the machine out.
"""
import importlib.machinery
import importlib.util
import os
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
# The script has no .py extension, so give importlib an explicit source loader.
_loader = importlib.machinery.SourceFileLoader("wb_monitors", os.path.join(HERE, "wb-monitors"))
spec = importlib.util.spec_from_loader(_loader.name, _loader)
wb = importlib.util.module_from_spec(spec)
_loader.exec_module(wb)


def out(name, make="", model="", serial="", w=1920, h=1080, x=0, y=0, scale=1.0, enabled=True):
    return {"name": name, "make": make, "model": model, "serial": serial, "enabled": enabled,
            "w": w, "h": h, "refresh": 60.0, "x": x, "y": y, "scale": scale, "modes": []}


DELL = out("DP-1", "Dell Inc.", "DELL U2723QE", "ABC123", 3840, 2160)
LG = out("DP-2", "LG Electronics", "LG HDR 4K", "DEF456", 3840, 2160)
LAPTOP = out("eDP-1", "AU Optronics", "0x1234", "", 2560, 1600)

CFG = {
    "policy": {"align": "top", "unknown": "append"},
    "display": [
        {"rank": 2, "make": "LG Electronics", "model": "LG HDR 4K", "serial": "DEF456"},
        {"rank": 1, "make": "Dell Inc.", "model": "DELL U2723QE", "serial": "ABC123"},
    ],
}


def order(resolved):
    return [r["name"] for r in resolved]


class Matching(unittest.TestCase):
    def test_named_fields_must_all_match_but_omitted_ones_are_wildcards(self):
        self.assertTrue(wb.matches({"model": "DELL U2723QE"}, DELL))
        self.assertTrue(wb.matches({"make": "Dell Inc.", "serial": "ABC123"}, DELL))
        self.assertFalse(wb.matches({"make": "Dell Inc.", "serial": "WRONG"}, DELL))

    def test_matching_ignores_case(self):
        self.assertTrue(wb.matches({"model": "dell u2723qe"}, DELL))

    def test_an_empty_rule_matches_nothing(self):
        # Otherwise a stray [[display]] with only a rank would claim every display on the desk.
        self.assertFalse(wb.matches({"rank": 1}, DELL))


class Ordering(unittest.TestCase):
    def test_rank_wins_over_the_order_outputs_were_announced_in(self):
        for live in ([DELL, LG], [LG, DELL]):
            resolved, _, _ = wb.plan(CFG, live)
            self.assertEqual(order(resolved), ["DP-1", "DP-2"], f"live = {order(resolved)}")

    def test_the_same_displays_on_swapped_ports_still_order_the_same(self):
        # The whole point: identity is EDID, so renaming the connectors changes nothing.
        swapped = [dict(DELL, name="DP-2"), dict(LG, name="DP-1")]
        resolved, _, _ = wb.plan(CFG, swapped)
        self.assertEqual([r["ident"] for r in resolved],
                         ["Dell Inc. DELL U2723QE ABC123", "LG Electronics LG HDR 4K DEF456"])
        self.assertEqual([r["x"] for r in resolved], [0, 3840])

    def test_positions_are_contiguous_with_no_gaps_or_overlaps(self):
        resolved, _, _ = wb.plan(CFG, [DELL, LG, LAPTOP])
        xs = [r["x"] for r in resolved]
        widths = [r["logical_w"] for r in resolved]
        self.assertEqual(xs, [0, 3840, 7680])
        for i in range(1, len(xs)):
            self.assertEqual(xs[i], xs[i - 1] + widths[i - 1], "displays must abut exactly")

    def test_unlisted_display_is_appended_deterministically(self):
        a, _, _ = wb.plan(CFG, [LAPTOP, DELL, LG])
        b, _, _ = wb.plan(CFG, [LG, LAPTOP, DELL])
        self.assertEqual(order(a), ["DP-1", "DP-2", "eDP-1"])
        self.assertEqual(order(a), order(b), "arrival order must not leak into the layout")

    def test_unknown_disable_policy_turns_the_stranger_off(self):
        cfg = {"policy": {"align": "top", "unknown": "disable"}, "display": CFG["display"]}
        resolved, disabled, notes = wb.plan(cfg, [DELL, LG, LAPTOP])
        self.assertEqual(order(resolved), ["DP-1", "DP-2"])
        self.assertEqual([o["name"] for o in disabled], ["eDP-1"])
        self.assertTrue(any("eDP-1" in n for n in notes))

    def test_scale_changes_the_logical_width_the_next_display_starts_at(self):
        cfg = {"policy": CFG["policy"], "display": [
            {"rank": 1, "model": "DELL U2723QE", "scale": 2.0},
            {"rank": 2, "model": "LG HDR 4K"},
        ]}
        resolved, _, _ = wb.plan(cfg, [DELL, LG])
        self.assertEqual(resolved[0]["logical_w"], 1920, "3840 at scale 2")
        self.assertEqual(resolved[1]["x"], 1920, "the neighbour abuts the LOGICAL width")

    def test_align_policy_places_a_shorter_display_top_centre_or_bottom(self):
        short = out("HDMI-A-1", "Acme", "Small", "S1", 1920, 1080)
        cfg_rules = [{"rank": 1, "model": "DELL U2723QE"}, {"rank": 2, "model": "Small"}]
        got = {}
        for align in ("top", "center", "bottom"):
            resolved, _, _ = wb.plan({"policy": {"align": align, "unknown": "append"},
                                      "display": cfg_rules}, [DELL, short])
            got[align] = resolved[1]["y"]
        self.assertEqual(got["top"], 0)
        self.assertEqual(got["center"], (2160 - 1080) // 2)
        self.assertEqual(got["bottom"], 2160 - 1080)

    def test_only_an_unlisted_display_present_still_gets_it_placed(self):
        resolved, disabled, _ = wb.plan(CFG, [LAPTOP])
        self.assertEqual(order(resolved), ["eDP-1"])
        self.assertEqual(disabled, [])
        self.assertEqual(resolved[0]["x"], 0)

    def test_disable_policy_never_leaves_the_machine_with_nothing_enabled(self):
        # Every ranked display unplugged AND unknown = disable: the naive result is zero enabled
        # outputs, i.e. a black screen with no way back. The safety net enables what is present.
        cfg = {"policy": {"align": "top", "unknown": "disable"}, "display": CFG["display"]}
        resolved, disabled, notes = wb.plan(cfg, [LAPTOP])
        self.assertEqual(order(resolved), ["eDP-1"])
        self.assertEqual(disabled, [])
        self.assertTrue(any("enabling everything present" in n for n in notes), notes)

    def test_mode_override_is_used_for_placement_before_it_is_applied(self):
        cfg = {"policy": CFG["policy"], "display": [
            {"rank": 1, "model": "DELL U2723QE", "mode": "1920x1080@60"},
            {"rank": 2, "model": "LG HDR 4K"},
        ]}
        resolved, _, _ = wb.plan(cfg, [DELL, LG])
        self.assertEqual((resolved[0]["w"], resolved[0]["h"]), (1920, 1080))
        self.assertEqual(resolved[1]["x"], 1920, "the neighbour follows the NEW mode, not the old")


class Idempotence(unittest.TestCase):
    def test_a_layout_already_correct_is_reported_as_such(self):
        live = [dict(DELL, x=0, y=0), dict(LG, x=3840, y=0)]
        resolved, disabled, _ = wb.plan(CFG, live)
        self.assertTrue(wb.already_applied(resolved, disabled, live))

    def test_a_moved_display_is_not(self):
        live = [dict(DELL, x=0, y=0), dict(LG, x=99, y=0)]
        resolved, disabled, _ = wb.plan(CFG, live)
        self.assertFalse(wb.already_applied(resolved, disabled, live))


class TextParsing(unittest.TestCase):
    SAMPLE = """HEADLESS-2 "Headless output 3"
  Make: (null)
  Model: (null)
  Serial: (null)
  Enabled: yes
  Modes:
    1920x1080 px (current)
  Position: 1280,0
  Transform: normal
  Scale: 1.000000
DP-1 "Dell Inc. DELL U2723QE ABC123"
  Make: Dell Inc.
  Model: DELL U2723QE
  Serial: ABC123
  Enabled: yes
  Modes:
    3840x2160 px, 59.997000 Hz (preferred)
    3840x2160 px, 60.000000 Hz (current)
  Position: 0,0
  Transform: normal
  Scale: 1.500000
"""

    def test_parses_the_0_3_listing_when_json_is_unavailable(self):
        got = wb.parse_text(self.SAMPLE)
        self.assertEqual([o["name"] for o in got], ["HEADLESS-2", "DP-1"])
        headless, dell = got
        self.assertEqual((headless["w"], headless["h"], headless["x"]), (1920, 1080, 1280))
        self.assertEqual(headless["make"], "", "(null) is not an EDID string")
        self.assertEqual((dell["make"], dell["model"], dell["serial"]),
                         ("Dell Inc.", "DELL U2723QE", "ABC123"))
        self.assertEqual((dell["w"], dell["h"]), (3840, 2160))
        self.assertAlmostEqual(dell["scale"], 1.5)
        self.assertTrue(dell["enabled"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
