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
# light-blue (#ADD3FF) band ~19px tall at y=88..106 on a 1600x1000
# screen. Each frame is first cropped to the client area (title bar off
# the top, Xvfb dead space off the right and bottom), then the crop is
# SELF-JUSTIFIED by a per-row, per-column scan: the script finds the
# bounding box of rows/columns that are not pure black and reports it,
# so a reader can see the measurement covers the window and nothing
# else. Run the scan verbosely with `--scan` to print every frame's
# detected box.
SCAN_TOP_MARGIN = 120  # never above the title bar's last blue row (106)


def frame_box(raw, width, height):
    """Bounding box of rows/columns that are not pure black.

    Returns (x0, y0, x1, y1) in frame coordinates. Pure black is the
    Xvfb backdrop on this rig, never the app (the canvas is #101014
    at its darkest), so the box is the window and only the window.
    """
    top = SCAN_TOP_MARGIN
    bottom = height
    for y in range(SCAN_TOP_MARGIN, height):
        base = y * width * 3
        if any(raw[base : base + width * 3]):
            top = y
            break
    for y in range(height - 1, SCAN_TOP_MARGIN - 1, -1):
        base = y * width * 3
        if any(raw[base : base + width * 3]):
            bottom = y + 1
            break
    left = 0
    right = width
    for x in range(width):
        if any(raw[(y * width + x) * 3] for y in range(top, bottom)):
            left = x
            break
    for x in range(width - 1, -1, -1):
        if any(raw[(y * width + x) * 3] for y in range(top, bottom)):
            right = x + 1
            break
    return (left, top, right, bottom)


def load_frame(path):
    out = subprocess.run(
        ["convert", path, "-depth", "8", "rgb:-"], capture_output=True
    ).stdout
    # Frame size comes from the file itself, not from an assumption.
    info = subprocess.run(
        ["identify", "-format", "%w %h", path], capture_output=True, text=True
    ).stdout.split()
    return out, int(info[0]), int(info[1])


def audit(path, verbose=False):
    raw, width, height = load_frame(path)
    box = frame_box(raw, width, height)
    x0, y0, x1, y1 = box
    if verbose:
        print(f"  box {os.path.basename(path)}: x {x0}..{x1} y {y0}..{y1}")
    blue = amber = tint = 0
    total = (x1 - x0) * (y1 - y0)
    for y in range(y0, y1):
        for x in range(x0, x1):
            i = (y * width + x) * 3
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
    verbose = "--scan" in sys.argv
    print(f"{'shot':32s} {'px':>9s} {'blue':>6s} {'amber':>7s} {"tint spread>24":>16s}")
    worst_blue = worst_amber = worst_tint = 0.0
    for shot in shots:
        total, blue, amber, tint = audit(shot, verbose)
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
