# #230 Phase 6 — kill legacy: BLOCKED, owner decision required

## Status

BLOCKED — cannot be executed as specified; this PR carries the decision request, no code.

## Measured facts

All figures re-derived on 2026-10-20 against `2cacffa`; the command sits next to each count.

- **1621** lines under `src/` contain the string `nex-sys-`, of which **1620** contain `--nex-sys-` (one line has the bare form) — `grep -r -- 'nex-sys-' src | wc -l` → 1621; `grep -r -- '--nex-sys-' src | wc -l` → 1620. Raw occurrences of `--nex-sys-` = **1729** — `grep -ro -- '--nex-sys-' src | wc -l` → 1729. `src/styles/tokens.css` holds **259** definitions — `grep -oE -- '--nex-sys-[a-z0-9-]+[[:space:]]*:' src/styles/tokens.css | wc -l` → 259, by family: color 148, type 30, state 25, shape 11, focus 10, elevation 10, space 8, scrollbar 6, z 5, layout 4, nav 2.
- **1307** live `var(--nex-sys-*)` references outside `tokens.css` — `grep -rho -- 'var(--nex-sys-\([a-z]*\)-' src | sed 's/var(--nex-sys-//; s/-$//' | sort | uniq -c`, by family:

  | family | refs | family | refs |
  |---|---|---|---|
  | color | 445 | layout | 16 |
  | space | 383 | z | 7 |
  | type | 252 | elevation | 7 |
  | shape | 99 | nav | 6 |
  | motion | 45 | focus | 3 |
  | state | 42 | scrollbar | 2 |

  The twelve family counts sum to 1307. (The working brief's aggregate "862" does not equal the sum of its own per-family list; the family counts were re-measured individually and are the ones used here.)
- Of those, **763 have no flat equivalent today**: space 383 + type 252 + motion 45 + layout 16 + z 7 + elevation 7 + nav 6 + focus 3 + scrollbar 2 + state 42. The 42 `--nex-sys-state-*` references use 11 names (`disabled-opacity`, `dragged-opacity`, `focus-opacity`, `hover`, `hover-opacity`, `inverse-hover`, `inverse-pressed`, `pressed`, `pressed-opacity`, `selected`, `selected-opacity` — `grep -rho -- 'var(--nex-sys-state-[a-z-]*' src | sed 's/var(--nex-sys-state-//' | sort -u`), while the flat set defines only `--state-ok` / `--state-warn` / `--state-err` (`src/index.css:363-365`, `437-439`); 721 + 42 = 763.
- `src/index.css:315` (`:root, :root[data-theme="dark"]`) defines only color 21, radius 8, state 6, font 6 (`grep -cE '^\s*--(color|radius|state|font)-' src/index.css`) plus the semantic roles (`--background`, `--foreground`, `--primary`, `--secondary`, `--muted`, `--accent`, `--destructive`, `--border`, `--input`, `--ring`, `--card`, `--popover`, `--tick`). It defines **zero** `--space-*`, `--type-*`, `--layout-*`, `--z-*`, `--elevation-*`, `--scrollbar-*`, `--motion-*`, `--focus-*`, `--nav-*` — `grep -cE '^\s*--(space|type|layout|z|elevation|scrollbar|motion|focus|nav)-' src/index.css` → 0 for every one.
- **459 distinct `nex-*` names across 42 files** — `grep -hoE '\bnex-[a-zA-Z0-9_-]+' src/components/*.tsx src/App.tsx | sort -u | wc -l` → 459; `grep -lE … | wc -l` → 42 (443 distinct across the 37 production components; the remainder appear only in colocated tests). No post-P5 replacement convention exists in the tree: `ds-*`, `ui-*`, `nx-*` prefixes all return 0, only `nex-` is used. Highest per-file counts (distinct names): `AgentRunSteps.tsx` 99, `PromptLibraryView.tsx` 49, `SettingsView.tsx` 38, `VersionControlPanel.tsx` 35, `ContextPanel.tsx` 33, `OnboardingFlow.tsx` 31, `Composer.tsx` 31, `DocsPanel.tsx` 29, `TaskPanel.tsx` 26, `DebtPanel.tsx` 26, `ActivityHealthPanel.tsx` 25, `AuditPanel.tsx` 24, `IssuesPanel.tsx` 22, `ConversationView.tsx` 22, `App.tsx` 18, `ReleasePanel.tsx` 15, `KanbanBoard.tsx` 15.
- The seven panels named in the P6 scope (PromptLibrary / Activity / Audit / Debt / Issues / Docs / Release) cover **118** distinct `nex-*` names; **77** of those are unique to them, **41** are shared with other files, and **382** names appear in files outside the seven panels — 118 + 382 − 41 = 459. Command: `scope=$(printf '%s\n' src/components/{PromptLibraryView,ActivityHealthPanel,AuditPanel,DebtPanel,IssuesPanel,DocsPanel,ReleasePanel}.tsx)` (unquoted braces expand), `all="$(ls src/components/*.tsx) src/App.tsx"`, `rest=$(comm -23 <(printf '%s\n' $all | sort) <(printf '%s\n' $scope | sort))`, `names(){ grep -hoE -- '\bnex-[a-zA-Z0-9_-]+' "$@" | sort -u; }`; then `names $scope | wc -l` → 118, `comm -23 <(names $scope) <(names $rest) | wc -l` → 77, `comm -12 <(names $scope) <(names $rest) | wc -l` → 41, `names $rest | wc -l` → 382. The per-file distinct counts sum to 190 only because 41 names are shared: 190 is not the in-scope surface, and "269 elsewhere" is not the out-of-scope surface either.
- **16 legacy sheets** still define `.nex-*` rules (`grep -cE '^\s*\.nex-' <sheet>`), **674 rules** in total:

  | sheet | rules | sheet | rules |
  |---|---|---|---|
  | `components.css` | 171 | `conversation.css` | 153 |
  | `composer.css` | 64 | `settings.css` | 46 |
  | `versionControl.css` | 40 | `tabs.css` | 33 |
  | `prompts.css` | 33 | `context.css` | 31 |
  | `activityHealth.css` | 25 | `onboarding.css` | 23 |
  | `tasks.css` | 15 | `shortcuts.css` | 15 |
  | `palette.css` | 12 | `tokens.css` | 5 |
  | `terminal.css` | 4 | `importExport.css` | 4 |

- 18 sheets exist in `src/styles/` (`ls -1 src/styles/*.css | wc -l` → 18); `src/index.css:110-134` imports **all 18** of them plus `src/styles.css` at `src/index.css:113` — `grep -c '@import "./styles' src/index.css` → 19 (18 sheets + `styles.css`).
- `src/styles/tokens.css` is imported at `src/index.css:110` inside `@layer(base)` and is deleted only in P6 — `NEXORA-FULL-PLAN.md:130`, restated at `AGENTS.md:593-595`.

**Sourcing note.** The P6 scope is stated in `NEXORA-FULL-PLAN.md:126-137` (LOCAL-ONLY, not in the repo tree; quoted in Conflict 1) and repeated in the instance-local `QUEUE-2.0-2.1.md:19-25` (goal :20, in-scope :21-22, acceptance :23, non-goals :24), so the queue entry corroborates the same conflict. `NEXORA-ORACLE-QUEUE-2.1.md` is a different file — the 2.1 CLI queue (Фаза 0-6 = #159-#165, `:11-84`) — and carries no #230/P6 entry; `grep -n '230\|P6\|kill legacy'` returns nothing.

## Conflict 1 — scope vs acceptance

Scope (`NEXORA-FULL-PLAN.md:127-131`): "Goal: zero `--nex-sys-*`, zero `nex-*` outside tests/history; legacy sheets deleted. Files IN: hidden panels (PromptLibrary/Activity/Audit/Debt/Issues/Docs/Release views) get a MECHANICAL token swap only (no restyle, render-smoke shots); then DELETE `tokens.css` + every emptied legacy sheet."

Acceptance (`NEXORA-FULL-PLAN.md:133-135`): "repo-wide grep `nex-sys-` = 0 hits outside tests/history; `nex-` class grep = 0 in components; build + full gate green."

Non-goals (`NEXORA-FULL-PLAN.md:136-137`): "any visual improvement (mechanical swap only — restyle waits for those panels' own issues)."

The class half of the acceptance line is app-wide: 459 names across 42 files, and the sheets that hold them are shared with visible panels (`conversation.css` 153, `components.css` 171, `composer.css` 64, `settings.css` 46, `versionControl.css` 40, `tabs.css` 33, `context.css` 31). "Delete every emptied legacy sheet" cannot reach them, because swapping the seven hidden panels covers 118 names, only 77 of them unique to those panels; the shared sheets keep serving visible panels. Meeting "`nex-` class grep = 0 in components" therefore requires removing every `nex-*` class from all 42 component files and deleting/re-expressing all 16 sheets — i.e. restyling the visible app, which `:136-137` forbids. Likewise, `tokens.css` holds 5 `.nex-*` rules plus its 259 token definitions, so it empties only after the 1307 live references are gone.

There is no reading of the scope under which the acceptance line is reachable: either the acceptance line is amended to the scope, or the scope is widened to the whole app (and split across more than one PR).

## Conflict 2 — token families

A "mechanical token swap" (`:128-129`) presupposes flat targets for every family it touches. For 763 live references there is none (see Measured facts: `src/index.css:315` defines zero space/type/layout/z/elevation/scrollbar/motion/focus/nav tokens, and its state tokens do not cover the 11 interaction-state names in use). The swap therefore cannot be mechanical until the owner picks a target definition:

- **(a) Transpose the exact legacy values under new flat names.** No visual delta, but it adds 75 declarations to the token authority (space 8, type 30, layout 4, z 5, elevation 10, scrollbar 6, nav 2, focus 10 — `src/styles/tokens.css`) plus 11 interaction-state names and the motion family (45 live refs; 54 `--motion-*` definitions across 111 lines of `src/styles/motion.css`). The family names are not specified by any binding document.
- **(b) Adopt the SYSTEM §2 "Numbers" scale** (`NEXORA-DESIGN-SYSTEM.md:81-84`). That section is explicitly "PROPOSED starting point, validate in Phase 0" and would change shipped spacing/type values — a visual change, which `NEXORA-FULL-PLAN.md:136-137` forbids in P6.

Separately, the naming authority collides with the acceptance line. `NEXORA-DESIGN-SYSTEM.md:72` states: "Canonical `--nex-sys-motion-*` wins over legacy `--motion-*` (migrate call sites)." `src/index.css:283-287` repeats the same rule and adds that "the legacy `--nex-sys-*` names themselves are pruned with `tokens.css` in Phase 6". So the binding system doc makes `--nex-sys-motion-*` canonical while the P6 acceptance line (`NEXORA-FULL-PLAN.md:133`) demands zero `--nex-sys-*` — the two cannot both hold for the 45 motion references.

## Owner decision required

**A.** SCOPE: is P6 the hidden-panels-only swap (acceptance line amended to match the scope), or the app-wide kill-legacy (scope widened: all 42 component files + all 18 sheets, and the phase must be split into more than one PR)?

Cost: hidden-panels-only leaves 382 names outside the seven panels, `tokens.css`, and the shared sheets alive, so P6 no longer reaches "zero legacy"; app-wide means 42 component files + 16 sheets re-expressed with a full visual-QA pass, split into at least two PRs. The "use tokens + Tailwind, no new visual language" half of the replacement scheme is already inherited from `NEXORA-DESIGN-SYSTEM.md:43-44` (Tailwind v4 for layout/composition only; colours, type, radii, motion from tokens; no arbitrary values); only the class naming scheme itself is still owner-grade.
Unblocks: the acceptance line in `NEXORA-FULL-PLAN.md:133-135` can be written to match reality, and an implementer can start without guessing.

**B.** TOKEN FAMILIES: may flat --space-*/--type-*/--layout-*/--z-*/--elevation-*/--scrollbar-*/--nav-*/--focus-*/--motion-* families be added to src/index.css, and with which values — the exact legacy values (no visual delta) or the SYSTEM §2 scale (visual change, out of P6 scope)?

Cost: exact legacy values add 75 flat declarations (+ motion handling) with zero pixel delta and no doc support for the names; the SYSTEM §2 scale changes shipped spacing/type app-wide and needs its own visual-QA pass outside P6.
Unblocks: the 763 currently untargetable references get a defined destination, making the swap mechanical.

**C.** MOTION NAMING: the acceptance line demands zero --nex-sys-*, but SYSTEM §2 makes --nex-sys-motion-* canonical. Which wins?

Cost: letting `--nex-sys-motion-*` win requires amending "zero `--nex-sys-*`" for motion (45 refs, 54 legacy definitions); letting the acceptance line win reverses `NEXORA-DESIGN-SYSTEM.md:72` and re-points those call sites to `--motion-*`.
Unblocks: a single naming rule for motion, so the token grep in the acceptance line becomes well-defined.

## What is NOT done and why

- No code, CSS, token, or test was changed; this commit adds this document only.
- `src/styles/tokens.css` is not deleted: it dies only in P6 (`NEXORA-FULL-PLAN.md:130`) and still backs 1307 live references (`src/index.css:110`).
- No sheet is deleted: all 16 still define `.nex-*` rules (674 rules), and six of them are shared with visible panels.
- Visible panels are untouched (382 of the 459 `nex-*` names appear in files outside the seven scope panels; only 77 are unique to those panels).
- `src/theme.test.ts` is not rewritten and `src/lib/navTrim.test.ts` is untouched.
- The CLI queue file (`NEXORA-ORACLE-QUEUE-2.1.md`) has no P6 entry; the P6 entry lives in the instance-local `QUEUE-2.0-2.1.md:19-25` and repeats the same scope-vs-acceptance conflict, corroborating the blocker rather than resolving it.

## What happens once decided

- **If A = hidden-panels-only:** amend `NEXORA-FULL-PLAN.md:133-135` so the class/token greps are scoped to the seven panels and their sheets, keep `tokens.css`, and the swap becomes executable against the seven panels (118 names, 77 unique) with the currently defined families only.
- **If A = app-wide:** rewrite the P6 scope in `NEXORA-FULL-PLAN.md:126-137` to name all 42 component files and 16 sheets, split the phase into at least two PRs (token families first, class/sheet removal second), and schedule the visual-QA pass the restyle requires.
- **If B = exact legacy values:** add the 75 flat declarations plus the 11 interaction-state names (and the motion mapping) to `src/index.css` from the `src/styles/tokens.css` values verbatim, then execute the swap with a zero-delta visual check.
- **If B = SYSTEM §2 scale:** P6 becomes a visual phase; move the scale adoption to its own issue/PR with a visual-QA pass, and keep P6 blocked until it lands.
- **If C = `--nex-sys-motion-*` wins:** amend the acceptance line to exempt motion, keep the 45 references, and record the exemption in `theme.test.ts`.
- **If C = acceptance line wins:** change `NEXORA-DESIGN-SYSTEM.md:72` and `src/index.css:283-287`, then migrate the 45 motion references to `--motion-*` in the same PR.
