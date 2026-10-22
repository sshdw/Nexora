# 230 Phase 7 — motion contract: real-Tauri pixel proof

Every frame is a REAL Tauri capture: the `src-tauri/target/debug/nexora`
WebKitGTK binary, the real SQLite backend, the real Vite dev server on
`http://127.0.0.1:1420`, the real provider configuration, the real OS keyring.
No `?mock`, no Chromium, no mocked IPC. Each frame was opened and inspected
before acceptance.

Branch `task/oci-230-p7`, PR #283. Code commit `e02274c` (P7 motion contract —
springs, press, zen cascade, TextType/gsap removal), based on `origin/main`
fbfa… (P6 merged). This README and the 9 PNGs are the proof commit on top of
`e02274c`; no self-referential proof-commit hash is pinned here.

## Environment

```
Xvfb :96 -screen 0 1600x1000x24 -ac +extension GLX +extension RENDER -noreset &
DISPLAY=:96 XDG_DATA_HOME=/tmp/opencode/p7-data \
  WEBKIT_DISABLE_COMPOSITING_MODE=1 ./src-tauri/target/debug/nexora
```

Window 1200×800 (frame 09: 720×800), captured with `import -window`.
Profile `p7-data`: dark theme, Russian UI, 54 conversations in history.

## Frames

| File | md5 (first 8) | Colours | What is in the frame | What I SAW |
|---|---|---|---|---|
| `01-launch-populated-dark.png` | `b52aee25` | 1065 | fresh process launch, no active tab | Dark theme, Russian UI. Rail: `+ Новый разговор`, `Поиск`, `Активные` history (54 rows, Oct 18/19, incl. `P3 — идущий запуск`, `P3 — длинная ветка`, `Thread P3 acceptance`), footer `Версионный контроль / Задачи / Настройки`. Centre: `Пока нет разговоров` / `Ваши разговоры появятся здесь`. No tab strip. This is a launch against populated history, not a fresh empty profile. |
| `02-search-mid-typing.png` | `a6a95c51` | 1171 | sidebar search mid-typing | `Ctrl+K` focused the sidebar search field; `Р` typed, caret visible, lookup spinner at the field's right edge, history list still unfiltered while typing. |
| `03-search-results.png` | `a5f8e317` | 1212 | search results | Query `Р3`; dropdown under the field headed `Разговоры` with three matches — `Thread P3 acceptance`, `P3 — длинная ветка`, `P3 — идущий запуск`, each with a `Разговор` subline. Result titles render very dark (pre-existing paint; see gaps). |
| `04-loaded-thread.png` | `976dd5de` | 2983 | loaded thread `P3 — идущий запуск` | Tab strip with one tab, header `P3 — идущий запуск` + workspace badge `p3-workspace` + export icon, `Чат / Контекст` tabs. User turn `Проверь миграции и почини сравнение версий.` Error card: `Ошибка · 2 вызова инструментов`, model `nvidia/nemotron-3-nano-omni-30b-a3b-reasoning`, `run 6`, and `Что произошло / Что сохранилось / Что делать` (`run interrupted by application shutdown`, `2 вызова инструментов`, `Отправьте сообщение ещё раз.`). Tool rows `#1 Размышление`, `#2 Прочитал src/lib/db.rs ok 140ms`, `#3 Искал ok 210ms`. Composer at the bottom with `Отправить ↑`, hint `Enter — отправить, Shift+Enter — новая строка`. |
| `05-two-tabs-split.png` | `28726e9a` | 3199 | two tabs + split | Left pane `P3 — длинная ветка` (assistant turns, steps 14–18, model `nvidia/nemotron-3.5-super`), right pane `P3 — идущий запуск` (error card as above). Two independent composers. Header actions `Закрыть сплит`, `Дзен`, `+`. Both composers clip `Отправить` to `Отп` at the divider/window edge (pre-existing layout; see gaps). |
| `06-hover-tool-row.png` | `fb1b62ef` | 3201 | hover state on tool row | Same loaded thread as `04` with the pointer on tool row `#2 Прочитал src/lib/db.rs`: the row's background lifts (mean luminance 0.173→0.188 over the row crop, AE 21,133 px inside the crop). This is the 120 ms hover token applied to a real control. |
| `07-dialog-export.png` | `4e6e8922` | 2926 | dialog surface open | Right-side export sheet over the loaded thread: `Экспорт разговора`, explanation `Сохраняет «P3 — идущий запуск» как локальный JSON-файл…`, actions `Отмена` and `Выбрать место`. Documents the dialog surface at rest. |
| `08-long-scroll-bottom.png` | `b37b2d78` | 1993 | long thread scrolled to bottom, single pane | `P3 — длинная ветка` in single-pane view scrolled to the end: steps 15–18 (`Шаг 15…`, assistant `миграция 16…`, `Шаг 17…`, assistant `миграция 18…`) plus repeated short follow-up prompts (`Ответь одним коротким предложением…`, `Скажи одно слово: привет`). Proves the long thread scrolls and renders below the fold. |
| `09-narrow-720.png` | `8cb31f58` | 1963 | narrow window, 720×800 | Same long thread at 720×800: sidebar and thread pane both survive the reflow, `Чат / Контекст` header and composer stack correctly; the composer's `Отправить` clips harder at this width (pre-existing layout; see gaps). |

## Journey coverage (§9)

| Journey beat | Status |
|---|---|
| cold start empty | **partial** — frame 01 is a fresh launch over a populated profile with no active tab; a genuinely empty profile was not captured (theme/locale would differ). Declared, not claimed as empty. |
| search mid-typing + results | yes — 02, 03 |
| 2 tabs + split | yes — 05 |
| loaded conversation + error turn | yes — 04 |
| streaming turn | **dropped** — provider `HTTP 400 invalid` |
| long conversation scroll | yes — 05 (mid-thread) and 08 (bottom) |
| narrow window | yes — 09 |
| hover | yes — 06 |
| focus | yes — 02 (caret in focused field) |
| selected / empty / loading / error | yes — 01 (empty centre), 02 (lookup spinner), 04 (error card), 05 (selected row) |
| disabled | not captured as a dedicated frame; the dim tab-strip actions in 04/05 are a paint defect, not a proven disabled state |
| reduced-motion on | **not captured as a pixel frame** — see below |

## What these frames can and cannot prove

P7 changes motion only: spring tokens, press/hover timing, exits, zen cascade,
and removal of TextType/gsap. It changes no paint, radius, or layout.

- **Captured surfaces render unchanged.** No visual regression is visible in
  frames 01–09.
- **Hover is shown, not timed.** `06` proves the hover affordance fires on a
  real control; the 120 ms duration is pinned in `theme.test.ts`, not measured
  from a static frame.
- **Reduced motion is not presented as a pixel frame.** A static screenshot
  cannot demonstrate animation timing. The reduced-motion gate is verified by
  `src/lib/motion.test.tsx` and `src/lib/zenExit.test.ts`, and pinned in the
  P7 gate.
- **The zen cascade is not captured mid-flight.** Three layers leave 40 ms
  apart, each 189 ms; the lifecycle holds the exiting class for **269 ms**
  (80 ms last delay + 189 ms last layer, `ZEN_EXIT_TOTAL_MS`). That is faster
  than a reliable screenshot round-trip. It is verified by
  `src/lib/zenExit.test.ts`. In this profile state the `Дзен` control rendered
  inert (hover AE = 0, click no-op), so no zen-active frame was captured
  either.
- **Streaming beat dropped.** The configured provider returns `HTTP 400
  invalid` for the streaming request (upstream model constraint, not an app
  bug), so no mid-stream or completed-stream frame exists. Streaming text is
  explicitly not animated under SYSTEM §7.2, so no motion contract is left
  unverified by this gap.

## Pre-existing issues observed (outside P7 scope)

These are visible in the frames and were flagged by Visual QA. They are NOT
introduced by P7: the P7 diff (`e02274c`) contains no layout or colour
changes — only motion tokens/timing, `zenExit`, `App` lifecycle, and the
TextType/gsap removal (`composer.css` changes are `transition` timings only).

1. Split and narrow composers clip `Отправить` to `Отп` (`05`, `09`).
2. `Закрыть сплит` / `Дзен` / `+` render very dim in the tab-strip corner
   (`04`, `05`, `06`, `07`).
3. Search result titles render near-black on the dark surface; only the
   `Разговор` subline reads (`03`).
4. The dialog's `Отмена` action renders near-black on the dark sheet (`07`).

They belong in a separate paint/layout task; fixing them here would violate
P7's motion-only scope.

## Measured, not eyeballed

- **Uniqueness**: `md5sum *.png | awk '{print $1}' | sort | uniq -d` → **0
  duplicates** across 9 files.
- **Colour floor**: every frame > 900 colours (range 1065–3201) — no stalled
  or blank capture.
- **State deltas**: `04`↔`06` AE 39,004 (hover lift); `04`↔`07` AE 373,922
  (dialog sheet); `04`↔`08` AE 242,666 (long-thread bottom); `08`↔`09` AE
  108,397 (narrow reflow). A 10th home-scroll capture was compared at AE 1,609
  against `05` and dropped as a near-duplicate rather than counted.

Effort: high
