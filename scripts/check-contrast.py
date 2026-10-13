#!/usr/bin/env python3
"""NEX-DS-007 gate: every text tier >= 4.5:1 on every text-bearing surface.

Reads the token hex values straight out of src/styles/tokens.css so the
check tracks the source of truth. Dark `bright` (#3d3c46) is excluded:
it is a non-text rest surface with zero text consumers (documented in
the CONTRAST LEDGER).

The four shell surface TIER ALIASES (canvas/elevated/nested/floating)
are defined as `var(--nex-sys-color-...)`, not as hex, so they must be
resolved before they can be gated — otherwise a tier alias is invisible
to this script and aliasing one onto an ungated role (e.g. `floating`
onto the excluded dark `bright`) would pass silently. Each alias is
resolved per theme, asserted to land inside that theme's gated surface
set, and then gated like any other surface. Exit 0 iff all pairs pass.
"""
import re
import sys
from pathlib import Path

TOKENS = Path(__file__).resolve().parent.parent / "src" / "styles" / "tokens.css"

TIER_ALIASES = ["surface-canvas", "surface-elevated",
                "surface-nested", "surface-floating"]


def lum(h: str) -> float:
    h = h.lstrip("#")
    r, g, b = (int(h[i : i + 2], 16) / 255 for i in (0, 2, 4))

    def f(c: float) -> float:
        return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4

    return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)


def ratio(a: str, b: str) -> float:
    la, lb = lum(a), lum(b)
    hi, lo = max(la, lb), min(la, lb)
    return (hi + 0.05) / (lo + 0.05)


def resolve(defs: dict, name: str) -> tuple[str, str]:
    """Follow var() indirection from a token to the hex behind it.

    Returns (resolved_role_name, hex). The shell tier aliases are
    declared as var() references to M3 container roles, so this is
    what makes them visible to the gate.
    """
    seen: set[str] = set()
    while True:
        if name in seen:
            raise ValueError(f"circular token alias at {name}")
        seen.add(name)
        value = defs[name]
        if value.startswith("#"):
            return name, value
        name = value.removeprefix("var(--nex-sys-color-").removesuffix(")")


def main() -> int:
    text = TOKENS.read_text()

    def block_defs(block: str) -> dict:
        # Hex definitions AND var() aliases — a tier alias is a def too.
        return dict(re.findall(
            r"--nex-sys-color-([a-z-]+):\s*"
            r"(#[0-9a-fA-F]{6}|var\(--nex-sys-color-[a-z-]+\))",
            block))

    dark_body = text.split(".nex-theme-dark {", 1)[1].split(
        "LIGHT THEME", 1)[0]
    light_body = text.split('.nex-theme-light {', 1)[1].split(
        "First-paint note", 1)[0]
    hc_body = text.split(".nex-theme-high-contrast {", 1)[1].split(
        "CONTRAST LEDGER", 1)[0]
    dark, light, hc = (block_defs(b) for b in (dark_body, light_body, hc_body))

    dark_text = ["surface", "surface-dim", "surface-container-lowest",
                 "surface-container-low", "surface-container",
                 "surface-container-high", "surface-container-highest"]
    light_text = ["surface", "surface-dim", "surface-container-lowest",
                  "surface-container-low", "surface-container",
                  "surface-container-high", "surface-container-highest",
                  "surface-bright"]
    hc_text = ["surface", "surface-dim", "surface-container-lowest",
               "surface-container-low", "surface-container",
               "surface-container-high", "surface-container-highest",
               "surface-bright"]

    tiers = ["on-surface", "on-surface-variant",
             "on-surface-tertiary", "on-surface-faint"]
    ok = True
    pairs = 0

    for name, vals, gated in (("dark", dark, dark_text),
                              ("light", light, light_text),
                              ("high-contrast", hc, hc_text)):
        # 1. Every shell tier alias must resolve into the gated set, so
        #    aliasing a tier onto an ungated rest surface fails loudly.
        surfaces = list(gated)
        for alias in TIER_ALIASES:
            role, hexv = resolve(vals, alias)
            inside = role in gated
            if not inside:
                ok = False
            print(f"{'PASS' if inside else 'FAIL'} {name:14s} tier {alias:16s} -> {role:26s}"
                  f" {hexv} {'(gated)' if inside else '(NOT GATED)'}")
            surfaces.append(alias)

        # 2. Text tiers on every gated surface, resolved through aliases.
        for tier in tiers:
            fg = vals[tier]
            for s in surfaces:
                bg = resolve(vals, s)[1]
                r = ratio(fg, bg)
                status = "PASS" if r >= 4.5 else "FAIL"
                if r < 4.5:
                    ok = False
                pairs += 1
                print(f"{status} {name:14s} {tier[11:] or 'base':9s} on {s:26s} {r:.2f}:1")

    print(f"PAIRS: {pairs}  ({len(TIER_ALIASES)} tier aliases x "
          f"{len(tiers)} text tiers x 3 themes resolved + gated)")
    print("RESULT:", "ALL PASS" if ok else "FAILURES PRESENT")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
