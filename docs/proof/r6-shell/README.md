# Round 6 close-out proof frames — mask fade, dock tooltip pair, §26 journey states

Real Tauri dev build (`npm run tauri dev`, WebView2 driven over its CDP debugging port — same
method as `docs/proof/round4/round6/`), client area only, 125 % display scaling (a 1200×800 CSS
viewport is a 1500×1000 PNG). Branch `feat/shell-round6` at `89429b1` (includes the close-out
`f77184c` and the mask-fade fix `89429b1`). Data is the owner's local dev SQLite (schema v14);
one test message ("2.0.0 landing QA ping", Chat mode, `openai/gpt-oss-20b`) was sent from the
app into the existing dev conversation "Composer height regression at 5-6 lines" to capture the
streaming state — the provider stream stayed in "Assistant is responding" for the whole capture
window and never settled (see Known gaps).

Supplements `docs/proof/round4/round6/` (workspace menu, model picker, context popover, session
actions already covered there); this set covers the close-out behaviors and the §26 journey
states that set lacked.

| File | CSS viewport | PNG | MD5 | Colours | What I saw |
|---|---|---|---|---|---|
| `01-main-empty-1200x800.png` | 1200×800 | 1500×1000 | `0FDBECFFE4786CEB67E83BFBDF68BD6C` | 1946 | Cold start: dock is four destinations on the left rail; workspace row `📁 nx · ⑂ feat/shell-round6 ⌄` sits above the composer card; bottom row `Chat \| Agent`, `Full-auto ⌄`, empty context ring, `openai/gpt-oss-20b ⌄`, muted Send plate. Nothing clipped. |
| `02-sessions-open-1200x800.png` | 1200×800 | 1500×1000 | `E198D569EC749B05EBEF63AECEA218E8` | 2136 | Sessions panel open: filter field, `Active` group, dated rows, styled vertical scrollbar (thumb, no platform arrows). The bottom of the list **dissolves via the alpha mask** — no painted gradient bar anywhere (state after `89429b1`). |
| `03-sessions-scrolled-1200x800.png` | 1200×800 | 1500×1000 | `ACF86CBCF0BC4D0F5FC6C70B5A2F236F` | 1981 | Same list scrolled (`scrollTop` 2432): older rows ("23 окт.", "Workspace roots switcher", "Composer height regression at 5-6 lin…"), the bottom band melts into the canvas over real content, thumb mid-track. |
| `04-search-mid-1200x800.png` | 1200×800 | 1500×1000 | `48F58C6CBE62C0617C168D765E357515` | 1896 | Filter mid-typing (`se`, caret in field): list already narrowed to the 2 matching rows. |
| `05-search-results-1200x800.png` | 1200×800 | 1500×1000 | `42277D5E8B1C52F7BA895E49AE44DB33` | 2524 | Filter `set`: honest empty state — "No sessions match that filter", panel keeps its shape, no layout jump. |
| `06-dock-expanded-focus-1200x800.png` | 1200×800 | 1500×1000 | `2395496652FC8C5E6E523B321E533E52` | 2129 | **Close-out pair, expanded side:** panel open AND the dock Sessions trigger focused — **no tooltip chip**, no clipped "S" at the panel's left edge (the `f77184c` behavior, visually confirmed). |
| `07-dock-hover-tooltip-1200x800.png` | 1200×800 | 1500×1000 | `ACB066A8CE356189682FEC30F502E5A2` | 2243 | **Close-out pair, collapsed side:** panel closed, trigger focused — the `Sessions` tooltip chip sits right of the button, focus ring on the trigger, nothing clipped. |
| `08-conversation-loaded-1200x800.png` | 1200×800 | 1500×1000 | `ACA7CB6B9B45D56B033F766CD539D80B` | 3797 | A real conversation loaded: 3 tabs in the strip, active tab "Composer height regression at 5…", thread shows the user message "Seeded capture fixture — …" (17 окт.); the sessions panel is still open above it. |
| `09-tabs-two-1200x800.png` | 1200×800 | 1500×1000 | `AFF06B24E73E2910F39CB7B992FF754C` | 2739 | Two tabs: strip with two "New Conversation" tabs (second active, hairline underline), `nx` workspace chip and the pane menu in the header, `Split` control at the right edge. |
| `10-tabs-split-1200x800.png` | 1200×800 | 1500×1000 | `558498FD2480F76B228F41F81F9E50BC` | 4084 | Split active: two panes — loaded conversation left, New Conversation right — each with its own header and composer; `Close split` replaced the Split control; a `[2]` count badge sits on the middle tab. |
| `11-send-early-1200x800.png` | 1200×800 | 1500×1000 | `1730BC8B69BC6899ECC71DE6DBECBFED` | 4423 | Send +1.5 s, Chat mode (left composer switched: `+` attach, no Full-auto): the assistant placeholder row starts appearing at the thread top, partially behind the still-open sessions panel. Mid-flight state, honestly occluded. |
| `12-send-settled-1200x800.png` | 1200×800 | 1500×1000 | `429383FC491BE54FFC1F01A7DD37EA10` | 4672 | Panel closed: clean split view — thread with the user message and the **"Assistant is responding"** streaming placeholder (spinner). The stream never advanced past this state during the capture window. |

Every frame is distinct (unique MD5) and carries >1890 colours.

## Known gaps

- **Error turn not captured.** The test send's provider stream hung in "Assistant is responding"
  until capture ended; no error surface was reached. Backend error mapping is covered by Rust
  tests (CommandError sentinels); the visual error-turn frame is deferred to the F3 campaign
  phase where send/error states get reworked anyway (markdown renderer).
- **Reduced-motion frame not captured.** It requires an OS-level animation toggle; the
  reduced-motion coverage is code-pinned instead (`src/styles/motion.css:182` global gate,
  `context.css:313`, `conversation.css:1282`, `src/lib/useReducedMotion.ts` tests) and gets a
  live pass in the F5 motion phase (P7 landing).
- The streaming hang itself is **not a shell regression**: the shell correctly holds the
  placeholder + spinner; the stall is on the provider side (gpt-oss-20b endpoint).
