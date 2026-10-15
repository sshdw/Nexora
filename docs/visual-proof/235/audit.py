#!/usr/bin/env python3
"""Amber/blue pixel audit for the #235 visual proof.

Reproducible, self-contained. Run from the repo root:

    python3 docs/visual-proof/235/audit.py

It crops the window-manager title bar off the top of every frame
(openbox's own light-blue decoration, not the app) and then reports,
per shot:
  * blue  : hue 195-275 deg, saturation > 0.25, value > 0.15
  * amber : the same saturation/value gate, hue OUTSIDE 195-275
  * tint  : RGB channel spread > 24/255, i.e. max(r,g,b) - min(r,g,b).
            Neither of the other two is usable as a "how neutral is
            this UI" metric: OKLCH `c` is a different unit, and HSV
            saturation is max-min over max, so a near-neutral dark
            tone like (26,25,31) scores s=0.19. The channel spread
            is unit-free and is what actually reads as colour.
Percentages are of the cropped frame's pixels.
"""
import colorsys
import glob
import os
import subprocess
import sys

# The app's client area starts below openbox's title bar, which paints a
# light-blue (#ADD3FF) band ~19px tall at y=88..106 on this 1600x1000
# screen. The crop below starts at 120 so no row of the title bar can
# reach the measurement. Verified by running the row-scan in the PR body.
APP_CROP = "1600x880+0+120"


def load(path):
    return subprocess.run(
        ["convert", path, "-crop", APP_CROP, "+repage", "-depth", "8", "rgb:-"],
        capture_output=True,
    ).stdout


def audit(path):
    raw = load(path)
    total = len(raw) // 3
    blue = amber = tint = 0
    for i in range(0, len(raw) - 2, 3):
        r8, g8, b8 = raw[i], raw[i + 1], raw[i + 2]
        r, g, b = r8 / 255, g8 / 255, b8 / 255
        if max(r8, g8, b8) - min(r8, g8, b8) > 24:
            tint += 1
        h, s, v = colorsys.rgb_to_hsv(r, g, b)
        if s > 0.25 and v > 0.15:
            if 195 <= h * 360 <= 275:
                blue += 1
            else:
                amber += 1
    return total, blue, amber, tint


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    shots = sorted(glob.glob(os.path.join(here, "*.png")))
    if not shots:
        sys.exit("no shots found next to this script")
    print(f"{'shot':32s} {'px':>9s} {'blue':>6s} {'amber':>7s} {"tint spread>24":>16s}")
    worst_blue = worst_amber = worst_tint = 0.0
    for shot in shots:
        total, blue, amber, tint = audit(shot)
        name = os.path.basename(shot)
        print(
            f"{name:32s} {total:9d} {blue:6d} {amber:7d} "
            f"{tint:8d} ({tint / total * 100:.3f}%)"
        )
        worst_blue = max(worst_blue, blue / total * 100)
        worst_amber = max(worst_amber, amber / total * 100)
        worst_tint = max(worst_tint, tint / total * 100)
    print()
    print(f"worst blue  : {worst_blue:.3f}% of frame")
    print(f"worst amber : {worst_amber:.3f}% of frame")
    print(f"worst tint  : {worst_tint:.3f}% of frame (spread > 24)")


if __name__ == "__main__":
    main()
