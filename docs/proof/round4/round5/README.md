# Round 5 proof frames — the composer is the screen

Real Tauri build (`npm run tauri dev`), client area only, 125 % display scaling (a 1200×800
CSS viewport is a 1500×1000 PNG). Single session open, pointer parked off every hover target.

| File | CSS viewport | PNG | MD5 | Colours | What I saw |
|---|---|---|---|---|---|
| `01-empty-1200x800.png` | 1200×800 | 1500×1000 | `A705B8D3CF2E16BF2E3F5F6E8825FE30` | 2542 | A new conversation is **only the composer**, centred, with the caret already in the field — no "No messages yet", no empty-state copy anywhere. Above the card one read-only line: `📁 nx · ⑂ feat/composer-round5`. In the bottom row, left to right: `Chat | Agent`, `● Supervised ⌄`, the context ring (dashed = no token data yet), `openai/gpt-oss-20b ⌄`, and an unfilled arrow. |
| `02-empty-1024x720.png` | 1024×720 | 1280×900 | `E37447ABDD495B8EDBA5012BD9501EEC` | 2542 | Same at the minimum window: the 760 card keeps its width and stays centred, the workspace line ellipsizes nothing at this width, the ring and model still fit on one row, nothing clips. |
| `03-empty-1536x800.png` | 1536×800 | 1920×1000 | `003EA2B4424E1D85F8D466E787A69DEA` | 2542 | Same at the widest check point: the card stays 760 wide and centred, the extra width becomes air on both sides instead of a wider reading measure. |
| `04-armed-send-1200x800.png` | 1200×800 | 1500×1000 | `3F4B8F0C4774BDA3BA01A7FEA7AE299E` | 2507 | The same button with a draft in the field: the arrow is now a **light filled rounded square with a dark arrow** — the one high-contrast element on the screen. Ring, autonomy trigger and model are unchanged, so the state change is carried by the button alone. |

Every frame is distinct and carries >900 colours.

**Caveat on frame 04.** The draft text reads `85/` because the capture harness types through
the OS keyboard layout and that layout was not the one the string was written for. It is a
harness artefact, not a product state — the frame exists to show the armed button, and the
draft's content is irrelevant to that. The empty frames carry no such artefact.

## What these frames prove / do not prove

- Prove: an empty conversation renders the composer as the whole screen (D1) with focus
  already in the field; the workspace line shows the real folder basename and the real branch
  from `gitInfo()` and is read-only, with no chevron promising a menu (D3); the context ring
  sits to the left of the model trigger and reads as an empty dashed track when the
  conversation has no token data (D4); the Send button is one seat that changes shape —
  unfilled arrow when there is nothing to send, filled square when there is (D2, R5-07); the
  icons are one family at one weight (D5).
- Do not prove: the stop state and the interrupt itself (needs a live provider run — the
  frame set was shot without an API key, so no run could start); the ring's known-value arc
  (this database has no persisted token usage, so `n/a` is the only state reachable); the
  agent-steps line (same reason — no run, no steps); hover/focus-visible/pressed; RU; light
  theme; 200 % zoom; motion.
