# Nexora Design Brief 2.0 — FINAL

**Status:** Final visual direction for Nexora 2.0
**Product:** Nexora — local-first AI desktop workspace / agent application
**Current codebase:** Tauri v2 + React 19 + TypeScript + Rust + SQLite
**Design decision:** M3E is no longer the visual identity. It may remain as implementation infrastructure where useful.

---

## 0. The decision in one sentence

> **Nexora is a quiet, dense, agent-native desktop workspace where the UI makes the AI's work observable without turning the product into a dashboard, IDE clone, or chat clone.**

The product should feel like a serious engineering instrument, not a Material showcase and not an AI-generated SaaS dashboard.

---

# 1. What Nexora is NOT

Do not design Nexora as:

- ChatGPT with a sidebar.
- Cursor with different colors.
- A Material 3 Expressive demo.
- A dashboard full of cards.
- A neon cyberpunk AI tool.
- A glassmorphism product.
- A giant rounded/pill-heavy UI.
- An IDE pretending to be an agent manager.
- A collection of "premium" gradients and glowing borders.

**Hard bans:** random gradients, purple/blue glow haze, glass cards, excessive blur, giant hero areas, decorative metric cards, pill badges everywhere, redundant borders, gratuitous rounded containers, constant pulsing animations, layout elements moving around between releases.

---

# 2. Product personality

### Core adjectives

**Quiet · technical · precise · alive · restrained · confident · fast**

### Visual metaphor

A dark engineering workstation at night: almost everything stays calm; only the thing that is currently happening gets visual energy.

### Design principle

> **Attention is a limited budget. Spend it only on state changes, decisions, and useful feedback.**

---

# 3. Design pillars

## A. Agent-native, not chat-first

The user should feel that they are observing and steering a work process.

The hierarchy becomes:

`intent → agent state → actions/tools → changes → result`

not:

`message bubble → message bubble → message bubble`

## B. Quiet by default

Most UI is nearly static and visually low-contrast.

Information becomes richer only when the user interacts with it.

Examples:

- model metadata appears on hover;
- tool details expand on demand;
- secondary actions appear on hover/focus;
- token/cost/time information is available without dominating the conversation;
- workspace state can be inspected without permanently occupying the main canvas.

## C. Stable layout

Never move essential controls because of a feature rollout.

No surprise extra sidebars.

No random relocation of terminal/settings/navigation.

The main shell should feel learnable and stable for months.

## D. Motion has meaning

Animation exists to communicate:

- entering/leaving;
- state transition;
- relationship;
- progress;
- completion;
- cause/effect.

It does not exist just to prove that the app can animate.

## E. Nexora owns a visual identity

Use references for principles, not copying.

Borrow:

- Cursor's strong agent observability;
- Linear's density and hierarchy;
- Claude's calm conversation treatment;
- IDEs' respect for workspace context.

Do **not** copy their layouts, icons, colors, naming, or exact animations.

---

# 4. Shell

Recommended desktop composition:

```text
┌────────────────────────────────────────────────────────────────────────────┐
│  workspace / conversation context                              window     │
├──────────────┬───────────────────────────────────────────────┬─────────────┤
│              │                                               │             │
│   NAVIGATION │                 AGENT WORKSPACE               │   CONTEXT   │
│              │                                               │   optional   │
│   project    │       timeline / conversation / result       │   details    │
│   agents     │                                               │             │
│   tasks      │                                               │             │
│   search     │                                               │             │
│              │                                               │             │
│   recent     │                                               │             │
│              │                                               │             │
└──────────────┴───────────────────────────────────────────────┴─────────────┘
```

The third column is **contextual**, not permanently visible.

Prefer one coherent workspace over many simultaneously visible panels.

---

# 5. Sidebar

Keep the sidebar compact and visually quiet.

Suggested structure:

```text
Nexora mark
────────────────
WORKSPACE
  Project
  Agents
  Tasks
  Search

RECENT
  Fix auth
  Refactor API
  UI polish
  ...

────────────────
workspace selector
settings
```

Rules:

- no oversized navigation cards;
- no icon-only ambiguity for primary navigation;
- active state uses a small local indicator + subtle surface shift;
- hover is quiet;
- selected item is unmistakable without being bright;
- sidebar may collapse, but collapsing must not change the underlying navigation model;
- recent items should look like history, not another dashboard.

---

# 6. Main conversation / agent workspace

The central canvas is the product's main stage.

It should combine ordinary chat with agent work without creating a second completely separate UI language.

### Empty state

Do not show an enormous centered welcome screen.

Use a compact mark + useful starting prompt near the composer.

### Messages

Assistant messages stay visually lighter than user turns.

User messages can use a quiet elevated surface, but avoid chat-bubble UI as the dominant visual metaphor.

Assistant content should read more like a document/work log.

Code gets proper containment.

---

# 7. The defining feature: Agent Activity Spine

Every active agent run gets a thin vertical visual spine.

Example:

```text
│
│  Understanding request
│
│  Reading 4 files
│
│  Planning changes
│
│  Editing auth.ts
│
│  Running tests
│
✓  Completed
```

The spine is extremely subtle at rest.

During execution a tiny active pulse travels through it. It must never look like a neon progress bar.

This becomes one of Nexora's recognizable visual signatures.

---

# 8. AI state language

Do not use a permanent generic `Thinking...` indicator.

Use human-readable, contextual states:

- Understanding request
- Tracing dependencies
- Reading files
- Searching workspace
- Planning changes
- Editing files
- Running command
- Checking result
- Waiting for approval
- Completed

The exact text can vary based on the actual agent event.

### Important

The wording is secondary to the visual state.

The **same Nexora mark** should subtly transform between states.

Example concept:

```text
idle       → stable mark
thinking   → slow internal movement
reading    → inward movement / gathering
writing    → directional movement
checking   → brief scan-like motion
done       → settle into stable mark
error      → controlled interruption, not an alarm animation
```

No spinning loader as the default agent identity.

---

# 9. Tool calls

Default state is compact.

Example:

```text
⌁ Read src/auth.ts
⌁ Read src/session.ts
⌁ Search "refreshToken"
✓ Found 7 references
```

Click/keyboard focus expands the entry:

```text
Read file
src/auth.ts
42.7 KB
```

The user should always be able to answer:

> What is the agent doing right now?

and, when needed:

> What exactly did it touch?

---

# 10. Diff / change presentation

Changes should feel like part of the run, not a totally separate product.

Example:

```text
Edit
src/auth.ts                    +24 −8
```

Expand → diff viewer.

The important visual relationship is:

`agent action → file → change → verification`

not:

`random file card → giant diff panel`.

---

# 11. Composer

The composer should be one of the strongest pieces of visual polish.

Base state:

```text
Ask Nexora anything...

[ attach ]                          [model] [mode]   ↑
```

Rules:

- one composed surface;
- no giant floating rounded rectangle;
- textarea is visually quiet;
- controls become more expressive only when needed;
- attachments are visible but compact;
- model/mode are inspectable without taking over;
- send action is always obvious;
- keyboard-first behavior is excellent.

When the user begins typing, the composer can gently become more informative. This should be a layout morph, not a dramatic expansion.

---

# 12. Model / provider UI

Avoid permanent technical clutter.

Default:

`Claude · Opus`

Hover/focus:

`Claude · Opus 4.x · context · cost · latency`

Running:

`● Opus · running`

Completed:

`✓ Opus · 18s`

One element should change state instead of spawning multiple indicators.

---

# 13. Micro-interactions: FINAL SET

These are approved as part of the design language. All are **quiet, contextual, temporary, and optional**. None creates a new permanent panel or dashboard element.

## 13.1 Context Echo

When an agent recently touched a file, the file can show a temporary, quiet `recently used` signal.

Example:

`App.tsx  ·`

On hover:

`Last used by Nexora · 8s ago`

Purpose: reinforce continuity without adding another permanent panel.

## 13.2 Workspace Pulse

When meaningful workspace changes occur, the relevant workspace/navigation item receives one tiny pulse and then returns to rest.

No toast required for every small event.

## 13.3 Quiet Completion

When a run completes successfully, do not throw confetti/checkmark explosions.

Instead:

- active spine settles;
- Nexora mark returns to its stable state;
- result line becomes slightly clearer for a moment;
- changed files remain available to inspect.

## 13.4 Focus Trail

Keyboard navigation should leave a very subtle visual trail for the currently focused region, helping users understand where keyboard input will go.

## 13.5 Hover Reveal

Secondary controls appear only on hover/focus where safe.

They should enter quickly (roughly the 100–150 ms range) and disappear without drama.

## 13.6 Stable Streaming

New streamed content should not cause the whole page to jump.

Only the actual insertion region moves.

## 13.7 Split Reveal

When split view opens, the secondary pane should spatially emerge from the workspace rather than abruptly materialize.

Duration can be expressive once, then immediately settle.

## 13.8 Command Palette Continuity

Ctrl/Cmd+K should visually feel like part of Nexora, not a generic modal.

Results should preserve the same hierarchy, keyboard behavior, and motion grammar as the rest of the app.

---

## 13.9 Temporal Fade

Old tool calls in a long run gradually become slightly quieter as they age.

- Only inactive/secondary rows fade.
- The currently active row stays fully readable.
- The fade is subtle enough that reading history remains comfortable.
- Do not fade important final results, errors, approvals, or user messages.

Purpose: the eye finds the current work immediately without hiding history.

## 13.10 Scroll Hold, Not Jump

If the user scrolls upward during streaming, the stream must **not** force the page back down.

At the bottom, show a tiny line such as:

`↧ streaming · 12 tok/s`

Clicking it returns to the live edge.

No toast. No forced auto-scroll.

## 13.11 File Touch Border

A file just touched by the agent gets a temporary 1px accent line in the file tree.

- very low opacity;
- no badge;
- no dot;
- fades away after roughly 60–90 seconds.

This is the physical counterpart to Context Echo.

## 13.12 Quiet Copy

Copy controls appear on hover/focus.

After copying, replace the button label with `✓ copied` briefly, then restore it.

No toast.

## 13.13 Path Middle-Truncate

Long paths remain compact at rest:

`src/features/.../refreshToken.ts`

On hover/focus, the complete path reveals itself.

Do not reserve huge horizontal space for paths that are rarely read in full.

## 13.14 Drag Intent, Not Overlay

Dragging a file over the app should highlight only the relevant drop target, especially the composer.

Use a restrained inset border and a tiny surface change.

Do not cover the whole application with a giant dashed drop zone.

## 13.15 Approval Dim

When a run is waiting for approval, slightly reduce the visual priority of the rest of the workspace while keeping it usable.

Recommended effect:

- very mild dimming only;
- approval block stays fully readable;
- no modal backdrop unless the action is genuinely destructive;
- return to normal immediately after the decision.

## 13.16 Composer Ghost Draft

Switching away from a conversation and returning can briefly show the saved draft as a quiet reminder for roughly 1–2 seconds.

The reminder disappears as soon as the user types or focuses the composer.

The draft itself remains safely preserved in the conversation state.

## 13.17 Intent Carry Chips

When a new conversation is created directly from another context, show a very temporary hint such as:

`from: auth.ts · session.ts`

The hint fades as soon as the user starts typing.

If the user explicitly keeps it, it becomes normal context rather than a transient suggestion.

## 13.18 Command Palette Weight

Recent/frequently used commands should be emphasized by typography and ordering, not by bright accent colors.

Think:

- slightly stronger weight;
- slightly clearer contrast;
- better ordering.

Avoid a rainbow of highlighted commands.

## 13.19 Mark Idle Breath

When Nexora is genuinely idle for a while, the mark can perform one almost imperceptible slow breath.

- approximately 1.0 → 1.015 scale;
- slow, once;
- no continuous loop;
- skip entirely when the window is inactive.

The goal is "alive", not "animated".

## 13.20 Terminal Cursor Discipline

The terminal cursor should blink only while the terminal is focused.

After a large paste, the caret can briefly become visually solid before returning to normal.

This gives a subtle perception of responsiveness without adding UI.

## 13.21 Selection Count

When the user selects text in code/diff views, a tiny transient label can show:

`3 lines · 87 chars`

It disappears after copying or the next interaction.

No floating toolbar unless an action is actually needed.

---

# 13.22 Return Marker

When the user returns to a long run, Nexora can remember the last place they were reading.

Show only a thin `last viewed` marker.

The marker disappears after the user scrolls or interacts.

Purpose: resume reading without a permanent bookmark UI.

## 13.23 Quiet Diff Linger

Lines recently changed by the agent can keep a very weak temporary change trace after the normal diff emphasis fades.

Purpose: when looking back at a file, the user can still see what was recently touched without opening the full diff.

## 13.24 Run Boundary

Separate runs visually with a tiny time/context marker instead of a large card.

Example:

`14:32 · run started`

This helps users distinguish separate pieces of work inside one conversation.

## 13.25 Context Carryover Line

If a new run intentionally continues from an earlier run, show the relationship through a tiny visual connection in the activity spine.

No permanent "previous context" panel is needed.

## 13.26 Focus Memory

When the user returns to a conversation, preserve useful workspace state:

- open tab;
- scroll position;
- split pane;
- active pane;
- draft;
- focused context where safe.

This should feel instant, not animated.

## 13.27 Selection Carry

Where technically safe, preserve a user's selected line/file context when switching panes or tabs.

A temporary anchor can make it clear which location was preserved.

## 13.28 Quiet Undo Window

Immediately after an agent edit, a small `Undo` action can exist in the local context for a short time.

Do not create a toast.

After the short recovery window, ordinary version control becomes the source of truth.

## 13.29 Success Settles; Failure Persists

Successful states should visually relax after completion.

Errors should remain readable until acknowledged or superseded.

Principle:

> Success can become quiet. Failure must remain findable.

## 13.30 Hover Intent Delay

Do not make secondary controls appear instantly whenever the pointer crosses a region.

Use a tiny intention delay so fast mouse movement does not cause UI flicker.

The result should feel calmer without the user noticing why.

## 13.31 Soft Keyboard Echo

When a user repeatedly performs an action through a shortcut, any first-use hint for that action should disappear permanently for that user after familiarity is established.

Nexora should teach once, then get out of the way.

## 13.32 File Path Memory

Frequently used paths can keep the same compact middle-truncation behavior and preserve the last meaningful folder/file anchor on hover.

The path remains readable without becoming a giant breadcrumb system.

## 13.33 Tool Group Folding

After a run finishes and the user has had a chance to see the live activity, long sequences of old tool calls may collapse into a compact summary:

`12 tool calls · 4 files · 2 commands`

Clicking expands them.

Never hide an active run or a blocking approval this way.

## 13.34 Invisible Progress

For long operations, prefer a tiny factual count over a large progress component.

Example:

`Writing files · 6 / 9`

Only show it when the count is meaningful.

## 13.35 Quiet Workspace Activity

If a workspace has a burst of meaningful changes, secondary workspace context can become very slightly more prominent for a short time.

This must stay within secondary contrast levels.

It is a background cue, not a dashboard metric.

## 13.36 Last Action Echo

After a tool action completes, the UI can briefly preserve the action's meaning before settling into the final state.

Example:

`✓ Tests passed`

then quietly:

`12 files checked`

then simply:

`Ready`

The transition should feel like information settling, not an animated notification.

## 13.37 Dead Zone Protection

Resizers, drag targets, splitters, and edge-triggered controls should require a small amount of pointer intent before activating.

This prevents accidental panel movement and hover flicker.

## 13.38 Agent Presence Without Avatar

When an agent is actively working in a file or area, use a tiny structural cue near the file name or relevant surface.

No robot avatar.

No status badge.

The message is simply:

> the agent is here right now.

## 13.39 Quiet Recovery

When a command fails, keep the error compact and immediately expose useful next actions:

`Retry · Inspect`

When the retry succeeds, the old failure should visually lose priority rather than remaining as a giant red block.

## 13.40 Human Boundary

The interface must make it clear which content is produced by the human and which is produced by the agent without relying only on colors.

Use alignment, spacing, structure, and labels.

The distinction should remain understandable in grayscale and high-contrast mode.

---

## 13.41 No-Op Honesty

If the agent decides that no code or files need to change, say so plainly:

`No changes needed`

Do not manufacture activity, fake progress, or show an empty diff just to make the run look busy.

Quiet products should be honest about doing nothing.

## 13.42 Context Freshness

When context used by the agent is old or potentially stale, do not add a persistent warning badge.

Use a temporary low-contrast hint only when the user is about to rely on it.

Purpose: keep the workspace calm while still preventing misleading context.

## 13.43 Approval Return Point

When an approval interrupts a long run, preserve a small visual anchor to the exact action waiting for approval.

After approval, the user should be able to see where execution continues without hunting through the timeline.

## 13.44 Composer Survival

Opening settings, search, terminal, VCS, tasks, or other secondary screens must never silently destroy an unfinished composer draft.

Returning to the conversation should restore the exact draft and its local context.

This is a behavioral rule, not a visual effect.

## 13.45 First-Class Quiet Loading

When a view is loading but the user can still understand what will appear, prefer keeping the existing structure stable and changing only the content state.

Avoid whole-screen skeleton jungles.

Use a small factual status where needed.

## 13.46 Learned UI

Nexora can remember harmless presentation preferences such as:

- whether a certain detail section was expanded;
- preferred panel width;
- last useful tab/split arrangement;
- command frequency for palette ranking.

These preferences should make the interface progressively calmer, never more cluttered.

## 13.47 Quiet No-Toast Principle

Do not use a toast when the affected element can explain the result itself.

Examples:

- copy → `✓ copied` in place;
- save → state on the saved item;
- run complete → settled run state;
- file changed → file trace;
- approval → approval block.

Toasts remain for events that truly occur outside the user's current visual context.

## 13.48 Last Responsible Moment

Do not reveal extra information earlier than necessary.

Examples:

- show cost when the user asks/hover-focuses;
- show full path on hover;
- show tool arguments when expanded;
- show detailed error diagnostics when inspected.

This keeps the default state calm without making detail inaccessible.

---

# 14. Global rule for all micro-interactions

All micro-interactions follow these rules:

- **Hover/focus:** roughly 100–150 ms.
- **Reveal/compact expansion:** roughly 150–250 ms.
- **Expressive motion:** only for genuinely meaningful start/finish/spatial events.
- **No continuous decorative loops** except the rare idle breath, and even that is one-shot.
- **No animation is required to understand the UI.**
- With `prefers-reduced-motion`, state changes happen without motion and all information remains visible.
- No approved micro-feature creates a new permanent dashboard widget merely to display itself.
- Every feature must have a quiet state and a clear active state.
- If a micro-interaction becomes visually noticeable after repeated use, it is probably too strong.

---

# 15. Motion system

Use motion sparingly but intentionally.

### Fast

Hover, focus, small state changes:

~100–150 ms.

### Normal

Micro layout transitions:

~150–250 ms.

### Expressive

Only for meaningful spatial events:

- run starts;
- run completes;
- new conversation;
- split opening;
- major view transition.

Typical upper bound:

~350–500 ms.

### Do not animate

- every row continuously;
- every icon independently;
- static decorative gradients;
- large blur effects;
- text with looping effects;
- entire page on every message token.

### Reduced motion

Always preserve the same information without the animation.

---

# 16. Color

The current warm-neutral direction is usable, but accent must become quieter.

### Base

Warm near-black / graphite canvas.

### Surface hierarchy

Use tone, not borders, wherever possible:

`canvas → elevated → nested`

### Accent

One primary restrained warm accent.

Amber can remain the seed, but it must act as punctuation:

- active state;
- focus;
- important actions;
- agent identity;
- thin indicators.

It should not color entire panels.

### Secondary state colors

Use muted semantic colors for:

- success;
- warning;
- error.

Never use color as the only signal.

---

# 17. Typography

Desktop-first.

Prefer a highly legible modern system sans for UI text.

Monospace is reserved for:

- code;
- paths;
- commands;
- model IDs where useful;
- technical metadata.

Typography should create hierarchy before color does.

Avoid oversized marketing typography inside the application.

---

# 18. Shape language

Current M3E radius infrastructure may remain underneath, but the visual result must become less obviously "M3".

Rules:

- medium rounding for meaningful containers;
- small rounding for controls/fields;
- full pills only when the geometry communicates an actual compact status/control;
- no pill-shaped everything;
- no decorative blobs;
- no giant rounded cards nested inside giant rounded cards.

---

# 19. Borders and elevation

Flat at rest.

Borders are structural, not decorative.

Use them mainly for:

- shell seams;
- input boundaries when necessary;
- code/diff containment;
- dialogs/popovers;
- high-contrast mode.

Shadows are reserved for actual floating layers.

---

# 20. Context panel

Context should be a mode, not permanent furniture.

Possible content:

- files;
- run details;
- diff;
- approvals;
- command output;
- provider/model metadata.

It should open because the user needs more detail, not because the designer wants to fill empty space.

---

# 21. Tabs and split panes

Tabs are workspace memory, not browser decoration.

Rules:

- clear active state;
- predictable close behavior;
- keyboard-first navigation;
- no excessive tab chrome;
- opening a tab should not reset context.

Split view should share the same visual language as the single pane.

---

# 22. Settings / secondary screens

Settings, prompts, tasks, VCS, audit, terminal, docs, etc. should look like members of the same application, not separate mini-products.

Use the same shell, same typography, same navigation semantics, same spacing and motion language.

Avoid turning every feature into a dashboard.

---

# 23. External feedback and why the direction is strict

The final direction is based on the common themes visible in recent developer/community discussion around AI coding tools:

- users dislike UI churn and controls moving around;
- large collections of cards, pills, gradients and glow create a generic "AI UI" look;
- strong design direction works better than telling an agent to make a UI merely "modern";
- motion is valued when it explains state or relationships rather than acting as decoration;
- AI workspaces benefit from making agent activity observable without adding permanent panels for every detail.

### Implication for Nexora

Nexora should have a **strong, explicit design language** before agents implement components. The design system is a constraint, not a suggestion.

---

# 24. M3E decision

## M3E is NOT the Nexora visual style.

Keep only pieces that are useful implementation infrastructure:

- design tokens;
- accessibility rules;
- reduced-motion handling;
- spacing/typography infrastructure;
- state-layer primitives where useful;
- motion timing primitives.

Do not let official M3E component shapes or expressive patterns dictate Nexora's visual appearance.

### Mental model

```text
M3E infrastructure
        ↓
Nexora design tokens / primitives
        ↓
Nexora-specific components
        ↓
Nexora-specific experience
```

Not:

```text
M3E
 ↓
Nexora
```

---

# 25. Implementation rules for coding agents

Every UI agent working on Nexora must obey these rules:

1. Read this document before changing UI.
2. Do not invent a new component visual language.
3. Reuse existing tokens/primitives unless there is a documented reason not to.
4. Do not introduce gradients, glass, neon, glow, giant cards, or arbitrary pills.
5. Do not add a sidebar/panel just because information exists.
6. Prefer contextual disclosure over permanent UI.
7. Any new animation must have a UX reason.
8. Any new persistent navigation item must justify its location in the shell.
9. Never move an established control merely to fit a new feature.
10. Every state must remain understandable with reduced motion.
11. Every new screen must look like Nexora, not like a generic React template.
12. When unsure between more UI and less UI, start with less UI.
13. New micro-interactions must not create permanent UI clutter.
14. If two ideas solve the same problem, use the simpler one.
15. Do not add a visual indicator when existing structure can already communicate the same state.
16. Prefer stable state + contextual detail over permanent status decoration.

---

# 26. Visual QA checklist

Before accepting a UI change, inspect it at the actual desktop form factor.

### Ask:

- Does the screen still read correctly in 2 seconds?
- Is the current agent state obvious?
- Can I tell what changed?
- Can I discover details without permanent clutter?
- Are the important controls stable and predictable?
- Does the UI look like Nexora rather than a Tailwind/M3 template?
- Is the accent restrained?
- Are cards/borders actually necessary?
- Does motion communicate something?
- Does the interface remain good with animations disabled?
- Did this change make the product denser, clearer, or merely busier?
- Did the new feature introduce another thing I have to constantly look at?
- Can the same information be communicated by an existing element changing state?

If the answer to the last two questions suggests more noise, reject or simplify the change.

---

# 27. Final aesthetic target

```text
                  QUIET
                    │
                    ▼
        ┌─────────────────────────┐
        │     NEXORA WORKSPACE    │
        │                         │
        │  stable shell           │
        │  restrained surfaces    │
        │  strong typography      │
        │                         │
        │  ── agent work ──────   │
        │     reading             │
        │     planning            │
        │     editing             │
        │     testing             │
        │                         │
        │  subtle motion          │
        │  visible consequences   │
        └─────────────────────────┘
                    │
                    ▼
                  ALIVE
```

The visual goal is not to impress the user for 30 seconds.

The goal is that after using Nexora for 3 hours, the interface feels **calm, intelligent, fast, trustworthy and distinctly Nexora**.

---

# 28. Implementation priority

### P0 — Core identity

These must exist before visual polish is considered complete:

- stable shell;
- agent Activity Spine;
- clear agent state language;
- compact tool events;
- coherent diff/change relationship;
- stable streaming;
- strong composer;
- M3E visual de-emphasis.

### P1 — High-value quiet UX

Implement next:

- Context Echo;
- File Touch Border;
- Scroll Hold;
- Temporal Fade;
- Quiet Copy;
- Path Middle-Truncate;
- Tool Group Folding;
- Focus Memory;
- Composer Survival;
- Quiet Recovery;
- Success Settles / Failure Persists.

### P2 — Delight that rewards long sessions

Implement after the core is solid:

- Composer Ghost Draft;
- Intent Carry Chips;
- Return Marker;
- Quiet Diff Linger;
- Run Boundary;
- Context Carryover Line;
- Selection Carry;
- Soft Keyboard Echo;
- Mark Idle Breath;
- Workspace Activity cue;
- Last Action Echo;
- Learned UI.

### P3 — Experimental / measure before shipping

These require usage testing and can be removed if they are too subtle or too distracting:

- Context Freshness;
- Quiet Workspace Activity;
- Agent Presence Without Avatar;
- Context Carryover Line;
- Invisible Progress in edge cases;
- any future "ambient" state cue.

---

# 29. Final directive

> **Build less chrome. Make the agent more visible. Make state more meaningful. Make motion rarer but better. Let the interface remember harmless context. Make every useful detail appear exactly when it is needed. Make Nexora unmistakably Nexora.**

The best Nexora feature is not something a user notices on first launch.

It is something they notice on day three and then miss immediately when they use another tool.

This document is the visual north star for Nexora 2.0.
