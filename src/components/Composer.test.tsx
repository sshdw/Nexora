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
import { tr } from "../lib/strings";
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
    // Autonomy is reachable from the Agent pane, but it is a popover now
    // (D-03): the control row carries one trigger naming the live mode, and
    // the three choices exist only while it is open.
    expect(
      screen.getByRole("button", { name: "Autonomy mode" }),
    ).toHaveTextContent("Semi-auto");
    expect(
      screen.queryByRole("radiogroup", { name: "Autonomy mode" }),
    ).not.toBeInTheDocument();
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

  it("keeps its visible mode labels as Chat / Agent — only the view switch was renamed", () => {
    // Defect B (round-4 visual QA): the stage's view chip and these two
    // mode pills both read "Chat" in the same chip grammar, one above
    // the other. THE VIEW SWITCH was renamed (Conversation | Context,
    // src/lib/viewSwitchLabels.test.ts); this pair is byte-identical to
    // what shipped, and that is what makes the two switches tell apart
    // on one screen.
    renderComposer({ agentMode: false });
    const group = screen.getByRole("radiogroup", { name: "Agent mode" });
    const labels = within(group)
      .getAllByRole("radio")
      .map((radio) => radio.textContent);
    expect(labels).toEqual(["Chat", "Agent"]);
    expect(within(group).queryByText("Conversation")).toBeNull();
  });

  it("switches agent mode from the pills", () => {
    const onAgentModeChange = vi.fn();
    renderComposer({ onAgentModeChange, agentMode: false });
    fireEvent.click(screen.getByRole("radio", { name: "Agent" }));
    expect(onAgentModeChange).toHaveBeenCalledWith(true);

    fireEvent.click(screen.getByRole("radio", { name: /Chat mode/ }));
    expect(onAgentModeChange).toHaveBeenCalledWith(false);
  });

  it("shows the autonomy trigger in agent mode only, and opens the choices", () => {
    const { rerender, props } = renderComposer({ agentMode: false });
    expect(screen.queryByRole("button", { name: "Autonomy mode" })).not.toBeInTheDocument();

    rerender(
      <LocaleProvider>
        <Composer {...props} agentMode />
      </LocaleProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: "Autonomy mode" }));
    const autonomy = screen.getByRole("radiogroup", { name: "Autonomy mode" });
    expect(within(autonomy).getByRole("radio", { name: "Semi-autonomous" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("dispatches autonomy changes from the store's own value", () => {
    const onAutonomyChange = vi.fn();
    renderComposer({ agentMode: true, onAutonomyChange });
    fireEvent.click(screen.getByRole("button", { name: "Autonomy mode" }));
    fireEvent.click(screen.getByRole("radio", { name: "Full-auto" }));
    expect(onAutonomyChange).toHaveBeenCalledWith("full_autonomous");
  });

  it("keeps the autonomy explainer's text in step with the store's mode", () => {
    // Reduced motion, so the sentence renders as its finished frame
    // and can be read at all — under motion it is still typing.
    mockReducedMotion(true);
    renderComposer({ agentMode: true, autonomy: autonomyStub({ mode: "full_autonomous" }) });
    fireEvent.click(screen.getByRole("button", { name: "Autonomy mode" }));
    expect(
      screen.getByText("Full-auto: the agent runs to the end without stopping."),
    ).toBeInTheDocument();
  });

  it("keeps the explainer in step with the live store after a switch", () => {
    mockReducedMotion(true);
    const autonomy = autonomyStub({ mode: "supervised" });
    const { rerender, props } = renderComposer({ agentMode: true, autonomy });
    fireEvent.click(screen.getByRole("button", { name: "Autonomy mode" }));
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

    // …and there is no teaching line left to advertise it: the chip row was
    // the rejected "teach chips" pattern (D-03) and is gone in both modes.
    expect(document.querySelector(".nex-composer-teach")).toBeNull();
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

  it("renders a long id in full and keeps it as the trigger's title", () => {
    // Phase C round 4: the 220px cap cut this id to `nvidia/nemotron-3-super-…`
    // at both proof sizes, and the cut part is the VARIANT — the only part
    // that identifies the model. The label node's text is the whole id now;
    // the sheet's measure (asserted below) is what keeps it on one line.
    const id = "nvidia/nemotron-3-super-120b-a12b";
    renderComposer({ selectedModel: id });
    const trigger = screen.getByRole("button", { name: "Agent mode and model" });
    const label = trigger.querySelector(".nex-composer-model-name");

    expect(label).not.toBeNull();
    expect(label!.textContent).toBe(id);
    // The full id is also the hover read AND the accessible description, so
    // even a shrunken box never leaves AT with the elided string.
    expect(trigger).toHaveAttribute("title", id);
  });

  it("falls back to the explanatory title only when there is no id to carry", () => {
    renderComposer({ selectedModel: null, models: [] });
    expect(screen.getByRole("button", { name: "Agent mode and model" })).toHaveAttribute(
      "title",
      "Model used for new messages",
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
  it("says nothing under a composer that can send", () => {
    // The hint row is reserved for the case where it is ACTIONABLE. "Enter
    // to send" restated what the field does and sat under every screen; a
    // ready composer now ends at its own border.
    renderComposer({ ready: true });
    expect(document.querySelector(".nex-composer-hint")).toBeNull();
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

describe("composer — no teaching line (D-03)", () => {
  // D-03 rejects the composer that opens with a row of teaching chips under
  // the field: "без наложений placeholder/подсказок/чипов". The row was the
  // pattern the brief calls teach chips, and it spent a line of the app's
  // most-used surface on two keyboard shortcuts the help dialog already
  // documents. What replaces it is nothing — the placeholder is the
  // affordance, and it is enough.

  it("renders no chip row in either mode, empty or filled", () => {
    for (const agentMode of [true, false]) {
      for (const draft of ["", "already typing"]) {
        const { unmount } = renderComposer({ agentMode, draft });
        expect(document.querySelector(".nex-composer-teach")).toBeNull();
        unmount();
      }
    }
  });

  it("keeps the native placeholder visible as the field's affordance, not as its name", () => {
    renderComposer();
    const field = screen.getByLabelText("Message");
    // The placeholder says what to do (Phase C round 4: a bare noun read as
    // a form label, not as an invitation to type)…
    expect(field).toHaveAttribute("placeholder", "Ask anything, or describe a task");
    // …and the accessible name stays the short noun, deliberately different:
    // a name names the control, it is not the instruction repeated.
    expect(field).toHaveAttribute("aria-label", "Message");
    expect(field.getAttribute("placeholder")).not.toBe(field.getAttribute("aria-label"));
  });

  it("keeps the shortcut registry as the one source for the help dialog", () => {
    // The chips are gone, but the registry they read from still owns the
    // keys — the shortcuts dialog renders from it, so a rebound key has one
    // place to change.
    expect(shortcutById("palette.toggle")).toBeDefined();
    expect(shortcutById("help.show-shortcuts")).toBeDefined();
    expect(shortcutKeys("no.such.shortcut")).toEqual([]);
    expect(SHORTCUTS.some((entry) => entry.id === "no.such.shortcut")).toBe(false);
  });
});

describe("composer — the field the user types in (Phase C round 2)", () => {
  // WHY THIS EXISTS. The composer shipped with its field present in the
  // DOM but visually absent on screen: the teaching legend was an opaque
  // `--card` overlay on the placeholder's line, the textarea has no chrome
  // of its own, and an empty composer therefore read as a stack of chrome
  // with no input. The behavioural tests here passed anyway, because jsdom
  // has no paint. So these pin the DOM contract (an editable, focusable
  // field with a placeholder, in BOTH modes) and the sheet contract that
  // keeps the placeholder uncovered.

  it("renders an editable, focusable field with its placeholder in Agent and in Chat", () => {
    for (const agentMode of [true, false]) {
      const { unmount, props } = renderComposer({ agentMode });
      const field = screen.getByLabelText("Message");

      // The element itself: a real textarea, editable, focusable, named.
      expect(field.tagName).toBe("TEXTAREA");
      expect(field).toBeEnabled();
      expect(field).toHaveAttribute("placeholder", "Ask anything, or describe a task");
      (field as HTMLTextAreaElement).focus();
      expect(field).toHaveFocus();

      // …and it takes text.
      fireEvent.change(field, { target: { value: "first message" } });
      expect(props.onDraftChange).toHaveBeenCalledWith("first message");

      unmount();
    }
  });

  it("keeps the field typeable in the narrowest proof window (1024×720)", () => {
    // jsdom renders no layout, so the width half is a statement about the
    // sheet: composer.css declares NO media query, so no breakpoint can
    // drop the field at a narrower window. The runtime half proves the
    // state path — the field exists and dispatches at that window size.
    const sheet = readRepoFile("src/styles/composer.css").replace(/\/\*[\s\S]*?\*\//g, "");
    expect(sheet).not.toContain("@media");

    const width = window.innerWidth;
    const height = window.innerHeight;
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1024 });
    Object.defineProperty(window, "innerHeight", { configurable: true, value: 720 });
    try {
      const { props } = renderComposer({ agentMode: true });
      const field = screen.getByLabelText("Message");
      expect(field).toBeEnabled();
      expect(field).toHaveAttribute("placeholder", "Ask anything, or describe a task");
      fireEvent.change(field, { target: { value: "at 1024" } });
      expect(props.onDraftChange).toHaveBeenCalledWith("at 1024");
    } finally {
      Object.defineProperty(window, "innerWidth", { configurable: true, value: width });
      Object.defineProperty(window, "innerHeight", { configurable: true, value: height });
    }
  });

  it("keeps the field's own box empty apart from the field", () => {
    // Nothing may sit on the placeholder's line. With the chip row gone the
    // box holds the textarea and nothing else, which is the strongest
    // version of the same contract.
    renderComposer();
    const field = screen.getByLabelText("Message");
    const box = field.parentElement!;
    expect(box).toHaveClass("nex-composer-field");
    expect(box.firstElementChild).toBe(field);
    expect(box.children).toHaveLength(1);
  });
});

describe("composer — the field's affordance (Phase C round 4)", () => {
  // WHY THIS EXISTS. The field has no chrome of its own: the shell is the
  // control, so the placeholder is the ONLY thing that says where typing
  // begins. It said "Message" — a bare noun, which reads as a form label
  // above the caret, not as an invitation. It is a sentence now, and the
  // accessible name is deliberately NOT that sentence.

  it("instructs in the placeholder while the accessible name stays a noun", () => {
    renderComposer();
    const field = screen.getByLabelText("Message");
    const placeholder = field.getAttribute("placeholder") ?? "";

    // A sentence that says what to do.
    expect(placeholder).toBe("Ask anything, or describe a task");
    expect(placeholder.trim().split(/\s+/).length).toBeGreaterThanOrEqual(4);

    // The name is the control's name, not the instruction: repeating the
    // sentence as the name makes AT announce it twice, and a sentence read
    // as a label is noise. This is the assertion that keeps the two apart.
    expect(field).toHaveAttribute("aria-label", "Message");
    expect(placeholder).not.toBe(field.getAttribute("aria-label"));

    // Short enough for the field's single line at the narrowest proof
    // window: the card is 760px at 1024 and the shell's inset leaves 726px,
    // so a 48-character ceiling cannot wrap (~350px at 0.9375rem).
    expect(placeholder.length).toBeLessThanOrEqual(48);
  });

  it("ships the same contract in both locales", () => {
    for (const locale of ["en", "ru"] as const) {
      const placeholder = tr(locale, "conv.messagePlaceholder");
      const name = tr(locale, "conv.messageAria");
      expect(placeholder.trim().split(/\s+/).length).toBeGreaterThanOrEqual(4);
      expect(placeholder.length).toBeLessThanOrEqual(48);
      expect(placeholder).not.toBe(name);
      // The bare nouns that shipped before are gone from both tables.
      expect(placeholder).not.toBe("Message");
      expect(placeholder).not.toBe("Сообщение");
    }
    // The RU sentence is the translated instruction, not a transliteration.
    expect(tr("ru", "conv.messagePlaceholder")).toBe(
      "Спросите что угодно или опишите задачу",
    );
  });
});

describe("composer — Send's two states (Phase C round 4)", () => {
  // The states are compared as TOKENS, not by eye. `ui/button` ships the
  // theme's disabled recipe as a full 1px hairline (`disabled:border-border`),
  // and a hairline is secondary-button grammar: on an empty draft Send read
  // as an enabled secondary action. The composer replaces that one utility
  // on the className (it cannot be done from composer.css — that sheet is
  // `layer(base)`, and a Tailwind utility outranks it at any specificity).
  const BORDER_TOKENS = ["border-transparent", "border-border"] as const;
  const SURFACE_TOKENS = ["bg-transparent", "bg-primary"] as const;
  const LABEL_TOKENS = ["text-muted-foreground", "text-primary-foreground"] as const;

  /** The token that actually paints one property in one state, resolved the
   *  way the browser resolves it: `disabled:` utilities apply only while the
   *  button is disabled, other variants (`hover:`, `focus-visible:`) are
   *  different states and are ignored, and within a state the last class of
   *  the group wins — tailwind-merge has already resolved the rest. */
  function paintToken(
    className: string,
    disabled: boolean,
    tokens: readonly string[],
  ): string {
    let found = "";
    for (const cls of className.split(/\s+/)) {
      const disabledOnly = cls.startsWith("disabled:");
      if (cls.includes(":") && !disabledOnly) continue;
      const bare = disabledOnly ? cls.slice("disabled:".length) : cls;
      if (!tokens.includes(bare)) continue;
      if (disabledOnly && !disabled) continue;
      found = bare;
    }
    return found;
  }

  function paint(className: string, disabled: boolean) {
    return {
      border: paintToken(className, disabled, BORDER_TOKENS),
      surface: paintToken(className, disabled, SURFACE_TOKENS),
      label: paintToken(className, disabled, LABEL_TOKENS),
    };
  }

  it("is unbordered and unfilled on an empty draft, and the light fill when armed", () => {
    const { rerender, props } = renderComposer({ draft: "", canSend: false });
    const unarmed = screen.getByRole("button", { name: /Send/ });
    expect(unarmed).toBeDisabled();

    // THE MERGE: the variant's hairline is replaced, not stacked.
    const classes = unarmed.className.split(/\s+/);
    expect(classes).toContain("disabled:border-transparent");
    expect(classes).not.toContain("disabled:border-border");

    const unarmedPaint = paint(unarmed.className, true);
    expect(unarmedPaint).toEqual({
      // No boundary, no fill, the theme's quiet text tier (5.64:1 on --card).
      border: "border-transparent",
      surface: "bg-transparent",
      label: "text-muted-foreground",
    });

    rerender(
      <LocaleProvider>
        <Composer {...props} draft="ready" canSend />
      </LocaleProvider>,
    );
    const armed = screen.getByRole("button", { name: /Send/ });
    expect(armed).toBeEnabled();

    const armedPaint = paint(armed.className, false);
    expect(armedPaint).toEqual({
      // The light fill and its own dark label (16.08:1 on --primary).
      border: "border-transparent",
      surface: "bg-primary",
      label: "text-primary-foreground",
    });

    // Different tokens on both axes that carry the read…
    expect(armedPaint.surface).not.toBe(unarmedPaint.surface);
    expect(armedPaint.label).not.toBe(unarmedPaint.label);
    // …and no control boundary in either state.
    expect(armedPaint.border).toBe("border-transparent");
    expect(unarmedPaint.border).toBe("border-transparent");
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


  it("gives the pill and its seat ONE radius, re-pointed on the group", () => {
    // G-18: the primitive's pill and the segment it covers both consume
    // `--seg-radius` (components.css), so the composer re-points that
    // ONE property on its two pill groups. Neither shape may declare a
    // `border-radius` of its own: a pill-only radius is exactly how the
    // pill drifted off its segment in Phase C (6px over 4.8px).
    const radiusRules = [
      ...code.matchAll(/([^{}]*)\{\s*--seg-radius:\s*([^;]+);/g),
    ];
    expect(radiusRules).toHaveLength(1);
    const [, selector = "", value = ""] = radiusRules[0]!;
    expect(selector).toContain(".nex-composer-pills.nex-seg");
    expect(selector).toContain(".nex-composer-autonomy.nex-seg");
    expect(value.trim()).toBe("calc(var(--radius-lg) - 2px)");

    // The seat keeps no radius declaration of its own — the primitive's
    // `.nex-seg:not(.nex-seg-connected) button` rule applies it.
    for (const seatSelector of [
      ".nex-composer-pills.nex-seg button",
      ".nex-composer-autonomy.nex-seg button",
    ]) {
      expect(ruleBody(seatSelector)).not.toMatch(/border-radius/);
    }

    // And the pill keeps none: the composer sheet does not paint the
    // indicator at all, because its corners are the seat's by
    // construction. A rule here is a second radius waiting to drift.
    expect(code).not.toMatch(/\.nex-seg-indicator/);

    // The derivation is real only if the primitive consumes that token
    // on both shapes: one re-point here moves the pill and the seat.
    const primitive = readRepoFile("src/styles/components.css").replace(
      /\/\*[\s\S]*?\*\//g,
      "",
    );
    const pill = /\.nex-seg-indicator\s*\{([\s\S]*?)\n\}/.exec(primitive)?.[1] ?? "";
    const seat =
      /\.nex-seg:not\(\.nex-seg-connected\) button\s*\{([\s\S]*?)\n\}/.exec(
        primitive,
      )?.[1] ?? "";
    expect(pill).toContain("border-radius: var(--seg-radius)");
    expect(seat).toContain("border-radius: var(--seg-radius)");
  });

  it("stacks the field above its teaching legend, and paints nothing over the placeholder", () => {
    // The regression QA caught: `.nex-composer-teach` was `position:
    // absolute` at the textarea's first line with `background: var(--card)`,
    // which covered the placeholder — the field's only affordance, because
    // it has no background or border of its own. The legend is in flow now.
    const fieldBlock = ruleBody(".nex-composer-field");
    expect(fieldBlock).toContain("display: flex");
    expect(fieldBlock).toContain("flex-direction: column");
    expect(fieldBlock).not.toContain("position: absolute");
    // The legend's row is reserved, so hiding the legend on the first
    // keystroke cannot collapse the card under the pointer.
    expect(fieldBlock).toMatch(/min-height:\s*\d+px/);

    const teachBlock = ruleBody(".nex-composer-teach");
    expect(teachBlock.length).toBeGreaterThan(0);
    expect(teachBlock).not.toContain("position: absolute");
    // No fill: an opaque legend is exactly the occluder.
    expect(teachBlock).not.toMatch(/background/);
    // No absolute inset and no clamp that could cut a wrapped line.
    expect(teachBlock).not.toMatch(/\b(top|left|right)\s*:/);
    expect(teachBlock).not.toContain("max-height");

    // The field keeps the height that makes its line reachable.
    const inputBlock = ruleBody(".nex-composer-shell .nex-composer-input");
    expect(inputBlock).toContain("min-height: 40px");
  });

  it("paints the model id, which no rule used to colour at all", () => {
    // The model id carried NO colour: on a <button> the UA's
    // `color: buttontext` beats inheritance, and with no `color-scheme`
    // declared that is black — ~1.1:1 on `--card`, the dimmest string in
    // the composer. It now takes the same quiet-secondary tone as the
    // metadata around it, so the assertion compares it to that tone
    // rather than restating a literal.
    const trigger = ruleBody(".nex-composer-model-trigger");
    expect(trigger).toContain("color: var(--muted-foreground)");

    const unselectedMode = ruleBody(".nex-composer-pills.nex-seg button");
    expect(unselectedMode).toContain("color: var(--muted-foreground)");

    // The other bare ghost button in the same popover had the same hole.
    expect(ruleBody(".nex-composer-model-footer button")).toContain(
      "color: var(--muted-foreground)",
    );
  });

  it("leaves the disabled Send to the theme's button, adding no colour of its own", () => {
    // Send's armed/unarmed difference is the vendored button's: `default` is
    // the light fill, and its disabled utilities swap it for `--muted-
    // foreground` text with no fill. Round 4 drops ONE of those utilities —
    // the hairline — on the className in Composer.tsx, because composer.css
    // is `layer(base)` and cannot outrank a Tailwind utility without
    // `!important` (reserved for the motion gate). This rule therefore stays
    // geometry-only; the state tokens themselves are pinned by
    // "composer — Send's two states".
    const send = ruleBody(".nex-composer-send");
    expect(send).toContain("flex-shrink: 0");
    expect(send).not.toMatch(/color|background|opacity|border/);
  });

  it("keeps the composer's reading measure equal to the thread's", () => {
    const thread = /--layout-measure-max:\s*([^;]+);/.exec(
      readRepoFile("src/index.css"),
    )?.[1];
    expect(thread).toBe("760px");
    expect(code).toMatch(/max-width:\s*760px/);
  });

  /** The inline-START padding a rule block ends up with, read the way the
   *  cascade reads it: declarations in source order, shorthand and longhand
   *  alike, later ones winning. `null` = the rule adds no inline padding. */
  const inlineStartPadding = (block: string): string | null => {
    let value: string | null = null;
    for (const declaration of block.split(";")) {
      const separator = declaration.indexOf(":");
      if (separator < 0) continue;
      const property = declaration.slice(0, separator).trim();
      const raw = declaration.slice(separator + 1).trim();
      if (raw === "") continue;
      if (property === "padding-inline-start") {
        value = raw;
      } else if (property === "padding-inline") {
        value = raw.split(/\s+/)[0]!;
      } else if (property === "padding") {
        const parts = raw.split(/\s+/);
        // 1 value: all sides · 2: block inline · 3: top inline bottom ·
        // 4: top right bottom left. `padding-block*` cannot touch this axis.
        value = parts.length === 1 ? parts[0]! : parts.length === 4 ? parts[3]! : parts[1]!;
      }
    }
    return value;
  };

  /** "No inline inset of its own" — a rule that says nothing, or says 0. */
  const addedInline = (block: string): string => {
    const value = inlineStartPadding(block);
    return value === null || value === "0" ? "none" : value;
  };

  it("puts ONE inner inset on the container and none on the rows", () => {
    // Phase C round 4: the four rows inside the card each spelled their own
    // horizontal padding (2 / 14 / 14 / 8px), so one card had three different
    // left edges. The inset lives on the shell now, once, and every row
    // inherits it — which is only true while no row adds one.
    const shell = ruleBody(".nex-composer-shell");
    // Both steps from the spacing scale: `--space-md` inline is the composer
    // region's own horizontal padding, `--space-sm` on the block axis keeps
    // the autonomy track off the card's top border (it was flush: 0px).
    expect(shell).toMatch(/padding:\s*var\(--space-sm\)\s+var\(--space-md\)/);

    // THE TRAP (Phase C round 5). `.nex-composer-input` is a SHARED class,
    // and its rule in conversation.css declares an inline padding. Omitting
    // the inline half here does NOT drop it — the shared 16px survives and
    // double-counts the shell's own 16px, which is exactly what pushed the
    // placeholder 17.6px right of the row beneath it. So the reset has to be
    // a declaration, and this asserts the shared rule really does carry one
    // (otherwise the reset below would be vacuous and the test meaningless).
    const sharedSheet = readRepoFile("src/styles/conversation.css").replace(
      /\/\*[\s\S]*?\*\//g,
      "",
    );
    const sharedField =
      /\.nex-composer-input\s*\{([\s\S]*?)\n\}/.exec(sharedSheet)?.[1] ?? "";
    expect(sharedField.length).toBeGreaterThan(0);
    expect(inlineStartPadding(sharedField)).toBe("var(--space-md)");

    // …and the composer's own rule resolves the field's inline start to 0,
    // so the text begins on the shell's inset like every other row.
    const fieldBlock = ruleBody(".nex-composer-shell .nex-composer-input");
    expect(inlineStartPadding(fieldBlock)).toBe("0");

    // RELATIONSHIP, not a measurement: every other row adds nothing to the
    // shell's inset, so the field's content edge IS the teaching row's
    // content edge (and the autonomy row's, and the bar's) — one value,
    // spelled once, on the container.
    const rows = [
      ".nex-agent-autonomy",
      ".nex-composer-teach",
      ".nex-composer-bar",
      ".nex-composer-attachments",
    ];
    for (const selector of rows) {
      const block = ruleBody(selector);
      expect(block.length).toBeGreaterThan(0);
      // No shorthand padding and no physical inline padding anywhere.
      expect(block).not.toMatch(/(^|[\s;{])padding\s*:/);
      expect(block).not.toMatch(/padding-(left|right)/);
      expect(addedInline(block)).toBe("none");
    }
    expect(addedInline(fieldBlock)).toBe(addedInline(ruleBody(".nex-composer-teach")));
    for (const selector of rows) {
      expect(addedInline(fieldBlock)).toBe(addedInline(ruleBody(selector)));
    }
  });

  it("gives the model label the measure of the longest catalog id", () => {
    // The label must render a real id in FULL: the cap is a `ch` count of the
    // element's own mono face, so it is a character count and follows the
    // font instead of guessing at pixels. It was `max-width: 220px` on the
    // trigger, which cut `nvidia/nemotron-3-super-120b-a12b` mid-token.
    const name = ruleBody(".nex-composer-model-name");
    const cap = Number(/max-width:\s*(\d+)ch/.exec(name)?.[1]);
    expect(cap).toBeGreaterThan(0);

    // …and the cap has to cover the longest id the catalogs can hand the
    // composer. Read out of the Rust lists rather than restated here, so a
    // longer id added there fails THIS test instead of truncating on screen.
    const catalogs = ["openai", "anthropic", "gemini"]
      .map((provider) =>
        readRepoFile(`src-tauri/src/infrastructure/providers/${provider}.rs`),
      )
      .join("\n");
    const ids = [
      ...catalogs.matchAll(/const \w*MODELS\w*: &\[&str\] = &\[([\s\S]*?)\];/g),
    ].flatMap((block) => [...block[1]!.matchAll(/"([^"]+)"/g)].map((match) => match[1]!));
    expect(ids.length).toBeGreaterThan(10);
    const longest = ids.reduce((a, b) => (b.length > a.length ? b : a));
    expect(longest.length).toBeLessThanOrEqual(cap);

    // The id from the round-4 frame is in the catalog and is the case that
    // regressed (33 characters; the old 220px cap cut it at ~29).
    expect(ids).toContain("nvidia/nemotron-3-super-120b-a12b");

    // The fixed cap is gone, and the trigger can still shrink gracefully
    // (`min-width: 0`) on a card narrower than the bar's budget.
    const trigger = ruleBody(".nex-composer-model-trigger");
    expect(trigger).not.toMatch(/max-width/);
    expect(trigger).toContain("min-width: 0");
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