//! The pane's opening mode (R4-02) — which composer the user meets.
//!
//! The composer itself is covered in Composer.test.tsx with the mode handed
//! to it. What was never pinned anywhere is the half ConversationView owns:
//! the pane's INITIAL value. It opened in Chat, so the Agent-first composer
//! render was reachable only after a click, and R4-02 was never true in the
//! running app even though every composer test passed. These two tests pin
//! the seam between the two components, plus the pill group's title copy —
//! a title that still called Agent "opt-in" while Agent is the default is
//! the same defect in words.
//!
//! jsdom computes no cascade: what is asserted is presence/absence and ARIA
//! state, which is what decides the mode the user is in.

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { tr } from "../lib/strings";
import type { Message } from "../lib/tauri";
import type { AutonomyStore } from "../lib/useAutonomy";
import type { WorkspaceStore } from "../lib/useWorkspace";
import { LOCALE_KEY, LocaleProvider } from "../lib/useLocale";
import ConversationView from "./ConversationView";

// The pane owns three hooks that all reach the backend. They are replaced
// wholesale: this file is about the mode the pane STARTS in, not about
// history, attachments or runs, and an empty conversation is the state the
// app opens on. `conversationMessages` lets the message-body describe block
// (ADD-01) seed exactly the rows it pins without touching the others.
let conversationMessages: Message[] = [];
vi.mock("../lib/useConversation", () => ({
  useConversation: () => ({
    messages: conversationMessages,
    loading: false,
    error: null,
    sending: false,
    reload: vi.fn(async () => {}),
    send: vi.fn(async () => null),
  }),
}));

vi.mock("../lib/useAttachments", () => ({
  useAttachments: () => ({
    attachments: [],
    error: null,
    busy: false,
    pickAndAttach: vi.fn(async () => {}),
    remove: vi.fn(async () => {}),
    refresh: vi.fn(async () => {}),
  }),
}));

vi.mock("../lib/useAgentRun", () => ({
  useAgentRun: () => ({
    runs: [],
    loading: false,
    error: null,
    reload: vi.fn(async () => {}),
    // AC-102/ADD-08 lazy-steps seam added to the store.
    loadRunSteps: vi.fn(async () => {}),
    expandStep: vi.fn(),
    collapseStep: vi.fn(),
  }),
}));

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

function workspaceStub(): WorkspaceStore {
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
  };
}

/** The pane wired the way App wires it, with the two props a test may vary
 *  (the lifted draft, so a re-render can be provoked without remounting). */
function pane(draft = "") {
  return (
    <LocaleProvider>
      <ConversationView
        conversationId={1}
        selectedProvider="gemini"
        selectedModel="gemini-3.6-flash"
        models={["gemini-3.6-flash"]}
        onSelectModel={() => {}}
        modelError={null}
        autonomy={autonomyStub()}
        onOpenSettings={() => {}}
        draft={draft}
        setDraft={() => {}}
        workspace={workspaceStub()}
      />
    </LocaleProvider>
  );
}

/** The composer's two pills — the only radio group in the thread view. */
function modeGroup(): HTMLElement {
  return screen.getByRole("radiogroup", { name: "Agent mode" });
}

beforeEach(() => {
  conversationMessages = [];
  window.localStorage.setItem(LOCALE_KEY, "en");
});

afterEach(() => {
  cleanup();
  window.localStorage.removeItem(LOCALE_KEY);
  vi.restoreAllMocks();
});

describe("pane default mode (R4-02)", () => {
  it("opens the pane in Agent mode, with the attach control absent", () => {
    render(pane());

    const group = modeGroup();
    expect(within(group).getAllByRole("radio")).toHaveLength(2);
    expect(within(group).getByRole("radio", { name: "Agent" })).toHaveAttribute(
      "aria-checked",
      "true",
    );

    // Agent mode renders no attach control at all (R4-02: the run path takes
    // no attachments yet, so the button is absent, not a disabled promise).
    expect(screen.queryByRole("button", { name: "Add file" })).toBeNull();

    // …and the pill group describes itself as the switch it now is, not as
    // an opt-in beside a Chat default.
    expect(screen.getByTitle(tr("en", "conv.agentToggleTitle"))).toBeInTheDocument();
  });

  it("keeps Chat one click away and holds the choice for the session", () => {
    const { rerender } = render(pane());

    fireEvent.click(
      within(modeGroup()).getByRole("radio", {
        name: "Chat mode — one answer per message",
      }),
    );

    expect(
      within(modeGroup()).getByRole("radio", {
        name: "Chat mode — one answer per message",
      }),
    ).toHaveAttribute("aria-checked", "true");
    expect(screen.getByRole("button", { name: "Add file" })).toBeInTheDocument();

    // The mode is pane state, not a per-render constant: a re-render of the
    // same pane (here via the lifted draft) does not send the user back to
    // Agent, and nothing is persisted to the database.
    rerender(pane("half-typed"));
    expect(screen.getByRole("button", { name: "Add file" })).toBeInTheDocument();
  });

  it("opens on a real, typeable field with Send unarmed until there is a draft", () => {
    // What visual QA saw on a fresh conversation: a composer card with no
    // visible input, and a Send that read as armed. The field is the
    // composer's own (pinned in Composer.test.tsx); what the PANE owns is
    // the arming — `canSend` requires a non-empty trimmed draft, so an
    // empty conversation cannot send and Send is disabled. A draft arms it.
    const { rerender } = render(pane(""));

    const field = screen.getByLabelText("Message");
    expect(field.tagName).toBe("TEXTAREA");
    expect(field).toBeEnabled();
    expect(field).toHaveAttribute("placeholder", "Ask anything, or describe a task");

    const send = screen.getByRole("button", { name: /Send/ });
    expect(send).toBeDisabled();

    rerender(pane("half-typed"));
    expect(screen.getByRole("button", { name: /Send/ })).toBeEnabled();

    // Whitespace is not a draft: the guard trims before it counts.
    rerender(pane("   "));
    expect(screen.getByRole("button", { name: /Send/ })).toBeDisabled();
  });
});

describe("mode pill title copy", () => {
  for (const locale of ["en", "ru"] as const) {
    it(`describes a Chat/Agent switch in ${locale}, without opt-in or attachments`, () => {
      const title = tr(locale, "conv.agentToggleTitle");
      const [chat, agent] =
        locale === "en" ? ["Chat", "Agent"] : ["Чат", "Агент"];

      expect(title.toLowerCase()).toContain(chat.toLowerCase());
      expect(title.toLowerCase()).toContain(agent.toLowerCase());
      expect(title).not.toMatch(/opt-in|опциональн/i);
      expect(title).not.toMatch(/attach|вложен/i);
    });
  }
});

describe("message bodies (ADD-01)", () => {
  it("parses ASSISTANT bodies as markdown and keeps USER bodies plain text", () => {
    // The seam the ADD-01 swap owns: only model-authored bodies are
    // markdown-interpreted (05-ADD §3.1) — user text is data, not formatting.
    // jsdom computes no cascade: what is pinned is element presence and
    // absence, which is what decides "interpreted" vs "escaped text".
    conversationMessages = [
      {
        id: 1,
        conversation_id: 1,
        role: "user",
        content: "**not bold** [x](https://a.example)",
        provider_id: null,
        model_name: null,
        created_at: 1_700_000_000,
      },
      {
        id: 2,
        conversation_id: 1,
        role: "assistant",
        content: "**bold** [x](https://a.example)",
        provider_id: 1,
        model_name: "gemini-3.6-flash",
        created_at: 1_700_000_060,
      },
    ];
    render(pane());

    // Assistant side: the emphasis became an element, the link became the
    // copy-only affordance — never an anchor (02-SECURITY §4.2 req 3).
    const assistantBody = screen
      .getByText("bold")
      .closest<HTMLElement>(".nex-message-body");
    expect(assistantBody).not.toBeNull();
    expect(assistantBody?.querySelector("strong")).not.toBeNull();
    expect(assistantBody?.querySelector("a")).toBeNull();
    expect(assistantBody?.querySelector("[href]")).toBeNull();
    expect(
      within(assistantBody!).getByRole("button", { name: "Copy link" }),
    ).toBeInTheDocument();

    // User side: the same syntax survives literally as one escaped text
    // node — no strong, no link affordance, no structure.
    const userBody = screen
      .getByText("**not bold** [x](https://a.example)")
      .closest<HTMLElement>(".nex-message-body");
    expect(userBody).not.toBeNull();
    expect(userBody?.querySelector("strong")).toBeNull();
    expect(userBody?.querySelector("a")).toBeNull();
    expect(
      within(userBody!).queryByRole("button", { name: "Copy link" }),
    ).toBeNull();
  });
});
