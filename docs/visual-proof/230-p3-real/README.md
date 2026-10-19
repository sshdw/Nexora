# 230 Phase 3 — REAL-Tauri capture evidence (thread + agent run steps + context panel)

**Every frame in this folder is REAL Tauri**: the `src-tauri/target/debug/nexora` WebKitGTK
binary, the real SQLite backend (schema v14), the real Vite dev server on
`http://127.0.0.1:1420`, the real provider (NVIDIA NIM over the OpenAI-compatible
path), the real OS keyring, the real agent run loop and the real governance
(approval) machinery. No `?mock`, no Chromium, no headless browser, no mocked
IPC. Every frame was opened and inspected before acceptance.

Branch: `task/oci-230-p3` @ `8c28f24` (P3 code + the round-1/round-2 review fixes).

## Environment (measured on this box)

```
Xvfb :97 -screen 0 1600x1000x24 -ac +extension GLX +extension RENDER +render -noreset &
DISPLAY=:97 openbox &
DISPLAY=:97 WEBKIT_DISABLE_COMPOSING_MODE=1 /home/ubuntu/nexora/src-tauri/target/debug/nexora
```

Window: 1200x800 at (200,107), captured with `import -window`. The `libEGL
DRI3/DRI2` warnings in the app's stderr are a cosmetic fallback notice, not a
fault (same as the P2 run).

Two capture displays were used: `:97` for the first eleven frames and `:99`
(a private display with its own data directory) for the last four, because a
second orchestrator session was driving the same display at the same time and
stole the window mid-shot. Every frame in this folder was inspected after
capture, so what is written here is what the pixels show.

**Provider.** The NVIDIA NIM endpoint is the app's own curated provider
(`nvidia` in the provider registry) and the key lives in the OS keyring under
`nexora@nvidia`; the app's `keyring` crate talks to it over the session bus'
Secret Service (DH, `dh-ietf1024-sha256-aes128-cbc-pkcs7`). Model:
`nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` (and
`nvidia/nemotron-3-super-120b-a12b` for the slower runs). The credential is
never in the database, the logs or this file.

**Autonomy** is `supervised` ("Под примотром"), so every tool call of an agent
run has to be approved by the user — that is where the approval frames come from.

## Frames

| File | Colours | What is in the frame | What I SAW |
|---|---|---|---|
| `01-cold-start-no-tabs.png` | 1065 | cold boot, **zero tabs**, populated sidebar | No tab-strip row at all (the strip contributes zero height with no tabs) and no orphan row. The thread column shows the compact empty state — **no hero mark** (`NexoraMark` removed from the empty state in this phase). Sidebar: brand, `Новый разговор`, search, the history list, the rail footer. |
| `02-empty-thread.png` | 2103 | a conversation with **no messages** | "Пока нет сообщений / Напишите первое сообщение ниже, чтобы начать разговор." — left-aligned in the thread column, one action, and that action is the composer already on screen. No hero, no mark, no 56px ornament. |
| `03-loaded-thread-folded.png` | 3562 | a loaded thread, two runs | The **user turn is plain text** — no bubble, no fill, no border, no right alignment (MAP row 7). Run 1 is folded to **ONE summary line**: "Завершён · 1 файл изменён · 1 команда · 6 вызовов инструментов" plus the model and `run 1`. Run 2 is the error turn: the three-line block "Что произошло / Что сохранилось / Что делать" with the provider's own 429 text, then the failed rows. |
| `04-steps-expanded-repeats-grouped.png` | 3030 | run 1's timeline expanded | Tool calls fold to **one line each** (action → file → duration). The three consecutive `read_file` calls on the same path are **grouped**: "#2–#4 Прочитан `src/lib/db.rs` … ок ×3" with the individual rows listed under it. The write step shows its change as **outline badges** "+1 −1", not filled pills. |
| `05-diff-expanded.png` | 3042 | the write step expanded | A real unified diff: the `--- a/…` / `+++ b/…` headers, the `@@ -40,7 +40,7 @@ impl MigrationRunner {` hunk header, the `-`/`+` glyph column, mono text. **The diff is not empty** (the `no changes` branch is not what renders here). |
| `06-diff-restored.png` | 3392 | the same row collapsed again | The diff is restored — the row is back to its one-line form "#6 Записал `src/lib/db.rs` +1 −1 … ок 480ms" and **nothing was lost**: the path and both counts are still on the line. |
| `07-context-legend.png` | 2316 | the Context pane (map row 20) | The context breakdown hairline plus the **legend**: "Пользователь 7.1% · Ассистент 7.1% · Вызовы инструментов 57.1% · Прочее 28.6%". The four category dots are **achromatic** — sampled below. The panel is a mode (the Chat/Context switch at the top), not permanent furniture. |
| `08-approvals-pending.png` | 3382 | a **live** agent run, parked on an approval | "read_file запрашивает подтверждение" with the call arguments in mono, **Разрешить** and **Запретить** live, the note "Если никто не ответит, запуск будет отменён автоматически." and "Истекает через 10 мин". The run header reads "Выполняется" with Пауза / Отмена. **Nothing was auto-approved and nothing is faded.** |
| `09-live-run-tool-calls.png` | 3156 | the same live run one step later | The approval is resolved into a step ("#2 Подтверждение · read_file … approved") and the real tool call follows: "#3 Прочитан `notes.txt`" with the arguments and the real file content the agent read (`alpha / beta / gamma`). The live run header shows the pulse next to "Выполняется". |
| `10-completed-run-summary.png` | 3205 | the run after it finished | The ONE summary line again — "Завершён · **Изменения не нужны**": the run only read a file, so the honest count of changed files is zero (the count is DISTINCT files, not write steps). No duplicated "Completed" block anywhere. |
| `11-long-thread-scrolled.png` | 2306 | a long thread, scrolled to the middle | 18 messages, the thread scrolled to the middle (scrollbar visible on the right), user turns plain and assistant turns on bare canvas, the thread measure capped. This is the long-thread scroll beat. |
| `12-narrow-window-850.png` | 2914 | the app resized to **850x800** (`xwininfo`-verified) | At 850px the tab strip, the pane header, the Chat/Context switch, the autonomy row and the composer **all still fit** — nothing is clipped off-screen, nothing overlaps. The live run and the approval block stay legible at that width. |
| `13-error-turn-live.png` | 3233 | a live run that failed | The error block again, this time for a real failed run: "Что произошло — `run interrupted by application shutdown`", "Что сохранилось — (empty)", "Что делать — Отправить сообщение ещё раз." The failed rows stay listed under it (`#2 Размышление`, `#3 Подтверждение · execute_command`). The cause is honest: the app was restarted while that run was in flight. |
| `14-reduced-motion-live-run.png` | 2950 | a live run with the **OS animation preference off** | The run is in the "Выполняется" state and every word of it is still there — the state is informative, not blank, and the indicator is rendered (not stripped). The static arm itself is in the shipped CSS, quoted below. |
| `15-streaming-assistant-reply.png` | 2217 | an assistant reply arriving in the thread | A real provider answer, rendered as a plain assistant turn under its author label and model name. The user turn above it is plain text — no bubble. |

## Measured, not eyeballed

- **Context legend dots** (frame `07`), sampled from the PNG with
  `convert 07-context-legend.png -format "%[pixel:p{283,702}]" info:`:
  - `Пользователь` → `srgb(237,234,231)`
  - `Ассистент` → `srgb(133,131,135)`
  - `Вызовы инструментов` → `srgb(133,131,135)`
  - `Прочее` → `srgb(133,131,135)`
  All four are blends of the theme's own on-surface grey (`#EDEAE7`) — **no hue
  anywhere**, and none of them is the legacy amber `#a9824a` the legend used
  to resolve to. The rule that ships it is
  `src/styles/context.css` → `color-mix(in oklch, var(--nex-sys-color-on-surface) …)`.
- **Frame uniqueness**: `md5sum *.png | awk '{print $1}' | sort | uniq -d` → **0
  duplicates** over 15 files. Two frames with the same hash would be a defect.
- **Reduced motion**: the OS preference was switched with
  `gsettings set org.gnome.desktop.interface enable-animations false`
  (read back as `false`) **before** the app process was started — that is the
  GTK setting WebKitGTK maps to the `prefers-reduced-motion` media feature,
  and the app's own gate reads exactly that query
  (`src/lib/useReducedMotion.ts`, `REDUCED_MOTION_QUERY`). The static arm
  ships in `src/styles/conversation.css`:

  ```css
  @media (prefers-reduced-motion: reduce) {
    .nex-agent-run-state-running:not(.nex-agent-run-parked) .nex-agent-spine-pulse {
      animation: none;
      opacity: 1;
      transform: translateY(150%);
    }
  }
  ```

  i.e. the travelling accent segment freezes as a **static, fully opaque mark**
  instead of disappearing on a transparent frame — the run still reads as
  running. Frames `08`/`09` are the same surface with the preference **on**,
  for the A/B.

## Frames that were dropped, and why

- `02-settings.png` (from the earlier pass) was a **byte-identical copy** of
  `01-cold-start.png` (`md5 cb9b0d3fee218e31910de2b484db968e` on both): it showed
  a cold start, not Settings. Deleted, not submitted. Settings is a P5 surface
  and is out of this phase anyway.
- The four frames this folder carried before this pass (`01-cold-start.png`,
  `01-loaded-thread.png`, `01a-cold-start-populated.png`, `08-empty-state.png`)
  were **stale**: they were captured before the round-1 review fix that removed
  the chat bubble from the user turn and before the compact empty state landed,
  so they show a UI this branch no longer renders. They were replaced by
  `01`–`03` and `08`–`15` above rather than kept next to the current code.

## Known gaps (declared, with follow-up issues)

These are **declared gaps**, not silent ones — the phase plan and the map rows
that cover them are recorded in the PR body and each one has a filed
follow-up issue (see the PR body for the numbers):

1. **MAP row 7 — long message content does not fold, and code fences are not
   rendered with copy.** The thread prints message content as plain text; the
   repository has no markdown/code-block renderer, and building one is a
   separate slice. The phase acceptance list does not include it.
2. **MAP row 9 — the diff's restore/confirm affordance.** The working-tree
   revert lives in the Version Control surface (`src/components/AuditPanel.tsx`),
   and the thread's diff expand/restore round trip is captured in
   `05`/`06`. The ambiguity in the row's wording is filed for a decision.
3. **Live agent run: the final assistant message only appears after the
   conversation is re-opened.** `src/lib/useConversation.ts` has no listener for
   the `agent-run-event` stream, so the message list is not reloaded when a run
   finishes. The run block itself updates live (frame `09`). This is a hook
   issue, outside a visual phase's "hooks untouched" rule.
