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
import type { AutonomyStore } from "../lib/useAutonomy";
import { LOCALE_KEY, LocaleProvider } from "../lib/useLocale";
import ConversationView from "./ConversationView";

// The pane owns three hooks that all reach the backend. They are replaced
// wholesale: this file is about the mode the pane STARTS in, not about
// history, attachments or runs, and an empty conversation is the state the
// app opens on.
vi.mock("../lib/useConversation", () => ({
  useConversation: () => ({
    messages: [],
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
      />
    </LocaleProvider>
  );
}

/** The composer's two pills — the only radio group in the thread view. */
function modeGroup(): HTMLElement {
  return screen.getByRole("radiogroup", { name: "Agent mode" });
}

beforeEach(() => {
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
    expect(field).toHaveAttribute("placeholder", "Message");

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
