//! Composer (#220) — behaviour, accessibility and the two hard gates.
//!
//! jsdom ships no layout and computes no cascade, so these pins
//! cover what jsdom CAN prove, and leave paint to the sheet:
//!
//!   1. BEHAVIOUR  Enter sends, Shift+Enter does not, the send
//!                 button is disabled by every guard that used to
//!                 disable it, attach/remove dispatch, the mode
//!                 pills switch agent mode, and the model picker
//!                 dispatches a selection instead of keeping one.
//!   2. TEACHING   the inline placeholder chips are read out of
//!                 src/lib/shortcuts.ts by id, so a rebound key
//!                 changes the chip with it. The ids are asserted
//!                 to exist — that is what keeps `shortcutKeys`'
//!                 soft failure from going silent.
//!   3. MOTION     the composer's one animated component renders
//!                 its finished sentence and creates NO GSAP
//!                 timeline under `prefers-reduced-motion`, with
//!                 a GSAP spy. Plus the reverse check: the global
//!                 gate in src/styles/motion.css still collapses
//!                 every animation already merged.
//!   4. THEME      src/styles/composer.css paints from shadcn
//!                 semantic tokens only, registers no animation
//!                 of its own (the gate is the one stylesheet's
//!                 job), and keeps the composer's reading measure
//!                 equal to the thread's.
//!   5. AGENT      Agent is the mode the pane opens on (R4-02); the
//!      POLICY    attach control is ABSENT there rather than a
//!                 disabled promise, the draft survives the switch
//!                 (a switch is not a send), and the model list —
//!                 a vertical list — opts out of the sliding pill.
//!
//! The paint itself, the hover/selected/focus looks and the
//! before/after screenshots are in the PR body; no timing
//! assertions anywhere (repo policy).

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { readRepoFile } from "../test/readRepoFile";

import { SHORTCUTS, shortcutById, shortcutKeys } from "../lib/shortcuts";
import { REDUCED_MOTION_QUERY } from "../lib/useReducedMotion";
import type { AutonomyStore } from "../lib/useAutonomy";
import type { Attachment } from "../lib/tauri";
import { LocaleProvider } from "../lib/useLocale";
import ComposerAutonomyHint from "./ComposerAutonomyHint";
import Composer, { type ComposerProps } from "./Composer";
import TextType from "./TextType";

// The GSAP spy is installed for the WHOLE file, hoisted, so that
// whichever copy of the vendored TextType a component ends up
// holding — the statically imported one or a freshly evaluated one —
// is talking to the same spy. `vi.resetModules()` per test was the
// alternative and it re-evaluates React itself under the already-
// imported testing-library, which is a much bigger variable than the
// thing being measured.
const { gsapSpy } = vi.hoisted(() => ({
  gsapSpy: { set: vi.fn(), to: vi.fn() },
}));

vi.mock("gsap", () => ({ gsap: gsapSpy }));

/** matchMedia that answers the reduced-motion query, and only that one. */
function mockReducedMotion(matches: boolean): void {
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: query === REDUCED_MOTION_QUERY ? matches : false,
    media: query,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
  }));
}

beforeEach(() => {
  gsapSpy.set.mockClear();
  gsapSpy.to.mockClear();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function autonomyStub(overrides: Partial<AutonomyStore> = {}): AutonomyStore {
  return {
    mode: "semi_autonomous",
    loading: false,
    error: null,
    reload: vi.fn(async () => {}),
    setMode: vi.fn(async () => null),
    ...overrides,
  };
}

function attachmentStub(overrides: Partial<Attachment> = {}): Attachment {
  return {
    id: 1,
    conversation_id: 1,
    message_id: null,
    file_name: "notes.md",
    file_path: "/tmp/notes.md",
    file_size_bytes: 2048,
    mime_type: "text/markdown",
    ...overrides,
  };
}

// The scene: Agent is the mode the pane opens on (R4-02), so the
// fixture opens there too. Tests that need Chat ask for it by name —
// which keeps every chat-only control's coverage honest instead of
// inheriting it from a default.
function composerProps(overrides: Partial<ComposerProps> = {}): ComposerProps {
  return {
    draft: "",
    onDraftChange: vi.fn(),
    onSubmit: vi.fn(),
    sending: false,
    agentBusy: false,
    attachmentsBusy: false,
    canSend: false,
    attachments: [],
    onPickFiles: vi.fn(),
    onRemoveAttachment: vi.fn(),
    agentMode: true,
    onAgentModeChange: vi.fn(),
    autonomy: autonomyStub(),
    onAutonomyChange: vi.fn(),
    selectedModel: "gemini-3.6-flash",
    models: ["gemini-3.6-flash", "gemini-3.1-pro-preview"],
    onSelectModel: vi.fn(),
    modelError: null,
    ready: true,
    onOpenSettings: vi.fn(),
    ...overrides,
  };
}

function renderComposer(overrides: Partial<ComposerProps> = {}) {
  const props = composerProps(overrides);
  const view = render(
    <LocaleProvider>
      <Composer {...props} />
    </LocaleProvider>,
  );
  return { ...view, props };
}

/** A parent-shaped harness: the composer is controlled, so a mode
 *  switch and a draft only exist through an owner that holds both —
 *  exactly how ConversationView wires it (one draft per pane, one
 *  mode for the composer). Nothing here clears or sends on its own. */
function StatefulComposer({ overrides = {} }: { overrides?: Partial<ComposerProps> }) {
  const [draft, setDraft] = useState(overrides.draft ?? "");
  const [agentMode, setAgentMode] = useState(overrides.agentMode ?? true);
  return (
    <LocaleProvider>
      <Composer
        {...composerProps(overrides)}
        draft={draft}
        onDraftChange={setDraft}
        agentMode={agentMode}
        onAgentModeChange={setAgentMode}
      />
    </LocaleProvider>
  );
}

describe("composer — send flow", () => {
  it("sends on Enter and never on Shift+Enter", () => {
    const { props } = renderComposer({ draft: "hello", canSend: true });
    const field = screen.getByLabelText("Message");

    fireEvent.keyDown(field, { key: "Enter" });
    expect(props.onSubmit).toHaveBeenCalledTimes(1);

    fireEvent.keyDown(field, { key: "Enter", shiftKey: true });
    expect(props.onSubmit).toHaveBeenCalledTimes(1);
  });

  it("dispatches on Enter whatever the readiness, leaving the guard to the caller", () => {
    // Unchanged contract from before this PR: the field always
    // dispatches, and the OWNER of the send decides whether it may
    // happen (`handleSubmit` returns early unless `canSend`). Two
    // guards would be two places to drift apart.
    const { props } = renderComposer({ draft: "", canSend: false });
    fireEvent.keyDown(screen.getByLabelText("Message"), { key: "Enter" });
    expect(props.onSubmit).toHaveBeenCalledTimes(1);
  });

  it("reports every draft change upward", () => {
    const { props } = renderComposer();
    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "next" } });
    expect(props.onDraftChange).toHaveBeenCalledWith("next");
  });

  it("disables send unless the caller says it can send", () => {
    const { rerender, props } = renderComposer({ draft: "hello", canSend: false });
    const send = screen.getByRole("button", { name: /Send/ });
    expect(send).toBeDisabled();
    expect(screen.getByLabelText("Message")).toBeEnabled();

    rerender(
      <LocaleProvider>
        <Composer {...props} canSend />
      </LocaleProvider>,
    );
    expect(screen.getByRole("button", { name: /Send/ })).toBeEnabled();
  });

  it("disables the field while a send or an agent run is in flight", () => {
    const { rerender, props } = renderComposer({ sending: true, agentBusy: false });
    expect(screen.getByLabelText("Message")).toBeDisabled();

    rerender(
      <LocaleProvider>
        <Composer {...props} sending={false} agentBusy />
      </LocaleProvider>,
    );
    expect(screen.getByLabelText("Message")).toBeDisabled();
  });

  it("names the busy state on the send button", () => {
    const { rerender, props } = renderComposer({ sending: true, canSend: true, agentMode: false });
    expect(screen.getByRole("button", { name: "Sending…" })).toHaveAttribute("aria-busy", "true");

    rerender(
      <LocaleProvider>
        <Composer {...props} sending={false} agentBusy agentMode canSend />
      </LocaleProvider>,
    );
    expect(screen.getByRole("button", { name: "Running…" })).toHaveAttribute("aria-busy", "true");
  });

  it("disables attach while sending or while attachments are loading", () => {
    const { rerender, props } = renderComposer({ sending: true, agentMode: false });
    expect(screen.getByRole("button", { name: "Add file" })).toBeDisabled();

    rerender(
      <LocaleProvider>
        <Composer {...props} sending={false} attachmentsBusy />
      </LocaleProvider>,
    );
    expect(screen.getByRole("button", { name: "Add file" })).toBeDisabled();

    rerender(
      <LocaleProvider>
        <Composer {...props} sending={false} attachmentsBusy={false} agentBusy />
      </LocaleProvider>,
    );
    expect(screen.getByRole("button", { name: "Add file" })).toBeDisabled();
  });

  it("dispatches attach and removal with no arguments the caller does not own", () => {
    const onPickFiles = vi.fn();
    const onRemoveAttachment = vi.fn();
    renderComposer({
      onPickFiles,
      onRemoveAttachment,
      attachments: [attachmentStub()],
      attachmentsBusy: false,
      sending: false,
      agentMode: false,
    });

    fireEvent.click(screen.getByRole("button", { name: "Add file" }));
    expect(onPickFiles).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("button", { name: "Remove notes.md" }));
    expect(onRemoveAttachment).toHaveBeenCalledWith(1);
  });

  it("disables attachment removal while a send or attachment work is in flight", () => {
    const { rerender, props } = renderComposer({
      attachments: [attachmentStub()],
      sending: true,
      agentMode: false,
    });
    expect(screen.getByRole("button", { name: "Remove notes.md" })).toBeDisabled();

    rerender(
      <LocaleProvider>
        <Composer {...props} sending={false} attachmentsBusy />
      </LocaleProvider>,
    );
    expect(screen.getByRole("button", { name: "Remove notes.md" })).toBeDisabled();
  });

  it("lists attached files by name and size", () => {
    renderComposer({
      agentMode: false,
      attachments: [attachmentStub({ id: 3, file_name: "spec.pdf", file_size_bytes: 4096 })],
    });
    const list = screen.getByLabelText("Attached files");
    expect(within(list).getByText("spec.pdf")).toBeInTheDocument();
    expect(within(list).getByText("4.1 KB")).toBeInTheDocument();
  });
});

describe("composer — modes", () => {
  it("opens in Agent, with the segmented control saying so", () => {
    // R4-02: Agent is the mode the pane opens on. The composer is
    // controlled — the pane owns the value — so what this pins is
    // that the control and the rows follow the prop with no
    // composer-local mirror of their own.
    renderComposer();
    const group = screen.getByRole("radiogroup", { name: "Agent mode" });
    expect(within(group).getByRole("radio", { name: "Agent" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(
      within(group).getByRole("radio", { name: "Chat mode — one answer per message" }),
    ).toHaveAttribute("aria-checked", "false");
    // The Agent pane is on screen, not just labelled: its autonomy row is.
    expect(screen.getByRole("radiogroup", { name: "Autonomy mode" })).toBeInTheDocument();
  });

  it("keeps the typed draft through a Chat → Agent round trip, and sends nothing", () => {
    const onSubmit = vi.fn();
    render(<StatefulComposer overrides={{ draft: "half a thought", canSend: true, onSubmit }} />);
    const field = screen.getByLabelText("Message");
    expect(field).toHaveValue("half a thought");
    expect(field).toBeEnabled();

    fireEvent.click(screen.getByRole("radio", { name: "Chat mode — one answer per message" }));
    expect(field).toHaveValue("half a thought");
    expect(field).toBeEnabled();

    fireEvent.click(screen.getByRole("radio", { name: "Agent" }));
    expect(field).toHaveValue("half a thought");
    expect(field).toBeEnabled();

    // A mode switch is not a send: nothing was dispatched, nothing was
    // cleared. Only the user's send clears the draft.
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("exposes the two primary modes as one radio group", () => {
    renderComposer({ agentMode: false });
    const group = screen.getByRole("radiogroup", { name: "Agent mode" });
    const chat = within(group).getByRole("radio", { name: "Chat mode — one answer per message" });
    const agent = within(group).getByRole("radio", { name: "Agent" });

    expect(chat).toHaveAttribute("aria-checked", "true");
    expect(agent).toHaveAttribute("aria-checked", "false");
  });

  it("switches agent mode from the pills", () => {
    const onAgentModeChange = vi.fn();
    renderComposer({ onAgentModeChange, agentMode: false });
    fireEvent.click(screen.getByRole("radio", { name: "Agent" }));
    expect(onAgentModeChange).toHaveBeenCalledWith(true);

    fireEvent.click(screen.getByRole("radio", { name: /Chat mode/ }));
    expect(onAgentModeChange).toHaveBeenCalledWith(false);
  });

  it("hides the autonomy row in chat mode and shows it in agent mode", () => {
    const { rerender, props } = renderComposer({ agentMode: false });
    expect(screen.queryByRole("radiogroup", { name: "Autonomy mode" })).not.toBeInTheDocument();

    rerender(
      <LocaleProvider>
        <Composer {...props} agentMode />
      </LocaleProvider>,
    );
    const autonomy = screen.getByRole("radiogroup", { name: "Autonomy mode" });
    expect(within(autonomy).getByRole("radio", { name: "Semi-autonomous" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("dispatches autonomy changes from the store's own value", () => {
    const onAutonomyChange = vi.fn();
    renderComposer({ agentMode: true, onAutonomyChange });
    fireEvent.click(screen.getByRole("radio", { name: "Full-auto" }));
    expect(onAutonomyChange).toHaveBeenCalledWith("full_autonomous");
  });

  it("keeps the autonomy explainer's text in step with the store's mode", () => {
    // Reduced motion, so the sentence renders as its finished frame
    // and can be read at all — under motion it is still typing.
    mockReducedMotion(true);
    renderComposer({ agentMode: true, autonomy: autonomyStub({ mode: "full_autonomous" }) });
    expect(
      screen.getByText("Full-auto: the agent runs to the end without stopping."),
    ).toBeInTheDocument();
  });

  it("keeps the explainer in step with the live store after a switch", () => {
    mockReducedMotion(true);
    const autonomy = autonomyStub({ mode: "supervised" });
    const { rerender, props } = renderComposer({ agentMode: true, autonomy });
    expect(
      screen.getByText("Supervised: every tool call waits for your approval."),
    ).toBeInTheDocument();

    // The store answering is the only thing that changes the text —
    // the composer holds no autonomy copy of its own.
    autonomy.mode = "semi_autonomous";
    rerender(
      <LocaleProvider>
        <Composer {...props} autonomy={autonomy} />
      </LocaleProvider>,
    );
    expect(
      screen.getByText("Semi-auto: routine steps run alone, writes still ask first."),
    ).toBeInTheDocument();
  });
});

describe("composer — attachment policy (R4-02 / G-20)", () => {
  it("renders the attach control in Chat mode only", () => {
    const { rerender, props } = renderComposer();
    expect(screen.queryByRole("button", { name: "Add file" })).not.toBeInTheDocument();
    expect(screen.queryByTitle("Add file")).not.toBeInTheDocument();

    rerender(
      <LocaleProvider>
        <Composer {...props} agentMode={false} />
      </LocaleProvider>,
    );
    expect(screen.getByRole("button", { name: "Add file" })).toBeInTheDocument();
  });

  it("shows nothing about attachments in Agent mode — the absence is the statement", () => {
    // `start_agent_run` takes no attachment ids, so a draft file
    // shown in Agent mode would be a promise the run silently breaks.
    // Absent — not disabled, not dead, not chipped, not hinted.
    renderComposer({ attachments: [attachmentStub()] });

    expect(screen.queryByRole("button", { name: "Add file" })).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Attached files")).not.toBeInTheDocument();
    expect(screen.queryByText("notes.md")).not.toBeInTheDocument();

    // …and the teaching line no longer advertises the missing control.
    const teach = document.querySelector(".nex-composer-teach");
    expect(teach).not.toBeNull();
    expect(teach!.textContent).not.toMatch(/attach/i);
    const chips = [...teach!.querySelectorAll("kbd")].map((chip) => chip.textContent);
    expect(chips).toEqual([shortcutKeys("palette.toggle")[0], shortcutKeys("help.show-shortcuts")[0]]);
    expect(teach!.textContent).toContain("command palette");
  });
});

describe("composer — model picker", () => {
  it("shows the store's model and opens a list of the provider's models", () => {
    renderComposer({ selectedModel: "gemini-3.6-flash" });
    const trigger = screen.getByRole("button", { name: "Agent mode and model" });
    expect(trigger).toHaveTextContent("gemini-3.6-flash");

    fireEvent.click(trigger);
    const list = screen.getByRole("radiogroup", { name: "Model" });
    expect(within(list).getByRole("radio", { name: "gemini-3.6-flash" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(within(list).getByRole("radio", { name: "gemini-3.1-pro-preview" })).toHaveAttribute(
      "aria-checked",
      "false",
    );
  });

  it("draws the model list as rows, with no pill to slide between them", () => {
    renderComposer();
    fireEvent.click(screen.getByRole("button", { name: "Agent mode and model" }));
    const list = screen.getByRole("radiogroup", { name: "Model" });

    // The primitive's grid root, re-flowed to rows by composer.css.
    expect(list).toHaveClass("nex-seg", "nex-composer-model-list");
    // `indicator={false}` — no decorative pill is emitted at all, so
    // nothing can slide across a vertical list.
    expect(list.querySelector(".nex-seg-indicator")).toBeNull();

    const rows = within(list).getAllByRole("radio");
    expect(rows).toHaveLength(2);
    for (const row of rows) {
      expect(row.parentElement).toBe(list);
    }
    // Selection stays legible without the pill: the active row says so.
    expect(rows[0]).toHaveAttribute("aria-checked", "true");
    expect(rows[0]).toHaveClass("is-active");
    expect(rows[1]).not.toHaveClass("is-active");

    // The mode group next door still carries the shared pill: the
    // opt-out is local to the list, not a global disable.
    expect(document.querySelector(".nex-composer-pills .nex-seg-indicator")).not.toBeNull();
  });

  it("dispatches the picked model and keeps no copy of the selection", () => {
    const onSelectModel = vi.fn();
    renderComposer({ onSelectModel });
    fireEvent.click(screen.getByRole("button", { name: "Agent mode and model" }));
    fireEvent.click(screen.getByRole("radio", { name: "gemini-3.1-pro-preview" }));

    // Dispatch only: the trigger still shows what the store holds,
    // because the store has not answered yet.
    expect(onSelectModel).toHaveBeenCalledWith("gemini-3.1-pro-preview");
    expect(screen.getByRole("button", { name: "Agent mode and model" })).toHaveTextContent(
      "gemini-3.6-flash",
    );
  });

  it("says so plainly when no model is selected", () => {
    renderComposer({ selectedModel: null, models: [] });
    expect(screen.getByRole("button", { name: "Agent mode and model" })).toHaveTextContent(
      "No model selected",
    );
  });

  it("surfaces a model-selection failure", () => {
    renderComposer({ modelError: "provider unreachable" });
    fireEvent.click(screen.getByRole("button", { name: "Agent mode and model" }));
    expect(screen.getByRole("alert")).toHaveTextContent("provider unreachable");
  });

  it("offers provider settings from the picker", () => {
    const onOpenSettings = vi.fn();
    renderComposer({ onOpenSettings });
    fireEvent.click(screen.getByRole("button", { name: "Agent mode and model" }));
    fireEvent.click(screen.getByRole("button", { name: "Provider settings" }));
    expect(onOpenSettings).toHaveBeenCalled();
  });
});

describe("composer — hint row", () => {
  it("states the composer keys when the composer is ready", () => {
    renderComposer({ ready: true });
    expect(
      screen.getByText("Enter to send · Shift+Enter for a new line"),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Open Settings" })).not.toBeInTheDocument();
  });

  it("offers Settings when no provider or model is configured", () => {
    const onOpenSettings = vi.fn();
    renderComposer({ ready: false, selectedModel: null, models: [], onOpenSettings });
    expect(
      screen.getByText("Choose a provider and model in Settings to send messages."),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Open Settings" }));
    expect(onOpenSettings).toHaveBeenCalled();
  });
});

describe("composer — the inline teaching line", () => {
  it("renders registry key labels, not invented ones", () => {
    renderComposer({ agentMode: false });
    const teach = document.querySelector(".nex-composer-teach");
    expect(teach).not.toBeNull();
    expect(teach).toHaveAttribute("role", "note");
    // Every kbd chip that is a KEY comes from the registry.
    const chips = [...teach!.querySelectorAll("kbd")].map((k) => k.textContent);
    expect(chips).toEqual([
      "+",
      shortcutKeys("palette.toggle")[0],
      shortcutKeys("help.show-shortcuts")[0],
    ]);
    // …and both of those ids really are inventoried.
    expect(shortcutById("palette.toggle")).toBeDefined();
    expect(shortcutById("help.show-shortcuts")).toBeDefined();
    expect(chips).toEqual(["+", "Ctrl+K", "Ctrl+/"]);
  });

  it("keeps the native placeholder as the fallback and the accessible name", () => {
    renderComposer();
    const field = screen.getByLabelText("Message");
    expect(field).toHaveAttribute("placeholder", "Message");
  });

  it("gets out of the way once there is a draft", () => {
    renderComposer({ draft: "already typing" });
    expect(document.querySelector(".nex-composer-teach")).toBeNull();
  });

  it("ships a lookup that fails soft for an unregistered id", () => {
    // The chips above assert two real ids; this is the other half —
    // the soft failure is a documented degrade, not a crash.
    expect(shortcutKeys("no.such.shortcut")).toEqual([]);
    expect(SHORTCUTS.some((entry) => entry.id === "no.such.shortcut")).toBe(false);
  });
});

describe("composer — reduced-motion gate (GSAP spy)", () => {
  it("renders the finished sentence and creates NO timeline", () => {
    mockReducedMotion(true);
    render(
      <LocaleProvider>
        <ComposerAutonomyHint mode="semi_autonomous" text="Semi-auto: routine steps run alone." />
      </LocaleProvider>,
    );

    // The whole sentence the animation would have typed…
    expect(screen.getByText("Semi-auto: routine steps run alone.")).toBeInTheDocument();
    // …and the assertion that matters: no timeline was created.
    // Not "it looked finished" — the GSAP API was never called.
    //
    // NOTE, and it is why the positive control below is not
    // optional: this explainer passes `showCursor={false}`, and
    // TextType only calls GSAP inside `if (showCursor && …)`. So
    // these two assertions CANNOT distinguish "no timeline" from
    // "this component never had a cursor to blink" — on their own
    // they would survive the reduced-motion branch being deleted.
    // What is load-bearing here is the finished-frame assertion
    // above plus the typing counterpart below; the spy reading is
    // the third leg, and the positive control proves the leg is
    // attached to anything.
    expect(gsapSpy.set).not.toHaveBeenCalled();
    expect(gsapSpy.to).not.toHaveBeenCalled();
  });

  it("POSITIVE CONTROL — the spy is live against the real GSAP module", () => {
    // If the spy were not wired to the module TextType imports, every
    // `.not.toHaveBeenCalled()` above would pass forever and mean
    // nothing. So: drive TextType itself — the REAL vendored
    // component, with a cursor, motion allowed — and require the
    // spy to see it. This test is what makes the negative
    // assertions load-bearing instead of decorative.
    mockReducedMotion(false);
    const { unmount } = render(<TextType text="x" showCursor />);

    expect(gsapSpy.set).toHaveBeenCalled();
    expect(gsapSpy.to).toHaveBeenCalled();
    unmount();
  });

  it("renders no explainer for a mode it does not know", () => {
    mockReducedMotion(true);
    const { container } = render(
      <LocaleProvider>
        <ComposerAutonomyHint mode={"nonsense" as never} text="unused" />
      </LocaleProvider>,
    );
    expect(container.innerHTML).toBe("");
  });

  it("REVERSE CHECK — the global CSS gate still collapses every merged animation", () => {
    // This PR adds motion, so the gate it relies on has to still
    // be the one gate: one block, in one file, holding the end
    // frame and freezing loops. Asserted against the file that
    // ships, exactly like src/lib/motion.test.tsx does.
    const css = readRepoFile("src/styles/motion.css").replace(/\/\*[\s\S]*?\*\//g, "");
    const block = /@media \(prefers-reduced-motion: reduce\) \{([\s\S]*?)\n\}/.exec(css);
    expect(block).not.toBeNull();
    const declarations = block![1];
    expect(declarations).toMatch(/animation-duration:\s*0\.01ms\s*!important/);
    expect(declarations).toMatch(/transition-duration:\s*0\.01ms\s*!important/);
    expect(declarations).toMatch(/animation-iteration-count:\s*1\s*!important/);
    expect(declarations).toMatch(/animation-fill-mode:\s*forwards\s*!important/);
    expect(declarations).toMatch(/\*,\s*\*::before,\s*\*::after/);
  });
});

describe("composer sheet — theme linkage (src/styles/composer.css)", () => {
  const css = readRepoFile("src/styles/composer.css");
  // Comments are prose, not declarations: a `--nex-sys-*` named in a
  // note must not satisfy (or break) a declaration assertion.
  const code = css.replace(/\/\*[\s\S]*?\*\//g, "");

  /** The declaration block of one rule, or "" when it is not there. */
  const ruleBody = (selector: string): string =>
    new RegExp(`${selector.replace(/\./g, "\\.")}\\s*\\{([\\s\\S]*?)\\n\\}`).exec(code)?.[1] ?? "";

  it("paints from shadcn semantic tokens only", () => {
    const m3eRefs = [...code.matchAll(/var\(\s*(--nex-sys-[\w-]+)/g)].map((m) => m[1]);
    expect([...new Set(m3eRefs)]).toEqual([]);
  });

  it("declares no raw colour of its own", () => {
    // Hex, rgb()/rgba(), hsl(), named colours and colour functions
    // are all ways a surface stops following the theme.
    expect(code).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
    expect(code).not.toMatch(/\brgba?\(/);
    expect(code).not.toMatch(/\bhsla?\(/);
    expect(code).not.toMatch(/\b(oklch|oklab|lab|lch|color)\(/);
    expect(code).not.toMatch(/:\s*(white|black|red|blue|green)\s*[;}]/);
  });

  it("resolves every var() it uses against the black theme", () => {
    const theme = readRepoFile("src/index.css").replace(/\/\*[\s\S]*?\*\//g, "");
    const dark = /:root,\s*:root\[data-theme="dark"\]\s*\{([\s\S]*?)\n\}/.exec(theme)?.[1] ?? "";
    const defined = new Set([...dark.matchAll(/(--[a-z0-9-]+)\s*:/gi)].map((m) => m[1]));
    // `--radius-*` steps and the motion vars come from the Tailwind
    // theme mapping / src/styles/motion.css, not the colour block.
    const known = new Set([
      ...defined,
      "--radius-sm",
      "--radius-md",
      "--radius-lg",
      "--radius-xl",
      "--radius-2xl",
      "--radius-3xl",
      "--radius-4xl",
      "--motion-duration-fast",
      "--motion-easing-standard",
    ]);
    const missing = [...code.matchAll(/var\(\s*(--[a-z0-9-]+)/g)]
      .map((m) => m[1])
      .filter((name) => !known.has(name));
    expect([...new Set(missing)].sort()).toEqual([]);
  });

  it("adds no animation of its own — the global gate is the only motion source", () => {
    // Every transition must go through a `--motion-*` duration, and
    // this sheet must declare no keyframes: a composer-local timeline
    // would be a second motion source the reduced-motion block in
    // motion.css still has to catch.
    expect(code).not.toMatch(/@keyframes/);
    expect(code).not.toMatch(/animation(-name)?\s*:/);
    const transitions = [...code.matchAll(/transition:[^;]+;/g)].map((m) => m[0]);
    expect(transitions.length).toBeGreaterThan(0);
    for (const declaration of transitions) {
      expect(declaration).toContain("var(--motion-");
    }
  });

  it("stacks the model list in grid flow, never in the inert flex-direction", () => {
    // The primitive's root is a grid (equal `1fr` column tracks), so
    // `flex-direction: column` did nothing and the rows stayed side by
    // side. A vertical list has to be expressed in grid terms.
    const block = ruleBody(".nex-composer-model-list.nex-seg");
    expect(block).toContain("grid-auto-flow: row");
    expect(block).not.toContain("flex-direction");
  });

  it("leaves the selected segment's fill to the one shared pill", () => {
    // A `background` on the selected segment paints a second, opaque
    // pill over the primitive's indicator — same tone, different
    // radius, and the slide underneath disappears. The pill is the
    // fill; these rules may only carry what the pill cannot.
    for (const selector of [
      ".nex-composer-pills.nex-seg button.is-active",
      ".nex-composer-autonomy.nex-seg button.is-active",
    ]) {
      const block = ruleBody(selector);
      expect(block).toContain("color: var(--foreground)");
      expect(block).not.toContain("background");
    }
  });


  it("keeps the composer's reading measure equal to the thread's", () => {
    const thread = /--layout-measure-max:\s*([^;]+);/.exec(
      readRepoFile("src/index.css"),
    )?.[1];
    expect(thread).toBe("760px");
    expect(code).toMatch(/max-width:\s*760px/);
  });

  it("registers no second stylesheet entry or unlayered import", () => {
    const entry = readRepoFile("src/index.css").replace(/\/\*[\s\S]*?\*\//g, "");
    expect(entry).toMatch(/@import\s+"\.\/styles\/composer\.css"\s+layer\(base\);/);
  });
});

describe("shared-class scope — the failure strip stays shared", () => {
  // The one real scope violation this PR shipped, pinned so it
  // cannot come back: `.nex-composer-error` reads like the
  // composer's, is used by 25 sites in 10 components, and round 1
  // moved it into composer.css and repainted it on `--destructive`
  // — recolouring nine panels outside this issue's scope. The
  // shared class is back on the M3E tokens in conversation.css; the
  // composer has its own strip.
  const conversation = readRepoFile("src/styles/conversation.css").replace(
    /\/\*[\s\S]*?\*\//g,
    "",
  );
  const composerSheet = readRepoFile("src/styles/composer.css").replace(
    /\/\*[\s\S]*?\*\//g,
    "",
  );

  it("keeps the shared strip in the shell sheet, on the M3E error tokens", () => {
    const block = /\.nex-composer-error\s*\{([\s\S]*?)\n\}/.exec(conversation);
    expect(block).not.toBeNull();
    expect(block![1]).toContain("var(--color-error)");
    expect(block![1]).toContain("var(--color-error-container)");
    expect(block![1]).toContain("var(--color-error-outline)");
  });

  it("declares no rule for the shared strip in the composer sheet", () => {
    // A descendant selector would not save it: the strip renders as
    // a SIBLING of `.nex-composer`, not inside it.
    expect(composerSheet).not.toMatch(/\.nex-composer-error\b/);
  });

  it("gives the composer its own strip, and that is the only destructive strip here", () => {
    const strips = [...composerSheet.matchAll(/\.(nex-composer-strip-error)\s*\{([\s\S]*?)\n\}/g)];
    expect(strips).toHaveLength(1);
    expect(strips[0][2]).toContain("var(--destructive)");
  });

  it("keeps the shared TEXT FIELD in the shell sheet too", () => {
    // `.nex-composer-input` is not the composer's either: Version
    // Control's commit box carries it, and App.tsx's "Focus message
    // input" queries it by name. Round 1 restyled that panel by
    // accident; the shared rule is back, and the composer reaches it
    // through a descendant selector instead.
    const block = /\.nex-composer-input\s*\{([\s\S]*?)\n\}/.exec(conversation);
    expect(block).not.toBeNull();
    expect(block![1]).toContain("var(--color-on-surface)");
    expect(block![1]).toContain("min-height: 48px");
    // …and the composer sheet has no UNSCOPED rule for it.
    expect(composerSheet).not.toMatch(/(?<!shell )\.nex-composer-input\s*[,{:]/);
    for (const hook of [
      ".nex-composer-shell .nex-composer-input",
      ".nex-composer-shell .nex-composer-input::placeholder",
      ".nex-composer-shell .nex-composer-input:disabled",
      ".nex-composer-shell .nex-composer-input:focus-visible",
    ]) {
      expect(composerSheet).toContain(hook);
    }
  });

  it("re-points the two shared M3E primitives only through descendants", () => {
    // `.nex-seg` and `.nex-chip` are shared with every other
    // surface, so the composer can only re-point them through a
    // selector that cannot match anywhere else. Both carry a second
    // class for exactly that reason.
    for (const hook of ["nex-composer-pills", "nex-composer-autonomy", "nex-composer-model-list"]) {
      expect(composerSheet).toMatch(new RegExp(`\\.${hook}\\.nex-seg`));
    }
    expect(composerSheet).toMatch(/\.nex-composer-attachments \.nex-chip/);
    expect(composerSheet).toMatch(/\.nex-composer \.nex-seg button:focus-visible/);
  });
});