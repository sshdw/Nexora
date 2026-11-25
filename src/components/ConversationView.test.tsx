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

import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useState } from "react";

import { tr } from "../lib/strings";
import type { Attachment, CommandError, Message } from "../lib/tauri";
import type { ConversationStore } from "../lib/useConversation";
import type { AgentRunView } from "../lib/useAgentRun";
import type { AutonomyStore } from "../lib/useAutonomy";
import type { WorkspaceStore } from "../lib/useWorkspace";
import { LOCALE_KEY, LocaleProvider } from "../lib/useLocale";
import ConversationView from "./ConversationView";

const {
  startAgentRunMock,
  decideRunPlanMock,
  continueAgentRunMock,
  extendAgentRunMock,
  reloadAgentMock,
  applyPlanDecisionMock,
  sendChatMock,
} = vi.hoisted(() => ({
  startAgentRunMock: vi.fn(async () => ({ run_id: 99 })),
  decideRunPlanMock: vi.fn(async () => ({
    plan: {
      id: 1,
      run_id: 42,
      conversation_id: 1,
      plan_content: "1. Plan step",
      decision_status: "accepted" as const,
      decision_reply: null,
      implemented_run_id: 100,
      created_at: 1000,
      decided_at: 1005,
    },
    launched_run_id: 100,
  })),
  continueAgentRunMock: vi.fn(async () => ({ run_id: 101 })),
  extendAgentRunMock: vi.fn(async () => {}),
  reloadAgentMock: vi.fn(async () => {}),
  applyPlanDecisionMock: vi.fn(),
  sendChatMock: vi.fn<ConversationStore["send"]>(async () => null),
}));

vi.mock("../lib/tauri", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/tauri")>()),
  startAgentRun: startAgentRunMock,
  decideRunPlan: decideRunPlanMock,
  continueAgentRun: continueAgentRunMock,
  extendAgentRun: extendAgentRunMock,
}));

// The pane owns three hooks that all reach the backend. They are replaced
// wholesale: this file is about the mode the pane STARTS in, not about
// history, attachments or runs, and an empty conversation is the state the
// app opens on. `conversationMessages` lets the message-body describe block
// (ADD-01) seed exactly the rows it pins without touching the others.
let conversationMessages: Message[] = [];
let draftAttachments: Attachment[] = [];
vi.mock("../lib/useConversation", () => ({
  useConversation: () => {
    const [error, setError] = useState<CommandError | null>(null);
    const [sending, setSending] = useState(false);
    return {
      messages: conversationMessages,
      loading: false,
      error,
      sending,
      reload: vi.fn(async () => {}),
      send: async (...args: Parameters<ConversationStore["send"]>) => {
        setError(null);
        setSending(true);
        const failure = await sendChatMock(...args);
        setError(failure);
        setSending(false);
        return failure;
      },
    };
  },
}));

vi.mock("../lib/useAttachments", () => ({
  useAttachments: () => ({
    attachments: draftAttachments,
    error: null,
    busy: false,
    pickAndAttach: vi.fn(async () => {}),
    remove: vi.fn(async () => {}),
    refresh: vi.fn(async () => {}),
  }),
}));

let agentRuns: AgentRunView[] = [];
vi.mock("../lib/useAgentRun", () => ({
  useAgentRun: () => ({
    runs: agentRuns,
    loading: false,
    error: null,
    reload: reloadAgentMock,
    // AC-102/ADD-08 lazy-steps seam added to the store.
    loadRunSteps: vi.fn(async () => {}),
    expandStep: vi.fn(),
    collapseStep: vi.fn(),
    applyPlanDecision: applyPlanDecisionMock,
  }),
}));

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
  draftAttachments = [];
  agentRuns = [];
  startAgentRunMock.mockClear();
  decideRunPlanMock.mockClear();
  continueAgentRunMock.mockClear();
  extendAgentRunMock.mockClear();
  reloadAgentMock.mockClear();
  applyPlanDecisionMock.mockClear();
  sendChatMock.mockReset();
  sendChatMock.mockResolvedValue(null);
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
      "true"
    );

    // Agent mode renders no attach control at all (R4-02: the run path takes
    // no attachments yet, so the button is absent, not a disabled promise).
    expect(screen.queryByRole("button", { name: "Add file" })).toBeNull();

    // …and the pill group describes itself as the switch it now is, not as
    // an opt-in beside a Chat default.
    expect(
      screen.getByTitle(tr("en", "conv.agentToggleTitle"))
    ).toBeInTheDocument();
  });

  it("keeps Chat one click away and holds the choice for the session", () => {
    const { rerender } = render(pane());

    fireEvent.click(
      within(modeGroup()).getByRole("radio", {
        name: "Chat mode — one answer per message",
      })
    );

    expect(
      within(modeGroup()).getByRole("radio", {
        name: "Chat mode — one answer per message",
      })
    ).toHaveAttribute("aria-checked", "true");
    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    expect(
      screen.getByRole("menuitem", { name: "Attach files" })
    ).toBeInTheDocument();

    // The mode is pane state, not a per-render constant: a re-render of the
    // same pane (here via the lifted draft) does not send the user back to
    // Agent, and nothing is persisted to the database.
    rerender(pane("half-typed"));
    expect(
      screen.getByRole("menuitem", { name: "Attach files" })
    ).toBeInTheDocument();
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
    expect(field).toHaveAttribute(
      "placeholder",
      "Ask anything, or describe a task"
    );

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
      within(assistantBody!).getByRole("button", { name: "Copy link" })
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
      within(userBody!).queryByRole("button", { name: "Copy link" })
    ).toBeNull();
  });
});

describe("Stage 5 PR 5B: ConversationView Plan toggle & Thinking integration", () => {
  it("shows Agent-only Plan toggle and toggles cleanly between Agent and Chat", () => {
    render(pane());

    const planBtn = screen.getByRole("button", { name: "Plan mode" });
    expect(planBtn).toHaveAttribute("aria-pressed", "false");
    fireEvent.click(planBtn);
    expect(planBtn).toHaveAttribute("aria-pressed", "true");

    fireEvent.click(
      within(modeGroup()).getByRole("radio", {
        name: "Chat mode — one answer per message",
      })
    );
    expect(screen.queryByRole("button", { name: "Plan mode" })).toBeNull();
  });
});

describe("managed attachments in Chat", () => {
  let stageDraft: (value: string) => void;

  function DraftPane({
    conversationId = 1,
    keyed = false,
  }: {
    conversationId?: number;
    keyed?: boolean;
  }) {
    const [draft, setDraft] = useState("Read this file");
    stageDraft = setDraft;
    return (
      <LocaleProvider>
        <ConversationView
          key={keyed ? conversationId : undefined}
          conversationId={conversationId}
          selectedProvider="gemini"
          selectedModel="gemini-3.6-flash"
          models={["gemini-3.6-flash"]}
          onSelectModel={() => {}}
          modelError={null}
          autonomy={autonomyStub()}
          onOpenSettings={() => {}}
          draft={draft}
          setDraft={setDraft}
          workspace={workspaceStub()}
        />
      </LocaleProvider>
    );
  }

  function selectChat() {
    fireEvent.click(
      within(modeGroup()).getByRole("radio", {
        name: "Chat mode — one answer per message",
      })
    );
  }

  beforeEach(() => {
    draftAttachments = [
      {
        id: 8,
        conversation_id: 1,
        message_id: null,
        file_name: "note.txt",
        file_path: "backend-only",
        file_size_bytes: 12,
        mime_type: "text/plain",
      },
    ];
  });

  it("retains text and the draft attachment after admission rejection, then retries", async () => {
    sendChatMock.mockResolvedValueOnce({
      kind: "invalid_input",
      message: "This model does not support attachments.",
    });
    const { container } = render(<DraftPane />);
    selectChat();

    fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(
        "This model does not support attachments."
      )
    );

    expect(screen.getByLabelText("Message")).toHaveValue("Read this file");
    expect(screen.getByText("note.txt")).toBeInTheDocument();
    expect(container.querySelectorAll(".nex-message")).toHaveLength(0);
    expect(screen.getByRole("button", { name: "Send message" })).toBeEnabled();

    fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    await waitFor(() =>
      expect(screen.getByLabelText("Message")).toHaveValue("")
    );
    expect(sendChatMock).toHaveBeenNthCalledWith(
      1,
      "Read this file",
      "gemini",
      "gemini-3.6-flash",
      [8],
      null
    );
    expect(sendChatMock).toHaveBeenNthCalledWith(
      2,
      "Read this file",
      "gemini",
      "gemini-3.6-flash",
      [8],
      null
    );
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(container.querySelectorAll(".nex-message")).toHaveLength(0);
  });

  it("keeps request failures retryable while rendering only persisted history", async () => {
    sendChatMock.mockImplementationOnce(async () => {
      conversationMessages = [
        {
          id: 1,
          conversation_id: 1,
          role: "user",
          content: "Read this file",
          provider_id: null,
          model_name: null,
          created_at: 1,
        },
      ];
      return { kind: "request", message: "Provider is offline." };
    });
    const { container } = render(<DraftPane />);
    selectChat();
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));

    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(
        "Provider is offline."
      )
    );
    expect(screen.getByLabelText("Message")).toHaveValue("Read this file");
    expect(container.querySelectorAll(".nex-message-user")).toHaveLength(1);
    expect(container.querySelectorAll(".nex-message-assistant")).toHaveLength(
      0
    );
    expect(screen.getByRole("button", { name: "Send message" })).toBeEnabled();
  });

  for (const failure of [
    null,
    { kind: "invalid_input", message: "Attachments are unsupported." },
  ]) {
    it(`preserves a newer staged draft when the pending send ${failure ? "fails" : "succeeds"}`, async () => {
      let finish!: (result: CommandError | null) => void;
      sendChatMock.mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            finish = resolve;
          })
      );
      render(<DraftPane />);
      selectChat();
      fireEvent.click(screen.getByRole("button", { name: "Send message" }));

      // App can stage a Prompt Library entry while the composer is busy.
      act(() => stageDraft("A newer staged prompt"));
      await act(async () => {
        finish(failure);
      });

      expect(screen.getByLabelText("Message")).toHaveValue(
        "A newer staged prompt"
      );
      expect(screen.getByText("note.txt")).toBeInTheDocument();
    });
  }

  it("does not clear a draft edited back to its submitted text", async () => {
    let finish!: (result: CommandError | null) => void;
    sendChatMock.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        })
    );
    render(<DraftPane />);
    selectChat();
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));

    act(() => stageDraft("A different staged prompt"));
    act(() => stageDraft("Read this file"));
    await act(async () => {
      finish(null);
    });

    expect(screen.getByLabelText("Message")).toHaveValue("Read this file");
  });

  for (const keyed of [false, true]) {
    it(`does not clear another conversation's draft after a pending send succeeds (${keyed ? "remounted" : "reused"} pane)`, async () => {
      let finish!: (result: CommandError | null) => void;
      sendChatMock.mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            finish = resolve;
          })
      );
      const { rerender } = render(<DraftPane keyed={keyed} />);
      selectChat();
      fireEvent.click(screen.getByRole("button", { name: "Send message" }));

      rerender(<DraftPane conversationId={2} keyed={keyed} />);
      await act(async () => {
        finish(null);
      });

      expect(screen.getByLabelText("Message")).toHaveValue("Read this file");
    });
  }
});

describe("managed attachments in Agent", () => {
  it("sends the visible draft IDs through Agent IPC", async () => {
    draftAttachments = [{ id: 8, conversation_id: 1, message_id: null, file_name: "note.rs", file_path: "backend-only", file_size_bytes: 12, mime_type: "text/plain" }];
    render(pane("Read this file"));
    expect(screen.getByText("note.rs")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    await waitFor(() => expect(startAgentRunMock).toHaveBeenCalledWith(1, "Read this file", "gemini", "gemini-3.6-flash", null, false, [8]));
  });

  it("renders missing historical attachments without exposing backend paths", () => {
    conversationMessages = [{ id: 1, conversation_id: 1, role: "user", content: "earlier", provider_id: null, model_name: null, created_at: 1, attachments: [{ id: 8, conversation_id: 1, message_id: 1, file_name: "legacy.pdf", file_path: "private-backend-path", file_size_bytes: 12, mime_type: "application/pdf", availability: "missing" }] }];
    render(pane(""));
    expect(screen.getByText("legacy.pdf")).toBeInTheDocument();
    expect(screen.getByText("Missing")).toBeInTheDocument();
    expect(screen.queryByText("private-backend-path")).not.toBeInTheDocument();
  });
});

describe("Stage 6 PR 6B: Plan decision flow and safe Continue", () => {
  it("submits with isPlan=true when Plan toggle is active", async () => {
    render(pane("Design the migration"));

    const planBtn = screen.getByRole("button", { name: "Plan mode" });
    fireEvent.click(planBtn);
    expect(planBtn).toHaveAttribute("aria-pressed", "true");

    fireEvent.click(screen.getByRole("button", { name: "Send message" }));

    await waitFor(() => {
      expect(startAgentRunMock).toHaveBeenCalledWith(
        1,
        "Design the migration",
        "gemini",
        "gemini-3.6-flash",
        null,
        true,
        []
      );
    });
  });

  it("renders composer-adjacent Plan decision prompt on reload, exits Plan mode on Yes/Custom reply, and records No cleanly", async () => {
    agentRuns = [
      {
        run_id: 42,
        conversation_id: 1,
        status: "completed",
        model: "gemini-3.6-flash",
        started_at: 1000,
        finished_at: 1002,
        error: null,
        steps: [],
        steps_loaded: true,
        total_steps: 1,
        pending_approval: null,
        pending_question: null,
        is_plan: true,
        plan_content: "1. Read files\n2. Plan diff",
        plan_decision_status: "pending",
      },
    ];

    const { rerender } = render(pane());

    const planBtn = screen.getByRole("button", { name: "Plan mode" });
    fireEvent.click(planBtn);
    expect(planBtn).toHaveAttribute("aria-pressed", "true");

    expect(screen.getByText("Start implementation?")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Yes" }));

    await waitFor(() => {
      expect(decideRunPlanMock).toHaveBeenCalledWith(
        42,
        "accepted",
        null,
        "gemini",
        "gemini-3.6-flash",
        null
      );
    });
    expect(screen.getByRole("button", { name: "Plan mode" })).toHaveAttribute(
      "aria-pressed",
      "false"
    );
    expect(applyPlanDecisionMock).toHaveBeenCalled();

    decideRunPlanMock.mockClear();
    fireEvent.click(screen.getByRole("button", { name: "No" }));
    await waitFor(() => {
      expect(decideRunPlanMock).toHaveBeenCalledWith(
        42,
        "rejected",
        null,
        null,
        null,
        null
      );
    });

    decideRunPlanMock.mockClear();
    fireEvent.click(screen.getByRole("button", { name: "Plan mode" }));
    fireEvent.click(screen.getByRole("button", { name: "Custom reply" }));
    fireEvent.change(screen.getByLabelText("Custom reply"), {
      target: { value: "Implement step 1 only" },
    });
    fireEvent.click(
      within(
        screen.getByRole("region", { name: "Start implementation?" })
      ).getByRole("button", { name: "Send" })
    );
    await waitFor(() => {
      expect(decideRunPlanMock).toHaveBeenCalledWith(
        42,
        "custom_reply",
        "Implement step 1 only",
        "gemini",
        "gemini-3.6-flash",
        null
      );
    });
    expect(screen.getByRole("button", { name: "Plan mode" })).toHaveAttribute(
      "aria-pressed",
      "false"
    );

    agentRuns = [
      {
        ...agentRuns[0]!,
        plan_decision_status: "accepted",
      },
    ];
    rerender(pane());
    expect(screen.queryByText("Start implementation?")).toBeNull();
  });

  it("wires Continue button on interrupted run to continueAgentRun rather than extendAgentRun", async () => {
    agentRuns = [
      {
        run_id: 77,
        conversation_id: 1,
        status: "interrupted",
        interrupted: true,
        model: "gemini-3.6-flash",
        started_at: 1000,
        finished_at: 1002,
        error: "app shutdown",
        steps: [],
        steps_loaded: true,
        total_steps: 2,
        pending_approval: null,
        pending_question: null,
      },
    ];

    render(pane());

    expect(continueAgentRunMock).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    await waitFor(() => {
      expect(continueAgentRunMock).toHaveBeenCalledWith(
        77,
        "gemini",
        "gemini-3.6-flash",
        null
      );
    });
    expect(extendAgentRunMock).not.toHaveBeenCalled();
  });
});

describe("Stage 12 PR 1: streaming Markdown, smart scroll, and IPC error formatting", () => {
  it("renders streaming agent response through MessageMarkdown preserving headings and lists", () => {
    agentRuns = [
      {
        run_id: 99,
        conversation_id: 1,
        status: "running",
        model: "gemini-3.6-flash",
        started_at: 1000,
        finished_at: null,
        error: null,
        steps: [],
        steps_loaded: true,
        total_steps: 0,
        pending_approval: null,
        pending_question: null,
        streaming_text: "## Summary\n\n- First item\n- Second item\n\n```ts\nconst x = 1;\n```",
      },
    ];

    const { container } = render(pane());
    const streamingArticle = container.querySelector(".nex-message-streaming");
    expect(streamingArticle).not.toBeNull();
    expect(streamingArticle!.querySelector("h2")?.textContent).toBe("Summary");
    expect(streamingArticle!.querySelectorAll("ul > li")).toHaveLength(2);
    expect(streamingArticle!.querySelector("pre code")?.textContent).toContain("const x = 1;");
  });

  it("does not hijack scroll when user scrolled up and shows the scroll-to-bottom button", () => {
    const { container, rerender } = render(pane());
    const thread = container.querySelector(".nex-thread") as HTMLDivElement;

    Object.defineProperty(thread, "scrollHeight", { value: 1200, configurable: true });
    Object.defineProperty(thread, "clientHeight", { value: 400, configurable: true });
    thread.scrollTop = 100;
    fireEvent.scroll(thread);

    const jumpBtn = screen.getByRole("button", { name: "Scroll to bottom" });
    expect(jumpBtn).toBeInTheDocument();

    agentRuns = [
      {
        run_id: 100,
        conversation_id: 1,
        status: "running",
        model: "gemini-3.6-flash",
        started_at: 1000,
        finished_at: null,
        error: null,
        steps: [],
        steps_loaded: true,
        total_steps: 0,
        pending_approval: null,
        pending_question: null,
        streaming_text: "New streamed token",
      },
    ];
    rerender(pane());

    expect(thread.scrollTop).toBe(100);

    fireEvent.click(jumpBtn);
    expect(thread.scrollTop).toBe(1200);
    expect(screen.queryByRole("button", { name: "Scroll to bottom" })).toBeNull();
  });

  it("extracts CommandError.message instead of rendering [object Object]", async () => {
    startAgentRunMock.mockRejectedValueOnce({
      kind: "request",
      message: "Provider timed out",
    });

    render(pane("Test prompt"));
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Provider timed out");
    expect(alert.textContent).not.toContain("[object Object]");
  });
});
