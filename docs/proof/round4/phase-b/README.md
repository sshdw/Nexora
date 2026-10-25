# Phase B proof frames — shell skeleton

Real Tauri build (`npm run tauri dev`, `target/debug/nexora.exe`, schema v14), not a
dev-server mock. Captured by the orchestrator with `PrintWindow(PW_CLIENTONLY |
PW_RENDERFULLCONTENT)` on the live window, client area only (no title bar).

Display scaling on the capture machine is 125%, so a CSS viewport of W×H produces a
W×1.25 × H×1.25 PNG. The CSS size is what the proof matrix names; the pixel size is
what the file holds.

| File | CSS viewport | PNG | MD5 | Colours (full scan) | What I saw |
|---|---|---|---|---|---|
| `01-1200x800-dark.png` | 1200×800 | 1500×1000 | `229BEBCBA7D4F7CAD0DA1D2F52F3E940` | 955 | Dock flush to the left edge, square left corners, rounded right side, vertically centred on the window; empty state centred; no line or sliver on any edge. |
| `02-1536x800-dark.png` | 1536×800 | 1920×1000 | `036A6EBA5B207E3EAE70760DB68AF242` | 955 | Same shell at the widest check point; nothing crowds or clips; Dock still pinned to the edge, not to a content column. |
| `03-1024x720-dark.png` | 1024×720 | 1280×900 | `4D0C904FDE2084027E78034E607D565D` | 955 | Smallest allowed window: Dock 312 CSS px tall with clear space above and below, empty state intact, no edge artefact. |
| `04-1200x800-light.png` | 1200×800 | 1500×1000 | `6CA5853B834CEEC150D51686197A7D1B` | 1018 | Light theme: canvas, elevated Dock surface and text all legible; one hairline seam separates the Dock; no amber tint anywhere. |
| `05-1024x720-light.png` | 1024×720 | 1280×900 | `2F59EDCC047841614CA3FD7749EF3AB9` | 1018 | Light theme at the minimum window: same legibility and alignment, nothing touches an edge. |

All five frames are distinct (unique MD5) and carry >900 distinct colours, i.e. they are
rendered frames rather than flat mock panels.

## What these frames prove / do not prove

- Prove: window opens at 1200×800 by default; the shell is a single column with the Dock
  as a fixed overlay; Geist renders; both themes resolve from the same token set; no
  fixed element bleeds a shadow into the viewport at rest.
- Window minimum: `WM_GETMINMAXINFO` of the live window reports a 1298×947 px minimum
  track, i.e. 1038×758 logical = **1024×720 client + 14×38 frame** — the R4-09 minimum is
  enforced by the OS window, not only by the config file.
- Do not prove: running / approval / completed / review / VCS / Tasks states, RU copy, or
  200 % zoom. Those rows of the §9 matrix belong to Phases D–F and are still open.

## Visual QA verdict (subagent, frames only)

APPROVE — 5/5 Tauri frames PASS and edge-clean; §26 checklist 12/13 PASS.
Open findings, kept as-is for the owner's decision:
item 6 (identity) FAIL — the empty state is a generic placeholder with no Nexora signature
and no visible way to start beyond the rail's "+"; the empty block is optically centred on
the full window, so it leans ~30 CSS px toward the Dock; the rail's icon rhythm is uneven
(+ sits loose, the third glyph carries more ink).
