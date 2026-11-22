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
//!   3. MOTION     the composer's autonomy explainer renders its
//!                 finished sentence instantly (plain text, no JS
//!                 timeline exists in the tree to gate — P7, review
//!                 06 D-15). Plus the reverse check: the global
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

import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { readRepoFile } from "../test/readRepoFile";

import { SHORTCUTS, shortcutById, shortcutKeys } from "../lib/shortcuts";
import { tr } from "../lib/strings";
import { REDUCED_MOTION_QUERY } from "../lib/useReducedMotion";
import type { AutonomyStore } from "../lib/useAutonomy";
import type { Attachment, PromptSkill } from "../lib/tauri";
import { listPromptSkills, openExternalUrl, previewPromptContext, updatePromptSkill, viewPromptSkill } from "../lib/tauri";
import { type ComposerPromptDraft, useComposerContext } from "../lib/useComposerContext";
import { LOCALE_KEY, LocaleProvider } from "../lib/useLocale";
import type { WorkspaceStore } from "../lib/useWorkspace";
import ComposerAutonomyHint from "./ComposerAutonomyHint";
import Composer, { type ComposerProps } from "./Composer";

vi.mock("../lib/tauri", async (importOriginal) => ({
  ...await importOriginal<typeof import("../lib/tauri")>(),
  listPromptSkills: vi.fn().mockResolvedValue([]),
  openExternalUrl: vi.fn().mockResolvedValue(undefined),
  previewPromptContext: vi.fn(),
  updatePromptSkill: vi.fn(),
  viewPromptSkill: vi.fn(),
}));

// P7 (review 06 D-15): the vendored per-character typing component and
// its timeline dependency left the tree, and with them the file-wide
// tween spy this file used to prove the autonomy explainer starts no
// timeline. The no-timeline property is structural now — no timeline
// library exists to import — so the remaining assertions pin what the
// stylesheet gate still owns (see the REVERSE CHECK at the bottom of
// this file).

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

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  vi.mocked(listPromptSkills).mockReset().mockResolvedValue([]);
});

function autonomyStub(overrides: Partial<AutonomyStore> = {}): AutonomyStore {
  return {
    access: {
      profile: "standard",
      loading: false,
      saving: false,
      error: null,
      reload: vi.fn(async () => {}),
      setProfile: vi.fn(async () => null),
    },
    mode: "semi_autonomous",
    loading: false,
    error: null,
    reload: vi.fn(async () => {}),
    setMode: vi.fn(async () => null),
    ...overrides,
  };
}

/** The composer only reads the workspace store through the context row; a
 *  root-less store is the honest default for a send-flow test. */
function workspaceStub(
  overrides: Partial<WorkspaceStore> = {}
): WorkspaceStore {
  return {
    root: null,
    recent: [],
    roots: [],
    loading: false,
    saving: false,
    error: null,
    reload: vi.fn(async () => {}),
    pickFolder: vi.fn(async () => null),
    selectRecent: vi.fn(async () => null),
    addRootFolder: vi.fn(async () => null),
    addRoot: vi.fn(async () => null),
    removeRoot: vi.fn(async () => null),
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
    conversationId: 1,
    contextRevision: "",
    liveRunId: null,
    onStop: vi.fn(),
    workspace: workspaceStub(),
    autoFocusField: false,
    ...overrides,
  };
}

function renderComposer(overrides: Partial<ComposerProps> = {}) {
  const props = composerProps(overrides);
  const view = render(
    <LocaleProvider>
      <Composer {...props} />
    </LocaleProvider>
  );
  return { ...view, props };
}

/** A parent-shaped harness: the composer is controlled, so a mode
 *  switch and a draft only exist through an owner that holds both —
 *  exactly how ConversationView wires it (one draft per pane, one
 *  mode for the composer). Nothing here clears or sends on its own. */
function StatefulComposer({
  overrides = {},
  withPromptContext = false,
}: {
  overrides?: Partial<ComposerProps>;
  withPromptContext?: boolean;
}) {
  const [draft, setDraft] = useState(overrides.draft ?? "");
  const [agentMode, setAgentMode] = useState(overrides.agentMode ?? true);
  const Field = withPromptContext ? ComposerWithContext : Composer;
  return (
    <LocaleProvider>
      <Field
        {...composerProps(overrides)}
        draft={draft}
        onDraftChange={setDraft}
        agentMode={agentMode}
        onAgentModeChange={setAgentMode}
      />
    </LocaleProvider>
  );
}

function ComposerWithContext(props: ComposerProps) {
  const context = useComposerContext(props.conversationId, props.workspace.root, props.draft, false);
  return <Composer {...props} promptContext={context} />;
}

describe("composer — context selectors", () => {
  it.each(["file", "folder", "diff", "symbol"])("opens /@%s with Enter", async (kind) => {
    const onSubmit = vi.fn();
    render(<StatefulComposer withPromptContext overrides={{ onSubmit, canSend: true }} />);
    const field = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(field, { target: { value: `/@${kind}` } });
    fireEvent.keyDown(field, { key: "Enter" });
    const dialog = await screen.findByRole("dialog", { name: `@${kind}` });
    expect(within(dialog).getAllByRole("textbox")[0]).toHaveFocus();
    expect(field).toHaveValue(`/@${kind}`);
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("uses the keyboard highlight for a partial slash context command without changing the draft", async () => {
    render(<StatefulComposer withPromptContext />);
    const field = screen.getByRole("textbox", { name: "Message" });
    const draft = "/@f keep this";
    fireEvent.change(field, { target: { value: draft, selectionStart: 3, selectionEnd: 3 } });
    fireEvent.keyDown(field, { key: "ArrowDown" });
    fireEvent.keyDown(field, { key: "Enter" });
    expect(await screen.findByRole("dialog", { name: "@folder" })).toBeInTheDocument();
    expect(field).toHaveValue(draft);
  });

  const readSkill: PromptSkill = {
    name: "read", description: "Summarize text", scope: "personal", path: "read/SKILL.md",
    reference: { source_id: "read-skill", revision: "revision" }, sha256: "hash", commit: null,
    enabled: true, trusted: true, auto_select: false,
  };

  it("localizes declined skill trust in Russian instead of showing raw English", async () => {
    const untrustedSkill: PromptSkill = { ...readSkill, trusted: false };
    vi.mocked(listPromptSkills).mockResolvedValue([untrustedSkill]);
    vi.mocked(viewPromptSkill).mockResolvedValue({ summary: untrustedSkill, text: "Read Skill contents" });
    vi.mocked(updatePromptSkill).mockRejectedValue({
      kind: "confirmationRequired",
      message: "Skill trust was not confirmed.",
    });
    window.localStorage.setItem(LOCALE_KEY, "ru");
    try {
      render(<StatefulComposer withPromptContext />);
      const field = screen.getByRole("textbox", { name: tr("ru", "conv.messageAria") });
      fireEvent.change(field, { target: { value: "/read" } });
      await screen.findByText("/read");
      fireEvent.keyDown(field, { key: "Enter" });
      const dialog = await screen.findByRole("dialog", { name: "/read" });
      await within(dialog).findByText("Read Skill contents");
      fireEvent.click(within(dialog).getByRole("button", { name: tr("ru", "prompt.trust") }));
      expect(await within(dialog).findByRole("alert")).toHaveTextContent(tr("ru", "prompt.trustDeclined"));
      expect(within(dialog).queryByText("Skill trust was not confirmed.")).toBeNull();
    } finally {
      window.localStorage.removeItem(LOCALE_KEY);
    }
  });

  it("renders @diff preview inside the sheet preview class without prompt-library clamping", async () => {
    const diffText = [
      "HEAD abcdef1 → working tree",
      "diff --git a/sample/example.rs b/sample/example.rs",
      "index 1111111..2222222 100644",
      "--- a/sample/example.rs",
      "+++ b/sample/example.rs",
      "@@ -1 +1 @@",
      "-fn fixture_value() -> u32 { 1 }",
      "+fn fixture_value() -> u32 { 2 }",
    ].join("\n");
    vi.mocked(previewPromptContext).mockResolvedValue({
      reference: { selector: { kind: "diff", path: null }, revision: "diff-rev" },
      label: "HEAD → working tree",
      text: diffText,
      omitted: false,
    });
    render(<StatefulComposer withPromptContext />);
    const field = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(field, { target: { value: "@diff" } });
    fireEvent.keyDown(field, { key: "Enter" });
    const dialog = await screen.findByRole("dialog", { name: "@diff" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Preview" }));
    const pre = await within(dialog).findByText(/-fn fixture_value\(\)/);
    expect(pre.tagName).toBe("PRE");
    expect(pre).toHaveClass("nex-prompt-sheet-preview");
    expect(pre).not.toHaveClass("nex-prompt-preview");
    expect(pre.textContent).toContain("+fn fixture_value() -> u32 { 2 }");
  });

  it("ignores a delayed skill disable from a previous workspace", async () => {
    let finishDisable!: (summary: PromptSkill) => void;
    vi.mocked(listPromptSkills).mockResolvedValue([readSkill]);
    vi.mocked(viewPromptSkill).mockResolvedValue({ summary: readSkill, text: "Read Skill contents" });
    vi.mocked(updatePromptSkill).mockReturnValue(new Promise((resolve) => { finishDisable = resolve; }));
    vi.mocked(previewPromptContext).mockResolvedValue({
      reference: { selector: { kind: "file", path: "keep.txt" }, revision: "keep-rev" },
      label: "keep.txt",
      text: "Kept content",
      omitted: false,
    });

    function WorkspaceSwitchHarness() {
      const [root, setRoot] = useState("/ws-a");
      const [draft, setDraft] = useState("");
      const [saved, setSaved] = useState<ComposerPromptDraft | undefined>(undefined);
      const store = {
        value: saved,
        update: (edit: (current?: ComposerPromptDraft) => ComposerPromptDraft | undefined) => {
          setSaved((current) => {
            const next = edit(current);
            return next && next !== current ? next : current;
          });
        },
      };
      const context = useComposerContext(1, root, draft, false, store);
      return (
        <LocaleProvider>
          <button type="button" onClick={() => setRoot("/ws-b")}>Switch workspace</button>
          <Composer
            {...composerProps({ workspace: workspaceStub({ root }) })}
            draft={draft}
            onDraftChange={setDraft}
            promptContext={context}
          />
        </LocaleProvider>
      );
    }

    render(<WorkspaceSwitchHarness />);
    const field = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(field, { target: { value: "/read" } });
    await screen.findByText("/read");
    fireEvent.keyDown(field, { key: "Enter" });
    const dialog = await screen.findByRole("dialog", { name: "/read" });
    await within(dialog).findByText("Read Skill contents");
    fireEvent.click(within(dialog).getByRole("button", { name: "Disable skill" }));

    fireEvent.click(screen.getByRole("button", { name: "Switch workspace" }));
    fireEvent.change(field, { target: { value: "@file" } });
    fireEvent.keyDown(field, { key: "Enter" });
    const fileDialog = await screen.findByRole("dialog", { name: "@file" });
    fireEvent.change(within(fileDialog).getByRole("textbox"), { target: { value: "keep.txt" } });
    fireEvent.click(within(fileDialog).getByRole("button", { name: "Preview" }));
    await within(fileDialog).findByText("Kept content");
    fireEvent.click(within(fileDialog).getByRole("button", { name: "Include in message" }));
    expect(screen.getByRole("button", { name: "Remove keep.txt from this message" })).toBeInTheDocument();

    await act(async () => { finishDisable({ ...readSkill, enabled: false }); });
    expect(screen.getByRole("button", { name: "Remove keep.txt from this message" })).toBeInTheDocument();
  });

  it("waits for skill discovery before submitting a slash token", async () => {
    let discover!: (skills: PromptSkill[]) => void;
    vi.mocked(listPromptSkills).mockReturnValue(new Promise((resolve) => { discover = resolve; }));
    vi.mocked(viewPromptSkill).mockResolvedValue({ summary: readSkill, text: "Read Skill contents" });
    const onSubmit = vi.fn();
    render(<StatefulComposer withPromptContext overrides={{ canSend: true, onSubmit }} />);
    const field = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(field, { target: { value: "/read" } });
    fireEvent.keyDown(field, { key: "Enter" });
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    expect(onSubmit).not.toHaveBeenCalled();
    await act(async () => { discover([readSkill]); });
    fireEvent.keyDown(field, { key: "Enter" });
    expect(await screen.findByText("Read Skill contents")).toBeInTheDocument();
  });

  it.each(["Enter", "Send"])("prioritizes exact slash names when invoked with %s", async (method) => {
    vi.mocked(listPromptSkills).mockResolvedValue([readSkill]);
    vi.mocked(viewPromptSkill).mockResolvedValue({ summary: readSkill, text: "Read Skill contents" });
    render(<StatefulComposer withPromptContext overrides={{ canSend: true, onReview: vi.fn() }} />);
    const field = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(field, { target: { value: "/read" } });
    const commands = screen.getByRole("listbox", { name: "Available slash commands" });
    await within(commands).findByText("/read");
    expect(within(commands).getByText("/review")).toBeInTheDocument();
    if (method === "Enter") fireEvent.keyDown(field, { key: "Enter" });
    else fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    expect(await screen.findByText("Read Skill contents")).toBeInTheDocument();
    expect(viewPromptSkill).toHaveBeenCalledWith(1, readSkill.reference);
  });

  it("keeps the keyboard-highlighted slash option in the list viewport", async () => {
    vi.mocked(listPromptSkills).mockResolvedValue([readSkill]);
    render(<StatefulComposer withPromptContext />);
    const field = screen.getByRole("textbox", { name: "Message" });
    field.focus();
    fireEvent.change(field, { target: { value: "/" } });
    const list = screen.getByRole("listbox", { name: "Available slash commands" });
    const option = (await within(list).findByText("/read")).closest("button")!;
    vi.spyOn(list, "getBoundingClientRect").mockReturnValue(new DOMRect(0, 0, 240, 120));
    vi.spyOn(option, "getBoundingClientRect").mockReturnValue(new DOMRect(0, 240, 240, 30));
    fireEvent.keyDown(field, { key: "ArrowUp" });
    expect(list.scrollTop).toBe(150);
    expect(option).toHaveAttribute("aria-selected", "true");
    expect(field).toHaveAttribute("aria-activedescendant", option.id);
    expect(field).toHaveFocus();
  });

  it("clears staged context along with the text when /clear runs", async () => {
    vi.mocked(previewPromptContext).mockResolvedValue({
      reference: { selector: { kind: "file", path: "sample.txt" }, revision: "fixture" },
      label: "sample.txt", text: "Synthetic contents", omitted: false,
    });
    const onSubmit = vi.fn();
    render(<StatefulComposer withPromptContext overrides={{ onSubmit, canSend: true }} />);
    const field = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(field, { target: { value: "@file" } });
    fireEvent.keyDown(field, { key: "Enter" });
    const dialog = await screen.findByRole("dialog", { name: "@file" });
    fireEvent.change(within(dialog).getByRole("textbox"), { target: { value: "sample.txt" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Preview" }));
    await within(dialog).findByText("Synthetic contents");
    fireEvent.click(within(dialog).getByRole("button", { name: "Include in message" }));
    expect(screen.getByRole("button", { name: "Remove sample.txt from this message" })).toBeInTheDocument();
    fireEvent.change(field, { target: { value: "@folder" } });
    fireEvent.keyDown(field, { key: "Enter" });
    const folder = await screen.findByRole("dialog", { name: "@folder" });
    expect(within(folder).getByText(tr("en", "prompt.noShell"))).toBeInTheDocument();
    fireEvent.click(within(folder).getByRole("button", { name: "Close" }));
    fireEvent.change(field, { target: { value: "/clear" } });
    fireEvent.keyDown(field, { key: "Enter" });
    expect(field).toHaveValue("");
    expect(screen.queryByRole("button", { name: "Remove sample.txt from this message" })).toBeNull();
    fireEvent.change(field, { target: { value: "Fresh message" } });
    fireEvent.keyDown(field, { key: "Enter" });
    expect(onSubmit).toHaveBeenCalledOnce();
  });

  it("keeps the draft on Escape and removes only the included partial token at the caret", async () => {
    vi.mocked(previewPromptContext).mockResolvedValue({
      reference: { selector: { kind: "file", path: "sample.txt" }, revision: "fixture" },
      label: "sample.txt", text: "Synthetic contents", omitted: false,
    });
    const onSubmit = vi.fn();
    render(<StatefulComposer withPromptContext overrides={{ onSubmit, canSend: true }} />);
    const field = screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Message" });
    const original = "Keep @fi and this";
    field.focus();
    fireEvent.change(field, { target: { value: original, selectionStart: 8, selectionEnd: 8 } });
    fireEvent.keyDown(field, { key: "Enter" });
    let dialog = await screen.findByRole("dialog", { name: "@file" });
    fireEvent.keyDown(within(dialog).getByRole("textbox"), { key: "Escape" });
    await waitFor(() => {
      expect(dialog).not.toBeInTheDocument();
      expect(field).toHaveFocus();
    });
    expect(field).toHaveValue(original);
    fireEvent.change(field, { target: { value: "Keep @fil and this", selectionStart: 9, selectionEnd: 9 } });
    fireEvent.keyDown(field, { key: "Enter" });
    dialog = await screen.findByRole("dialog", { name: "@file" });
    fireEvent.change(within(dialog).getByRole("textbox"), { target: { value: "sample.txt" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Preview" }));
    expect(await within(dialog).findByText("Synthetic contents")).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "Include in message" }));
    expect(field).toHaveValue("Keep  and this");
    await waitFor(() => {
      expect(field).toHaveFocus();
      expect(field.selectionStart).toBe(5);
    });
    fireEvent.keyDown(field, { key: "Enter" });
    expect(onSubmit).toHaveBeenCalledOnce();
  });
});

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
    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "next" },
    });
    expect(props.onDraftChange).toHaveBeenCalledWith("next");
  });

  it("disables send unless the caller says it can send", () => {
    const { rerender, props } = renderComposer({
      draft: "hello",
      canSend: false,
    });
    const send = screen.getByRole("button", { name: /Send/ });
    expect(send).toBeDisabled();
    expect(screen.getByLabelText("Message")).toBeEnabled();

    rerender(
      <LocaleProvider>
        <Composer {...props} canSend />
      </LocaleProvider>
    );
    expect(screen.getByRole("button", { name: /Send/ })).toBeEnabled();
  });

  it("disables the field while a send or an agent run is in flight", () => {
    const { rerender, props } = renderComposer({
      sending: true,
      agentBusy: false,
    });
    expect(screen.getByLabelText("Message")).toBeDisabled();

    rerender(
      <LocaleProvider>
        <Composer {...props} sending={false} agentBusy />
      </LocaleProvider>
    );
    expect(screen.getByLabelText("Message")).toBeDisabled();
  });

  it("marks the send button busy without changing its send name", () => {
    const { rerender, props } = renderComposer({
      sending: true,
      canSend: false,
      agentMode: false,
    });
    const send = screen.getByRole("button", { name: "Send message" });
    expect(send).toBeDisabled();
    expect(send).toHaveAttribute("aria-busy", "true");

    rerender(
      <LocaleProvider>
        <Composer
          {...props}
          sending={false}
          agentBusy
          canSend={false}
          agentMode
        />
      </LocaleProvider>
    );
    const running = screen.getByRole("button", { name: "Send message" });
    expect(running).toBeDisabled();
    expect(running).toHaveAttribute("aria-busy", "true");
  });

  it("disables attach while sending or while attachments are loading", () => {
    const { rerender, props } = renderComposer({
      sending: true,
      agentMode: false,
    });
    expect(screen.getByRole("button", { name: "Add" })).toBeDisabled();

    rerender(
      <LocaleProvider>
        <Composer {...props} sending={false} attachmentsBusy />
      </LocaleProvider>
    );
    const plusBtn = screen.getByRole("button", { name: "Add" });
    expect(plusBtn).toBeEnabled();
    fireEvent.click(plusBtn);
    expect(
      screen.getByRole("menuitem", { name: "Attach files" })
    ).toBeDisabled();

    rerender(
      <LocaleProvider>
        <Composer
          {...props}
          sending={false}
          attachmentsBusy={false}
          agentBusy
        />
      </LocaleProvider>
    );
    expect(screen.getByRole("button", { name: "Add" })).toBeDisabled();
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

    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Attach files" }));
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
    expect(
      screen.getByRole("button", { name: "Remove notes.md" })
    ).toBeDisabled();

    rerender(
      <LocaleProvider>
        <Composer {...props} sending={false} attachmentsBusy />
      </LocaleProvider>
    );
    expect(
      screen.getByRole("button", { name: "Remove notes.md" })
    ).toBeDisabled();
  });

  it("lists attached files by name and size", () => {
    renderComposer({
      agentMode: false,
      attachments: [
        attachmentStub({ id: 3, file_name: "spec.pdf", file_size_bytes: 4096 }),
      ],
    });
    const list = screen.getByLabelText("Attached files");
    expect(within(list).getByText("spec.pdf")).toBeInTheDocument();
    expect(within(list).getByText("4.1 KB")).toBeInTheDocument();
  });
});

describe("composer — the one action (D2 / R5-07)", () => {
  it("is disabled only when the draft is empty and no run is live", () => {
    const { rerender, props } = renderComposer({ draft: "", canSend: false });
    expect(screen.getByRole("button", { name: "Send message" })).toBeDisabled();

    rerender(
      <LocaleProvider>
        <Composer {...props} draft="hello" canSend />
      </LocaleProvider>
    );
    expect(screen.getByRole("button", { name: "Send message" })).toBeEnabled();

    // A live run makes the same button a stop, and a stop is never disabled
    // by an empty draft: that is the whole point of the third state.
    rerender(
      <LocaleProvider>
        <Composer {...props} draft="" canSend={false} liveRunId={7} />
      </LocaleProvider>
    );
    const stop = screen.getByRole("button", { name: "Stop agent run" });
    expect(stop).toBeEnabled();
    expect(stop).toHaveAttribute("data-state", "stop");
    expect(stop.tagName).toBe("BUTTON");
  });

  it("swaps the arrow for the square, and dispatches stop instead of send", () => {
    const onStop = vi.fn();
    const { rerender, props } = renderComposer({
      draft: "hello",
      canSend: true,
      liveRunId: 7,
      onStop,
    });
    const stop = screen.getByRole("button", { name: "Stop agent run" });
    expect(stop).toHaveAttribute("data-state", "stop");
    fireEvent.click(stop);
    expect(onStop).toHaveBeenCalledTimes(1);
    expect(props.onSubmit).not.toHaveBeenCalled();

    rerender(
      <LocaleProvider>
        <Composer {...props} liveRunId={null} onStop={onStop} />
      </LocaleProvider>
    );
    const send = screen.getByRole("button", { name: "Send message" });
    expect(send).toHaveAttribute("data-state", "armed");
    fireEvent.click(send);
    expect(props.onSubmit).toHaveBeenCalledTimes(1);
    expect(onStop).toHaveBeenCalledTimes(1);
  });

  it("paints a plate in all three states, and only the plate and glyph change", () => {
    // R6-01: the arrow used to hang on the composer's own background — the
    // Round 5 finding. One geometry, three paints: idle is the muted plate,
    // armed and stop are the light one, and stop's glyph is filled.
    const { rerender, props } = renderComposer({ draft: "", canSend: false });
    const idle = screen.getByRole("button", { name: "Send message" });
    expect(idle).toHaveAttribute("data-state", "idle");
    expect(idle).toBeDisabled();
    expect(idle.className).toContain("bg-secondary");

    rerender(
      <LocaleProvider>
        <Composer {...props} draft="hello" canSend />
      </LocaleProvider>
    );
    const armed = screen.getByRole("button", { name: "Send message" });
    expect(armed).toHaveAttribute("data-state", "armed");
    expect(armed.className).toContain("bg-primary");
    expect(armed.className).not.toContain("bg-secondary");

    rerender(
      <LocaleProvider>
        <Composer {...props} draft="hello" canSend liveRunId={7} />
      </LocaleProvider>
    );
    const stop = screen.getByRole("button", { name: "Stop agent run" });
    expect(stop).toHaveAttribute("data-state", "stop");
    expect(stop.className).toContain("bg-primary");
    // The stop square is the one filled glyph in the composer; the arrow
    // beside it is a stroke glyph. Filled = no `fill="none"`.
    const glyph = stop.querySelector("svg");
    expect(glyph).not.toBeNull();
    expect(glyph).not.toHaveAttribute("fill", "none");
    expect(glyph).toHaveAttribute("fill", "currentColor");
  });

  it("never sends on Enter while a run is live", () => {
    const { props } = renderComposer({
      draft: "next message",
      canSend: false,
      liveRunId: 7,
    });
    fireEvent.keyDown(screen.getByLabelText("Message"), { key: "Enter" });
    expect(props.onSubmit).not.toHaveBeenCalled();
  });
});

describe("composer — context ring placement (D4)", () => {
  it("puts the ring before the model trigger, inside the one trailing group", () => {
    const { container } = renderComposer();
    const trailing = container.querySelector(".nex-composer-bar-trailing");
    expect(trailing).not.toBeNull();

    const autonomy = screen.getByRole("button", { name: "Access profile" });
    const ring = screen.getByRole("button", { name: "Context window: n/a" });
    const model = screen.getByRole("button", { name: "Agent mode and model" });
    const send = screen.getByRole("button", { name: "Send message" });

    // The group is ONE flex item: ring, model and Send still wrap together
    // in a split pane. Autonomy keeps its place at the head of the group.
    expect([...trailing!.children]).toEqual([autonomy, ring, model, send]);

    // Reference order in the row: ring → model → Send.
    expect(
      ring.compareDocumentPosition(model) & Node.DOCUMENT_POSITION_FOLLOWING
    ).toBeTruthy();
    expect(
      model.compareDocumentPosition(send) & Node.DOCUMENT_POSITION_FOLLOWING
    ).toBeTruthy();
  });

  it("keeps the same order in Chat mode, where autonomy is absent", () => {
    const { container } = renderComposer({ agentMode: false });
    const trailing = container.querySelector(".nex-composer-bar-trailing");
    expect(trailing).not.toBeNull();

    const ring = screen.getByRole("button", { name: "Context window: n/a" });
    const model = screen.getByRole("button", { name: "Agent mode and model" });
    const send = screen.getByRole("button", { name: "Send message" });
    expect([...trailing!.children]).toEqual([ring, model, send]);
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
      "true"
    );
    expect(
      within(group).getByRole("radio", {
        name: "Chat mode — one answer per message",
      })
    ).toHaveAttribute("aria-checked", "false");
    // Autonomy is reachable from the Agent pane, but it is a popover now
    // (D-03): the control row carries one trigger naming the live mode, and
    // the three choices exist only while it is open.
    expect(
      screen.getByRole("button", { name: "Access profile" })
    ).toHaveTextContent("Standard");
    expect(
      screen.queryByRole("radiogroup", { name: "Access profile" })
    ).not.toBeInTheDocument();
  });

  it("keeps the typed draft through a Chat → Agent round trip, and sends nothing", () => {
    const onSubmit = vi.fn();
    render(
      <StatefulComposer
        overrides={{ draft: "half a thought", canSend: true, onSubmit }}
      />
    );
    const field = screen.getByLabelText("Message");
    expect(field).toHaveValue("half a thought");
    expect(field).toBeEnabled();

    fireEvent.click(
      screen.getByRole("radio", { name: "Chat mode — one answer per message" })
    );
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
    const chat = within(group).getByRole("radio", {
      name: "Chat mode — one answer per message",
    });
    const agent = within(group).getByRole("radio", { name: "Agent" });

    expect(chat).toHaveAttribute("aria-checked", "true");
    expect(agent).toHaveAttribute("aria-checked", "false");
  });

  it("keeps its visible mode labels as Chat / Agent", () => {
    // Defect B (round-4 visual QA): the stage's view chip and these two
    // mode pills both read "Chat" in the same chip grammar, one above
    // the other. THE VIEW SWITCH is gone now (R6-05) — the context ring's
    // popover carries what the second view showed — and this pair stays
    // byte-identical to what shipped.
    renderComposer({ agentMode: false });
    const group = screen.getByRole("radiogroup", { name: "Agent mode" });
    const labels = within(group)
      .getAllByRole("radio")
      .map((radio) => radio.textContent);
    expect(labels).toEqual(["Chat", "Agent"]);
    expect(within(group).queryByText("Conversation")).toBeNull();

    // Byte-identical in both locales, hard-coded rather than read back from
    // the catalog: a sweep that renamed one key and not the other would keep
    // a key-to-key comparison green. (The stage's own view switch, whose
    // labels this pair used to have to tell apart, is gone — R6-05.)
    expect(tr("en", "conv.modeChat")).toBe("Chat");
    expect(tr("en", "conv.agentToggle")).toBe("Agent");
    expect(tr("ru", "conv.modeChat")).toBe("Чат");
    expect(tr("ru", "conv.agentToggle")).toBe("Агент");
  });

  it("switches agent mode from the pills", () => {
    const onAgentModeChange = vi.fn();
    renderComposer({ onAgentModeChange, agentMode: false });
    fireEvent.click(screen.getByRole("radio", { name: "Agent" }));
    expect(onAgentModeChange).toHaveBeenCalledWith(true);

    fireEvent.click(screen.getByRole("radio", { name: /Chat mode/ }));
    expect(onAgentModeChange).toHaveBeenCalledWith(false);
  });

  it("shows the access profile trigger in agent mode only, opens choices without legacy autonomy", () => {
    const { rerender, props } = renderComposer({ agentMode: false });
    expect(
      screen.queryByRole("button", { name: "Access profile" })
    ).not.toBeInTheDocument();

    rerender(
      <LocaleProvider>
        <Composer {...props} agentMode />
      </LocaleProvider>
    );
    fireEvent.click(screen.getByRole("button", { name: "Access profile" }));
    const profileGroup = screen.getByRole("radiogroup", {
      name: "Access profile",
    });
    expect(
      within(profileGroup).getByRole("radio", { name: "Standard" })
    ).toHaveAttribute("aria-checked", "true");
    expect(
      within(profileGroup).getByRole("radio", { name: "Read only" })
    ).toBeInTheDocument();
    expect(
      within(profileGroup).getByRole("radio", { name: "Supervised" })
    ).toBeInTheDocument();
    // Duplicated legacy autonomy section is removed from the Composer popover.
    expect(
      screen.queryByRole("radiogroup", { name: "Autonomy mode" })
    ).not.toBeInTheDocument();
    expect(screen.queryByText("Legacy autonomy")).not.toBeInTheDocument();
  });

  it("dispatches access profile changes from the store's own value", () => {
    const setProfile = vi.fn(async () => null);
    const autonomy = autonomyStub({
      access: {
        profile: "standard",
        loading: false,
        saving: false,
        error: null,
        reload: vi.fn(async () => {}),
        setProfile,
      },
    });
    renderComposer({ agentMode: true, autonomy });
    fireEvent.click(screen.getByRole("button", { name: "Access profile" }));
    fireEvent.click(screen.getByRole("radio", { name: "Supervised" }));
    expect(setProfile).toHaveBeenCalledWith("supervised");
  });

  it("keeps the access profile explainer's text in step with the store's profile", () => {
    mockReducedMotion(true);
    const autonomy = autonomyStub({
      access: {
        profile: "read_only",
        loading: false,
        saving: false,
        error: null,
        reload: vi.fn(async () => {}),
        setProfile: vi.fn(async () => null),
      },
    });
    const { rerender, props } = renderComposer({ agentMode: true, autonomy });
    fireEvent.click(screen.getByRole("button", { name: "Access profile" }));
    expect(
      screen.getByText(tr("en", "access.hint.read_only"))
    ).toBeInTheDocument();

    autonomy.access.profile = "supervised";
    rerender(
      <LocaleProvider>
        <Composer {...props} autonomy={autonomy} />
      </LocaleProvider>
    );
    expect(
      screen.getByText(tr("en", "access.hint.supervised"))
    ).toBeInTheDocument();
  });
});

describe("composer — attachment policy (R4-02 / G-20)", () => {
  it("renders the attach control in both Chat and Agent modes", () => {
    const { rerender, props } = renderComposer();
    const plusBtn = screen.getByRole("button", { name: "Add" });
    expect(plusBtn).toBeInTheDocument();
    fireEvent.click(plusBtn);
    expect(
      screen.getByRole("menuitem", { name: "Attach files" })
    ).toBeInTheDocument();
    expect(
      screen.getByRole("menuitem", { name: /Commands/ })
    ).toBeInTheDocument();
    expect(
      screen.getByRole("menuitem", { name: /Mentions/ })
    ).toBeInTheDocument();

    rerender(
      <LocaleProvider>
        <Composer {...props} agentMode={false} />
      </LocaleProvider>
    );
    expect(
      screen.getByRole("menuitem", { name: "Attach files" })
    ).toBeInTheDocument();
  });

  it("shows managed draft attachments and the attach action in Agent mode", () => {
    renderComposer({ attachments: [attachmentStub()] });
    expect(screen.getByLabelText("Attached files")).toBeInTheDocument();
    expect(screen.getByText("notes.md")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    expect(screen.getByRole("menuitem", { name: "Attach files" })).toBeEnabled();
  });

  it("keeps attachment-bearing live drafts visible and blocks text-only live actions", () => {
    const onClarifyNow = vi.fn();
    const onQueuePrompt = vi.fn();
    renderComposer({ attachments: [attachmentStub({ availability: "missing" })], draft: "next task", liveRunId: 4, onClarifyNow, onQueuePrompt });
    expect(screen.getByText("Missing")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Clarify now" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Queue" })).toBeDisabled();
    expect(screen.getByRole("status")).toHaveTextContent("Attachments stay in this draft");
    fireEvent.click(screen.getByRole("button", { name: "Clarify now" }));
    fireEvent.click(screen.getByRole("button", { name: "Queue" }));
    expect(onClarifyNow).not.toHaveBeenCalled();
    expect(onQueuePrompt).not.toHaveBeenCalled();
    expect(screen.getByText("notes.md")).toBeInTheDocument();
  });

});

describe("composer — model picker (R6-02)", () => {
  const trigger = () =>
    screen.getByRole("button", { name: "Agent mode and model" });
  const openPicker = () => fireEvent.click(trigger());
  const list = () => screen.getByRole("listbox", { name: "Model" });
  const search = () => screen.getByRole("searchbox", { name: "Search models" });

  it("shows the store's model and opens a real listbox with the provider's models", () => {
    renderComposer({ selectedModel: "gemini-3.6-flash" });
    expect(trigger()).toHaveTextContent("gemini-3.6-flash");

    openPicker();
    const options = within(list()).getAllByRole("option");
    expect(options.map((option) => option.textContent)).toEqual([
      "gemini-3.6-flash",
      "gemini-3.1-pro-preview",
    ]);
    expect(options[0]).toHaveAttribute("aria-selected", "true");
    expect(options[1]).toHaveAttribute("aria-selected", "false");
    // The selected row carries the one check; the other row does not.
    expect(
      options[0]?.querySelector(".nex-composer-model-option-check")
    ).not.toBeNull();
    expect(
      options[1]?.querySelector(".nex-composer-model-option-check")
    ).toBeNull();
  });

  it("lets the trigger shrink on a narrow card, so the label truncates before Send leaves", () => {
    // F5 defect (visual QA, frame 09-narrow-900): at 900px in split view the
    // wrapped trailing line overflowed its card — the trigger held its full
    // mono measure, the id ran past the card edge, and the chevron and Send
    // were pushed out of view. The shrink can only live on the className:
    // the vendored base string ships `shrink-0` (ui/button.tsx) and
    // composer.css is `layer(base)`, which cannot outrank a utility.
    renderComposer({ selectedModel: "nvidia/nemotron-3-ultra-550b-a55b" });
    const picker = trigger();
    // tailwind-merge must have EVICTED the base's bare `shrink-0` for this
    // later `shrink` — both present is the pre-fix state, and `shrink-0`
    // wins in the layer order, returning the overflow.
    expect(picker).toHaveClass("shrink");
    expect(picker).not.toHaveClass("shrink-0");
    // The svg-scoped variant stays: the chevron itself must not compress.
    expect(picker.className).toContain("[&_svg]:shrink-0");
    // …and the label inside carries the truncation contract that makes the
    // shrink honest (ellipsis, never paint-over — pinned on the sheet in
    // "composer sheet — theme linkage" below).
    expect(picker.querySelector(".nex-composer-model-name")).not.toBeNull();
  });

  it("filters the list as the query is typed, case-insensitively", () => {
    renderComposer();
    openPicker();
    expect(search()).toHaveFocus();

    fireEvent.change(search(), { target: { value: "PRO" } });
    const options = within(list()).getAllByRole("option");
    expect(options).toHaveLength(1);
    expect(options[0]).toHaveTextContent("gemini-3.1-pro-preview");
  });

  it("distinguishes an empty catalog from an empty search", () => {
    renderComposer({ models: [] });
    openPicker();
    expect(screen.getByText("No models configured")).toBeInTheDocument();
    // Nothing to search when there is nothing to list.
    expect(
      screen.queryByRole("searchbox", { name: "Search models" })
    ).toBeNull();
  });

  it("says the search matched nothing without claiming the catalog is empty", () => {
    renderComposer();
    openPicker();
    fireEvent.change(search(), { target: { value: "no such model" } });
    expect(screen.getByText("No models match your search")).toBeInTheDocument();
    expect(screen.queryByText("No models configured")).toBeNull();
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("moves the active row with the arrow keys and Home/End", () => {
    renderComposer({ models: ["a-model", "b-model", "c-model"] });
    openPicker();
    const options = within(list()).getAllByRole("option");
    expect(search()).toHaveAttribute("aria-activedescendant", options[0]!.id);

    fireEvent.keyDown(search(), { key: "ArrowDown" });
    expect(search()).toHaveAttribute("aria-activedescendant", options[1]!.id);
    expect(options[1]).toHaveAttribute("data-active", "true");

    fireEvent.keyDown(search(), { key: "End" });
    expect(search()).toHaveAttribute("aria-activedescendant", options[2]!.id);

    fireEvent.keyDown(search(), { key: "ArrowDown" });
    // Wraps, so the list never traps the cursor at an edge.
    expect(search()).toHaveAttribute("aria-activedescendant", options[0]!.id);

    fireEvent.keyDown(search(), { key: "Home" });
    expect(search()).toHaveAttribute("aria-activedescendant", options[0]!.id);
    fireEvent.keyDown(search(), { key: "ArrowUp" });
    expect(search()).toHaveAttribute("aria-activedescendant", options[2]!.id);
  });

  it("picks the active row on Enter and closes", async () => {
    const onSelectModel = vi.fn();
    renderComposer({ onSelectModel });
    openPicker();
    fireEvent.keyDown(search(), { key: "ArrowDown" });
    fireEvent.keyDown(search(), { key: "Enter" });

    expect(onSelectModel).toHaveBeenCalledWith("gemini-3.1-pro-preview");
    await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());
  });

  it("closes on Escape without picking", async () => {
    const onSelectModel = vi.fn();
    renderComposer({ onSelectModel });
    openPicker();
    fireEvent.keyDown(search(), { key: "Escape" });
    expect(onSelectModel).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());
  });

  it("dispatches the clicked model and keeps no copy of the selection", () => {
    const onSelectModel = vi.fn();
    renderComposer({ onSelectModel });
    openPicker();
    fireEvent.click(
      screen.getByRole("option", { name: "gemini-3.1-pro-preview" })
    );

    expect(onSelectModel).toHaveBeenCalledWith("gemini-3.1-pro-preview");
    expect(trigger()).toHaveTextContent("gemini-3.6-flash");
  });

  it("says so plainly when no model is selected", () => {
    renderComposer({ selectedModel: null, models: [] });
    expect(trigger()).toHaveTextContent("No model selected");
  });

  it("renders a long id in full and keeps it as the trigger's title", () => {
    // Phase C round 4: the 220px cap cut this id to `nvidia/nemotron-3-super-…`
    // at both proof sizes, and the cut part is the VARIANT — the only part
    // that identifies the model. The label node's text is the whole id now;
    // the sheet's measure (asserted below) is what keeps it on one line.
    const id = "nvidia/nemotron-3-super-120b-a12b";
    renderComposer({ selectedModel: id });
    const label = trigger().querySelector(".nex-composer-model-name");

    expect(label).not.toBeNull();
    expect(label!.textContent).toBe(id);
    // The full id is also the hover read AND the accessible description, so
    // even a shrunken box never leaves AT with the elided string.
    expect(trigger()).toHaveAttribute("title", id);
  });

  it("falls back to the explanatory title only when there is no id to carry", () => {
    renderComposer({ selectedModel: null, models: [] });
    expect(trigger()).toHaveAttribute("title", "Model used for new messages");
  });

  it("surfaces a model-selection failure inside the picker", () => {
    renderComposer({ modelError: "provider unreachable" });
    openPicker();
    expect(screen.getByRole("alert")).toHaveTextContent("provider unreachable");
  });

  it("offers provider settings from the picker", () => {
    const onOpenSettings = vi.fn();
    renderComposer({ onOpenSettings });
    openPicker();
    fireEvent.click(screen.getByRole("button", { name: "Provider settings" }));
    expect(onOpenSettings).toHaveBeenCalled();
  });

  it("truncates long model ids cleanly on a single line with title attribute", () => {
    const sheet = readRepoFile("src/styles/composer.css").replace(
      /\/\*[\s\S]*?\*\//g,
      ""
    );
    const menu =
      /\.nex-composer-model-menu\s*\{([\s\S]*?)\n\}/.exec(sheet)?.[1] ?? "";
    expect(menu).toContain("overflow-x: hidden");

    const listBlock =
      /\.nex-composer-model-list\s*\{([\s\S]*?)\n\}/.exec(sheet)?.[1] ?? "";
    expect(listBlock).toContain("overflow-x: hidden");
    expect(listBlock).toContain("flex-direction: column");

    const option =
      /\.nex-composer-model-option-name\s*\{([\s\S]*?)\n\}/.exec(sheet)?.[1] ??
      "";
    expect(option).toContain("overflow: hidden");
    expect(option).toContain("text-overflow: ellipsis");
    expect(option).toContain("white-space: nowrap");

    // No `.nex-seg` re-point survives: the list is a plain listbox now, so
    // the primitive's inline-grid root cannot come back through this hook.
    expect(sheet).not.toMatch(/\.nex-composer-model-list\.nex-seg/);

    const longId = "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free";
    renderComposer({ selectedModel: longId, models: [longId] });
    openPicker();
    const label = within(list())
      .getByRole("option")
      .querySelector(".nex-composer-model-option-name");
    expect(label).toHaveAttribute("title", longId);
  });
});

describe("composer — hint row", () => {
  it("says nothing under a composer that can send", () => {
    // The hint row is reserved for the case where it is ACTIONABLE. "Enter
    // to send" restated what the field does and sat under every screen; a
    // ready composer now ends at its own border.
    renderComposer({ ready: true });
    expect(document.querySelector(".nex-composer-hint")).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Open Settings" })
    ).not.toBeInTheDocument();
  });

  it("offers Settings when no provider or model is configured", () => {
    const onOpenSettings = vi.fn();
    renderComposer({
      ready: false,
      selectedModel: null,
      models: [],
      onOpenSettings,
    });
    expect(
      screen.getByText(
        "Choose a provider and model in Settings to send messages."
      )
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
    expect(field).toHaveAttribute(
      "placeholder",
      "Ask anything, or describe a task"
    );
    // …and the accessible name stays the short noun, deliberately different:
    // a name names the control, it is not the instruction repeated.
    expect(field).toHaveAttribute("aria-label", "Message");
    expect(field.getAttribute("placeholder")).not.toBe(
      field.getAttribute("aria-label")
    );
  });

  it("keeps the shortcut registry as the one source for the help dialog", () => {
    // The chips are gone, but the registry they read from still owns the
    // keys — the shortcuts dialog renders from it, so a rebound key has one
    // place to change.
    expect(shortcutById("palette.toggle")).toBeDefined();
    expect(shortcutById("help.show-shortcuts")).toBeDefined();
    expect(shortcutKeys("no.such.shortcut")).toEqual([]);
    expect(SHORTCUTS.some((entry) => entry.id === "no.such.shortcut")).toBe(
      false
    );
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
      expect(field).toHaveAttribute(
        "placeholder",
        "Ask anything, or describe a task"
      );
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
    const sheet = readRepoFile("src/styles/composer.css").replace(
      /\/\*[\s\S]*?\*\//g,
      ""
    );
    expect(sheet).not.toContain("@media");

    const width = window.innerWidth;
    const height = window.innerHeight;
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      value: 1024,
    });
    Object.defineProperty(window, "innerHeight", {
      configurable: true,
      value: 720,
    });
    try {
      const { props } = renderComposer({ agentMode: true });
      const field = screen.getByLabelText("Message");
      expect(field).toBeEnabled();
      expect(field).toHaveAttribute(
        "placeholder",
        "Ask anything, or describe a task"
      );
      fireEvent.change(field, { target: { value: "at 1024" } });
      expect(props.onDraftChange).toHaveBeenCalledWith("at 1024");
    } finally {
      Object.defineProperty(window, "innerWidth", {
        configurable: true,
        value: width,
      });
      Object.defineProperty(window, "innerHeight", {
        configurable: true,
        value: height,
      });
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
      "Спросите что угодно или опишите задачу"
    );
  });
});

describe("composer — Send's three states (R6-01)", () => {
  // The states are compared as TOKENS, not by eye. `ui/button` ships the
  // theme's disabled recipe as a full 1px hairline (`disabled:border-border`),
  // and a hairline is secondary-button grammar: on an empty draft Send read
  // as an enabled secondary action. The composer replaces that one utility
  // on the className (it cannot be done from composer.css — that sheet is
  // `layer(base)`, and a Tailwind utility outranks it at any specificity).
  //
  // R6-01: idle is no longer unfilled — the arrow used to hang on the
  // composer's own background. It sits on the muted plate now; the light
  // plate is armed and stop, and stop swaps the arrow for a filled square.
  const BORDER_TOKENS = ["border-transparent", "border-border"] as const;
  const SURFACE_TOKENS = [
    "bg-transparent",
    "bg-primary",
    "bg-secondary",
  ] as const;
  const LABEL_TOKENS = [
    "text-muted-foreground",
    "text-primary-foreground",
  ] as const;

  /** The token that actually paints one property in one state, resolved the
   *  way the browser resolves it: `disabled:` utilities apply only while the
   *  button is disabled, other variants (`hover:`, `focus-visible:`) are
   *  different states and are ignored, and within a state the last class of
   *  the group wins — tailwind-merge has already resolved the rest. */
  function paintToken(
    className: string,
    disabled: boolean,
    tokens: readonly string[]
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

  it("paints the muted plate on an empty draft, and the light fill when armed", () => {
    const { rerender, props } = renderComposer({ draft: "", canSend: false });
    const unarmed = screen.getByRole("button", { name: /Send/ });
    expect(unarmed).toBeDisabled();

    // THE MERGE: the variant's hairline is replaced, not stacked.
    const classes = unarmed.className.split(/\s+/);
    expect(classes).toContain("disabled:border-transparent");
    expect(classes).not.toContain("disabled:border-border");

    const unarmedPaint = paint(unarmed.className, true);
    expect(unarmedPaint).toEqual({
      // No boundary, the muted plate, the theme's quiet text tier.
      border: "border-transparent",
      surface: "bg-secondary",
      label: "text-muted-foreground",
    });

    rerender(
      <LocaleProvider>
        <Composer {...props} draft="ready" canSend />
      </LocaleProvider>
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

describe("composer — reduced-motion gate (plain-text explainer)", () => {
  it("renders the finished sentence instantly", () => {
    mockReducedMotion(true);
    render(
      <LocaleProvider>
        <ComposerAutonomyHint
          mode="semi_autonomous"
          text="Semi-auto: routine steps run alone."
        />
      </LocaleProvider>
    );

    // The whole sentence, present in the first frame — the explainer
    // is PLAIN TEXT ALWAYS (NEXORA-DESIGN-SYSTEM.md §5(b)). It used to
    // be a per-character typing ornament with a tween spy proving no
    // timeline started; P7 removed the vendored typing component and
    // its timeline library from the tree entirely (review 06 D-15), so
    // the property this test guards is structural: there is no JS
    // timeline machinery left to gate, and the finished sentence is the
    // only render there is.
    expect(
      screen.getByText("Semi-auto: routine steps run alone.")
    ).toBeInTheDocument();
  });

  it("renders no explainer for a mode it does not know", () => {
    mockReducedMotion(true);
    const { container } = render(
      <LocaleProvider>
        <ComposerAutonomyHint mode={"nonsense" as never} text="unused" />
      </LocaleProvider>
    );
    expect(container.innerHTML).toBe("");
  });

  it("REVERSE CHECK — the global CSS gate still collapses every merged animation", () => {
    // This PR adds motion, so the gate it relies on has to still
    // be the one gate: one block, in one file, holding the end
    // frame and freezing loops. Asserted against the file that
    // ships, exactly like src/lib/motion.test.tsx does.
    const css = readRepoFile("src/styles/motion.css").replace(
      /\/\*[\s\S]*?\*\//g,
      ""
    );
    const block =
      /@media \(prefers-reduced-motion: reduce\) \{([\s\S]*?)\n\}/.exec(css);
    expect(block).not.toBeNull();
    const declarations = block![1];
    expect(declarations).toMatch(/animation-duration:\s*0\.01ms\s*!important/);
    expect(declarations).toMatch(/transition-duration:\s*0\.01ms\s*!important/);
    expect(declarations).toMatch(/animation-iteration-count:\s*1\s*!important/);
    expect(declarations).toMatch(
      /animation-fill-mode:\s*forwards\s*!important/
    );
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
    new RegExp(
      `${selector.replace(/\./g, "\\.")}\\s*\\{([\\s\\S]*?)\\n\\}`
    ).exec(code)?.[1] ?? "";

  it("paints from shadcn semantic tokens only", () => {
    // The ban is on the M3E COLOUR system reaching this sheet —
    // `--nex-sys-color-*`. The canonical `--nex-sys-motion-*` names are
    // not paint (they resolve through motion.css, which owns motion the
    // way index.css owns colour) and the sheet consumes them since the
    // D-10 call-site rename (review 06) — carved out explicitly so the
    // carve-out is visible rather than a prefix accident.
    const m3eRefs = [
      ...code.matchAll(/var\(\s*(--nex-sys-(?!motion-)[\w-]+)/g),
    ].map((m) => m[1]);
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
    const theme = readRepoFile("src/index.css").replace(
      /\/\*[\s\S]*?\*\//g,
      ""
    );
    const dark =
      /:root,\s*:root\[data-theme="dark"\]\s*\{([\s\S]*?)\n\}/.exec(
        theme
      )?.[1] ?? "";
    const defined = new Set(
      [...dark.matchAll(/(--[a-z0-9-]+)\s*:/gi)].map((m) => m[1])
    );
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
      // D-10 (review 06): the sheet consumes the canonical
      // --nex-sys-motion-* names after the call-site rename; they
      // resolve through the same aliases in motion.css.
      "--nex-sys-motion-duration-fast",
      "--nex-sys-motion-easing-standard",
    ]);
    const missing = [...code.matchAll(/var\(\s*(--[a-z0-9-]+)/g)]
      .map((m) => m[1])
      .filter((name) => !known.has(name));
    expect([...new Set(missing)].sort()).toEqual([]);
  });

  it("adds no animation of its own — the global gate is the only motion source", () => {
    // Every transition must go through a canonical --nex-sys-motion-*
    // duration (D-10 rename), and
    // this sheet must declare no keyframes: a composer-local timeline
    // would be a second motion source the reduced-motion block in
    // motion.css still has to catch.
    expect(code).not.toMatch(/@keyframes/);
    expect(code).not.toMatch(/animation(-name)?\s*:/);
    const transitions = [...code.matchAll(/transition:[^;]+;/g)].map(
      (m) => m[0]
    );
    expect(transitions.length).toBeGreaterThan(0);
    for (const declaration of transitions) {
      expect(declaration).toContain("var(--nex-sys-motion-");
    }
  });

  it("keeps the mode groups on the segmented primitive, never the model list", () => {
    // R6-02: the model list stopped being a segmented group. Its
    // `inline-grid` root resolved its implicit column to max-content and
    // grew past the popover. The two MODE groups still use the primitive,
    // so the opt-out is local to the list (pinned in its own describe).
    for (const hook of ["nex-composer-pills", "nex-composer-autonomy"]) {
      expect(code).toMatch(new RegExp(`\\.${hook}\\.nex-seg`));
    }
    expect(code).not.toMatch(/\.nex-composer-model-list\.nex-seg/);
  });

  it("lets each popover sheet own its width", () => {
    // The primitive used to ship `w-72`, and a utilities-layer width
    // outranks every `layer(base)` width — so all three composer popovers
    // silently rendered at 288px while their sheets said 300/320/340. The
    // primitive now carries no width; these are the declarations that
    // actually paint.
    const primitive = readRepoFile("src/components/ui/popover.tsx").replace(
      /\/\*[\s\S]*?\*\//g,
      ""
    );
    expect(primitive).not.toMatch(/\bw-72\b/);
    for (const [selector, width] of [
      [".nex-composer-model-menu", "300px"],
      [".nex-composer-workspace-menu", "320px"],
      [".nex-composer-context-menu", "min(420px, calc(100vw - 32px))"],
    ] as const) {
      const start = code.indexOf(`${selector} {`);
      expect(start, `${selector} has no rule`).toBeGreaterThan(-1);
      const block = code.slice(start, code.indexOf("}", start));
      expect(block, selector).toContain(`width: ${width}`);
    }
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
      ""
    );
    const pill =
      /\.nex-seg-indicator\s*\{([\s\S]*?)\n\}/.exec(primitive)?.[1] ?? "";
    const seat =
      /\.nex-seg:not\(\.nex-seg-connected\) button\s*\{([\s\S]*?)\n\}/.exec(
        primitive
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
      "color: var(--muted-foreground)"
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
      readRepoFile("src/index.css")
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
        value =
          parts.length === 1
            ? parts[0]!
            : parts.length === 4
              ? parts[3]!
              : parts[1]!;
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
      ""
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
    expect(addedInline(fieldBlock)).toBe(
      addedInline(ruleBody(".nex-composer-teach"))
    );
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
    // AC-302 moved the OpenAI and Gemini lists into `catalog.rs` (the
    // executor modules keep only re-exports); Anthropic still holds its
    // list inline, so the pin reads those two files.
    const catalogs = ["catalog", "anthropic"]
      .map((file) =>
        readRepoFile(`src-tauri/src/infrastructure/providers/${file}.rs`)
      )
      .join("\n");
    const ids = [
      ...catalogs.matchAll(/const \w*MODELS\w*: &\[&str\] = &\[([\s\S]*?)\];/g),
    ].flatMap((block) =>
      [...block[1]!.matchAll(/"([^"]+)"/g)].map((match) => match[1]!)
    );
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

  it("keeps the narrow-card overflow chain intact (label truncates, controls stay)", () => {
    // F5 defect (frame 09-narrow-900): on a wrapped line the trailing
    // group's BOX shrinks to the card (`min-width: 0` on the group), the
    // trigger's `shrink` utility (ModelPicker.tsx — a sheet rule cannot
    // outrank the base string's `shrink-0`) then shrinks the button, and
    // the label's ellipsis cuts the id, so the chevron and Send never
    // leave the card. Every link of that chain is load-bearing.
    expect(ruleBody(".nex-composer-bar-trailing")).toContain("min-width: 0");

    const name = ruleBody(".nex-composer-model-name");
    expect(name).toContain("overflow: hidden");
    expect(name).toContain("text-overflow: ellipsis");
    expect(name).toContain("white-space: nowrap");
  });

  it("gives the workspace row the same narrow-card contract as the model row", () => {
    // The workspace chip + branch share the composer card (the row sits
    // above the shell), so at the same narrow widths its folder + branch
    // must truncate instead of overflowing: the button caps at the card
    // (min-width 0 lets it get there), and head/branch are the parts CSS
    // cuts — middle ellipsis with a fixed tail, per WorkspaceContextRow.
    const row = ruleBody(".nex-composer-context");
    expect(row).toContain("max-width: 100%");
    expect(row).toContain("min-width: 0");
    expect(ruleBody(".nex-composer-context-head")).toContain(
      "text-overflow: ellipsis"
    );
    expect(ruleBody(".nex-composer-context-branch")).toContain(
      "text-overflow: ellipsis"
    );
  });

  it("registers no second stylesheet entry or unlayered import", () => {
    const entry = readRepoFile("src/index.css").replace(
      /\/\*[\s\S]*?\*\//g,
      ""
    );
    expect(entry).toMatch(
      /@import\s+"\.\/styles\/composer\.css"\s+layer\(base\);/
    );
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
    ""
  );
  const composerSheet = readRepoFile("src/styles/composer.css").replace(
    /\/\*[\s\S]*?\*\//g,
    ""
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
    const strips = [
      ...composerSheet.matchAll(
        /\.(nex-composer-strip-error)\s*\{([\s\S]*?)\n\}/g
      ),
    ];
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
    expect(composerSheet).not.toMatch(
      /(?<!shell )\.nex-composer-input\s*[,{:]/
    );
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
    // class for exactly that reason. The model list is NOT in this
    // set any more: it is a plain listbox (R6-02), not a segmented
    // primitive, so there is no `.nex-seg` there to re-point.
    for (const hook of ["nex-composer-pills", "nex-composer-autonomy"]) {
      expect(composerSheet).toMatch(new RegExp(`\\.${hook}\\.nex-seg`));
    }
    expect(composerSheet).toMatch(/\.nex-composer-attachments \.nex-chip/);
    expect(composerSheet).toMatch(
      /\.nex-composer \.nex-seg button:focus-visible/
    );
  });
});

// D1: the empty screen IS the composer, so the caret belongs in the field
// when a conversation opens. The guard is the whole contract — an empty
// screen must never pull focus out of a dialog or away from wherever the
// reader already is — and the latch keeps it from re-claiming focus on every
// later render of the same pane.
describe("composer — empty-screen focus (D1)", () => {
  it("focuses the field when nothing else holds focus", () => {
    renderComposer({ autoFocusField: true });
    expect(screen.getByLabelText("Message")).toHaveFocus();
  });

  it("never steals focus from another field", () => {
    const other = document.createElement("input");
    other.setAttribute("aria-label", "Elsewhere");
    document.body.append(other);
    try {
      other.focus();
      renderComposer({ autoFocusField: true });
      expect(other).toHaveFocus();
      expect(screen.getByLabelText("Message")).not.toHaveFocus();
    } finally {
      other.remove();
    }
  });

  it("leaves focus to a modal dialog on top", () => {
    const modal = document.createElement("div");
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");
    document.body.append(modal);
    try {
      renderComposer({ autoFocusField: true });
      expect(screen.getByLabelText("Message")).not.toHaveFocus();
    } finally {
      modal.remove();
    }
  });

  it("claims focus once per pane, not on every later open flag", () => {
    const { rerender, props } = renderComposer({ autoFocusField: false });
    const field = screen.getByLabelText("Message");
    expect(field).not.toHaveFocus();

    rerender(
      <LocaleProvider>
        <Composer {...props} autoFocusField />
      </LocaleProvider>
    );
    expect(field).toHaveFocus();

    // The reader moved on; flipping the flag again must not yank them back.
    field.blur();
    rerender(
      <LocaleProvider>
        <Composer {...props} autoFocusField={false} />
      </LocaleProvider>
    );
    rerender(
      <LocaleProvider>
        <Composer {...props} autoFocusField />
      </LocaleProvider>
    );
    expect(field).not.toHaveFocus();
  });
});

describe("composer — Stage 5 PR 5B: Plan toggle and / + @ triggers", () => {
  it("shows the Plan toggle in Agent mode only and reflects honest UI state", () => {
    const onPlanModeChange = vi.fn();
    const { rerender, props } = renderComposer({
      agentMode: true,
      planMode: false,
      onPlanModeChange,
    });

    const planBtn = screen.getByRole("button", { name: "Plan mode" });
    expect(planBtn).toBeEnabled();
    expect(planBtn).toHaveAttribute("aria-pressed", "false");
    expect(screen.queryByRole("status")).toBeNull();

    fireEvent.click(planBtn);
    expect(onPlanModeChange).toHaveBeenCalledWith(true);

    rerender(
      <LocaleProvider>
        <Composer {...props} agentMode={true} planMode={true} onPlanModeChange={onPlanModeChange} />
      </LocaleProvider>
    );
    expect(screen.getByRole("button", { name: "Plan mode" })).toHaveAttribute(
      "aria-pressed",
      "true"
    );
    expect(
      screen.getByText(/Plan mode active — read-only planning/)
    ).toBeInTheDocument();

    // Switching to Chat removes the Plan toggle and its banner.
    rerender(
      <LocaleProvider>
        <Composer {...props} agentMode={false} planMode={true} onPlanModeChange={onPlanModeChange} />
      </LocaleProvider>
    );
    expect(screen.queryByRole("button", { name: "Plan mode" })).toBeNull();
    expect(
      screen.queryByText(/Plan mode active — read-only planning/)
    ).toBeNull();
  });

  it("omits separate / and @ toolbar buttons and opens slash commands from the + menu", () => {
    const onAgentModeChange = vi.fn();
    const onDraftChange = vi.fn();
    const onOpenSettings = vi.fn();
    renderComposer({
      agentMode: true,
      draft: "/ch",
      onAgentModeChange,
      onDraftChange,
      onOpenSettings,
    });

    expect(
      screen.queryByRole("button", { name: "Slash commands" })
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Mention context" })
    ).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    fireEvent.click(screen.getByRole("menuitem", { name: /Commands/ }));
    const listbox = screen.getByRole("listbox", {
      name: "Available slash commands",
    });
    const chatOpt = within(listbox).getByRole("option", { name: /\/chat/ });
    fireEvent.click(chatOpt);

    expect(onAgentModeChange).toHaveBeenCalledWith(false);
    expect(onDraftChange).toHaveBeenCalledWith("");
  });

  it("opens context mention menu from the + menu and inserts workspace or folder actions", () => {
    const onDraftChange = vi.fn();
    const pickFolder = vi.fn(async () => null);
    renderComposer({
      agentMode: true,
      draft: "Check ",
      onDraftChange,
      workspace: workspaceStub({
        root: "/repos/nexora",
        recent: ["/repos/nexora", "/repos/docs"],
        pickFolder,
      }),
    });

    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    fireEvent.click(screen.getByRole("menuitem", { name: /Mentions/ }));
    const listbox = screen.getByRole("listbox", {
      name: "Available context references",
    });
    fireEvent.click(within(listbox).getByRole("option", { name: /@nexora/ }));
    expect(onDraftChange).toHaveBeenCalledWith("Check @nexora ");

    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    fireEvent.click(screen.getByRole("menuitem", { name: /Mentions/ }));
    fireEvent.click(
      within(
        screen.getByRole("listbox", { name: "Available context references" })
      ).getByRole("option", { name: /Choose workspace folder/ })
    );
    expect(pickFolder).toHaveBeenCalledTimes(1);
  });

  it("opens / and @ popovers when typing in the textarea without stealing focus", () => {
    const onDraftChange = vi.fn();
    renderComposer({
      agentMode: true,
      draft: "",
      onDraftChange,
    });

    const field = screen.getByLabelText("Message");
    (field as HTMLTextAreaElement).focus();
    expect(field).toHaveFocus();

    fireEvent.change(field, { target: { value: "/" } });
    expect(
      screen.getByRole("listbox", { name: "Available slash commands" })
    ).toBeInTheDocument();
    expect(field).toHaveFocus();

    fireEvent.change(field, { target: { value: "Check @" } });
    expect(
      screen.getByRole("listbox", { name: "Available context references" })
    ).toBeInTheDocument();
    expect(field).toHaveFocus();
  });

  it("anchors / and @ popovers to the top edge of the composer shell and opens Plan tooltip above", () => {
    const { container } = renderComposer({ agentMode: true });
    const shell = container.querySelector(".nex-composer-shell");
    expect(shell).not.toBeNull();
    const topAnchors = shell!.querySelectorAll(".nex-composer-top-anchor");
    expect(topAnchors).toHaveLength(2);
    const planBtn = screen.getByRole("button", { name: "Plan mode" });
    const tooltipBubble = planBtn.parentElement?.querySelector(
      '[data-slot="tooltip-bubble"]'
    );
    expect(tooltipBubble).toHaveAttribute("data-side", "top");
  });

  it("renders Clarify now and Queue buttons while a run is live without breaking Stop", () => {
    const onStop = vi.fn();
    const onClarifyNow = vi.fn();
    const onQueuePrompt = vi.fn();
    renderComposer({
      agentMode: true,
      ready: true,
      liveRunId: 42,
      draft: "Focus on unit tests",
      onStop,
      onClarifyNow,
      onQueuePrompt,
    });

    const clarifyBtn = screen.getByRole("button", { name: "Clarify now" });
    const queueBtn = screen.getByRole("button", { name: "Queue" });
    const stopBtn = screen.getByRole("button", { name: "Stop agent run" });

    fireEvent.click(clarifyBtn);
    expect(onClarifyNow).toHaveBeenCalledTimes(1);

    fireEvent.click(queueBtn);
    expect(onQueuePrompt).toHaveBeenCalledTimes(1);

    fireEvent.click(stopBtn);
    expect(onStop).toHaveBeenCalledTimes(1);
  });

  it("displays ChatGPT plan usage notice and Manage usage link at the composer only when ChatGPT subscription is active", () => {
    vi.mocked(openExternalUrl).mockClear();
    const { unmount } = renderComposer({
      selectedConnectionId: "openai",
      selectedModel: "gpt-5.2",
    });
    expect(screen.queryByText("Using your ChatGPT plan")).toBeNull();
    expect(screen.queryByRole("link", { name: "Manage usage" })).toBeNull();
    unmount();

    renderComposer({
      selectedConnectionId: "chatgpt_subscription",
      selectedModel: "gpt-5.4-plan",
      connectionGroups: [
        {
          connectionId: "chatgpt_subscription",
          connectionName: "ChatGPT subscription",
          models: ["gpt-5.4-plan"],
          modelLabels: { "gpt-5.4-plan": "GPT-5.4" },
        },
      ],
    });
    expect(screen.getByText("Using your ChatGPT plan")).toBeInTheDocument();
    const manageLink = screen.getByRole("link", { name: "Manage usage" });
    expect(manageLink).toHaveAttribute("href", "https://chatgpt.com/#settings");
    fireEvent.click(manageLink);
    expect(openExternalUrl).toHaveBeenCalledWith(
      "https://chatgpt.com/#settings"
    );
  });
});
