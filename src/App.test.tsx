import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";

import { save } from "@tauri-apps/plugin-dialog";

import App, { buildConnectionGroups, mergeConnectionModels } from "./App";
import { rememberSession } from "./lib/lastSession";
import type { ConnectionModelCapabilities, ContextSelector } from "./lib/tauri";
import type { ProviderStatus } from "./lib/useProviders";

const { mockInvoke, mockListen, state } = vi.hoisted(() => {
  const state = {
    supported: [
      {
        name: "openai",
        display_name: "OpenAI",
        models: ["gpt-5.2", "gpt-5.2-mini"],
      },
      {
        name: "anthropic",
        display_name: "Anthropic",
        models: ["claude-sonnet-4"],
      },
      {
        name: "openai_compat",
        display_name: "OpenAI-Compatible",
        models: [] as string[],
      },
    ],
    configured: [
      { id: 1, name: "openai", display_name: "OpenAI" },
      { id: 2, name: "anthropic", display_name: "Anthropic" },
      { id: 3, name: "openai_compat", display_name: "OpenAI-Compatible" },
    ],
    credentialed: new Set<string>(["openai", "anthropic", "openai_compat"]),
    settings: new Map<string, string | null>([
      ["provider.selected", "openai"],
      ["provider.model", "gpt-5.2"],
    ]),
    capabilities: [] as ConnectionModelCapabilities[],
    pendingTurn: null as Promise<void> | null,
    pendingRevert: null as Promise<void> | null,
    undoFixture: false,
  };
  const mockListen = vi.fn(async () => () => {});
  const mockInvoke = vi.fn(
    async (command: string, args?: Record<string, unknown>) => {
      switch (command) {
        case "list_conversations":
          return [
            ...[2, 3].map((id) => ({
              id,
              title: id === 2 ? "Inactive session" : "Archived session",
              status: id === 3 ? "archived" : "active",
              created_at: 1_700_000_000,
              updated_at: 1_700_000_050,
              workspace_root: null,
            })),
            {
              id: 1,
              title: "Test conversation",
              status: "active",
              created_at: 1_700_000_000,
              updated_at: 1_700_000_100,
              workspace_root: null,
            },
          ];
        case "supported_providers":
          return JSON.parse(JSON.stringify(state.supported));
        case "list_providers":
          return JSON.parse(JSON.stringify(state.configured));
        case "has_provider_credential":
          return state.credentialed.has(String(args?.provider ?? ""));
        case "get_setting":
          return state.settings.get(String(args?.key ?? "")) ?? null;
        case "set_setting": {
          const key = String(args?.key ?? "");
          const value = (args?.value as string | null | undefined) ?? null;
          if (value === null) state.settings.delete(key);
          else state.settings.set(key, value);
          return null;
        }
        case "list_model_capabilities":
          return JSON.parse(JSON.stringify(state.capabilities));
        case "get_model_capabilities": {
          const connectionId = String(args?.connectionId ?? "");
          const modelId = String(args?.modelId ?? "");
          return (
            state.capabilities.find(
              (c) => c.connection_id === connectionId && c.model_id === modelId
            ) ?? {
              connection_id: connectionId,
              model_id: modelId,
              context_window: 200_000,
              supported_efforts: ["low", "medium", "high"],
              supports_thinking: true,
              supports_attachments: true,
              supports_tools: true,
              provenance: "built_in_catalog",
              has_manual_override: false,
            }
          );
        }
        case "request_confirmation":
          return "export-confirmation";
        case "conversation_history":
          return [];
        case "list_conversation_runs":
        case "list_agent_runs":
        case "get_durable_run_events":
        case "list_workspace_recent":
        case "list_attachments":
        case "list_draft_attachments":
        case "list_prompts":
        case "list_tasks":
        case "list_permission_rules":
        case "list_run_checkpoints":
          return state.undoFixture && args?.runId === 77
            ? [{
                id: 1, run_id: 77, turn_index: 0, file_path: "sample.txt",
                pre_blob_sha256: "pre", post_blob_sha256: "post",
                created_at: 1_700_000_010, reverted_at: null,
              }]
            : [];
        case "revert_turn_checkpoints":
          await state.pendingRevert;
          return [];
        case "list_run_questions":
        case "list_work_queue":
        case "list_prompt_skills":
          return [];
        case "preview_prompt_context": {
          if ((args?.selector as ContextSelector).kind === "diff") {
            return {
              reference: { selector: args?.selector, revision: "review-fixture" },
              label: "Tracked diff", text: "A changed line", omitted: false,
            };
          }
          const selector = args?.selector as Extract<ContextSelector, { kind: "file" | "folder" }>;
          return {
            reference: { selector, revision: selector.path }, label: selector.path,
            text: `Preview of ${selector.path}`, omitted: false,
          };
        }
        case "start_agent_run":
        case "send_message":
          await state.pendingTurn;
          return { run_id: 11 };
        case "list_conversation_run_events":
          if (state.undoFixture && args?.conversationId === 1 && (args?.afterEventId ?? 0) === 0) {
            return {
              events: [{
                event_id: 1, conversation_id: 1, run_id: 77, kind: "run_finished",
                step_id: null, schema_version: 1, created_at: 1_700_000_010,
                payload: null, step: null,
                terminal: {
                  id: 77, conversation_id: 1, model: "gpt-5.2", mode: "semi_autonomous",
                  status: "completed", started_at: 1_700_000_000, finished_at: 1_700_000_010,
                  total_steps: 1, final_content: "Done", error: null,
                  spent_micro_usd: 0, limit_micro_usd: null, interrupted: false,
                },
              }],
              next_cursor: 1,
              high_water_event_id: 1,
              has_more: false,
            };
          }
          return {
            events: [],
            next_cursor: 0,
            high_water_event_id: 0,
            has_more: false,
          };
        case "get_workspace_root":
          return "D:\\projects\\nx";
        case "roots_list":
          return { active: "D:\\projects\\nx", roots: ["D:\\projects\\nx"] };
        case "get_active_agent_run":
        case "get_compat_config":
          return null;
        case "get_access_profile":
          return "standard";
        case "conversation_context_stats":
          return {
            conversation_id: 1,
            estimated_tokens: 0,
            context_limit: 200_000,
            system_tokens: 0,
            message_tokens: 0,
            attachment_tokens: 0,
            tool_tokens: 0,
          };
        default:
          return null;
      }
    }
  );
  return { mockInvoke, mockListen, state };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mockListen }));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(async () => null),
  save: vi.fn(async () => null),
}));

const realScrollIntoView = Element.prototype.scrollIntoView;
beforeEach(() => {
  Element.prototype.scrollIntoView = vi.fn();
  window.localStorage.removeItem("nexora.locale");
  mockInvoke.mockClear();
  mockListen.mockClear();
  state.pendingTurn = null;
  state.pendingRevert = null;
  state.undoFixture = false;
  window.localStorage.setItem("nexora.onboarding.completed", "1");
  rememberSession(1);
  state.settings = new Map<string, string | null>([
    ["provider.selected", "openai"],
    ["provider.model", "gpt-5.2"],
  ]);
  state.capabilities = [
    {
      connection_id: "openai_compat",
      model_id: "llama-3.3-70b",
      context_window: 131_072,
      supported_efforts: [],
      supports_thinking: false,
      supports_attachments: false,
      supports_tools: true,
      provenance: "api_metadata",
      has_manual_override: false,
    },
    {
      connection_id: "openai",
      model_id: "gpt-5.2",
      context_window: 200_000,
      supported_efforts: ["low", "medium", "high", "xhigh"],
      supports_thinking: true,
      supports_attachments: true,
      supports_tools: true,
      provenance: "built_in_catalog",
      has_manual_override: false,
    },
    {
      connection_id: "openai",
      model_id: "gpt-5.6-discovered",
      context_window: 400_000,
      supported_efforts: ["low", "high"],
      supports_thinking: true,
      supports_attachments: true,
      supports_tools: true,
      provenance: "api_metadata",
      has_manual_override: false,
    },
  ];
});

afterEach(() => {
  cleanup();
  Element.prototype.scrollIntoView = realScrollIntoView;
});

function makeCap(
  connection_id: string,
  model_id: string,
  provenance: ConnectionModelCapabilities["provenance"] = "api_metadata"
): ConnectionModelCapabilities {
  return {
    connection_id,
    model_id,
    context_window: 128_000,
    supported_efforts: ["low", "high"],
    supports_thinking: true,
    supports_attachments: false,
    supports_tools: true,
    provenance,
    has_manual_override: false,
  };
}

describe("App — buildConnectionGroups & mergeConnectionModels", () => {
  it("merges openai_compat configured/discovered models and deduplicates against static models", () => {
    const providers: ProviderStatus[] = [
      {
        supported: {
          name: "openai",
          display_name: "OpenAI",
          models: ["gpt-5.2", "gpt-5.2-mini"],
        },
        configured: true,
        credentialed: true,
        available: true,
      },
      {
        supported: {
          name: "openai_compat",
          display_name: "OpenAI-Compatible",
          models: [],
        },
        configured: true,
        credentialed: true,
        available: true,
      },
      {
        supported: {
          name: "gemini",
          display_name: "Gemini",
          models: ["gemini-3.6-flash"],
        },
        configured: false,
        credentialed: false,
        available: false,
      },
    ];
    const capabilities: ConnectionModelCapabilities[] = [
      makeCap("openai", "gpt-5.2", "built_in_catalog"),
      makeCap("openai", "gpt-5.7-discovered", "api_metadata"),
      makeCap("openai_compat", "compat-configured-model", "unknown"),
      makeCap("openai_compat", "compat-discovered-v2", "api_metadata"),
      makeCap("openai_compat", "compat-configured-model", "api_metadata"),
      makeCap("gemini", "gemini-ignored-unavailable", "api_metadata"),
    ];

    const groups = buildConnectionGroups(providers, capabilities, "openai");
    expect(groups).toEqual([
      {
        connectionId: "openai",
        connectionName: "OpenAI",
        models: ["gpt-5.2", "gpt-5.2-mini", "gpt-5.7-discovered"],
      },
      {
        connectionId: "openai_compat",
        connectionName: "OpenAI-Compatible",
        models: ["compat-configured-model", "compat-discovered-v2"],
      },
    ]);
    expect(
      mergeConnectionModels(["a", "a", ""], "openai", [
        makeCap("openai", "a"),
        makeCap("openai", "b"),
        makeCap("anthropic", "c"),
      ])
    ).toEqual(["a", "b"]);
  });

  it("keeps the visible ChatGPT account catalog authoritative without merging capability-only slugs and passes display names", () => {
    const providers: ProviderStatus[] = [
      {
        supported: {
          name: "chatgpt_subscription",
          display_name: "ChatGPT subscription",
          models: ["gpt-5.4-visible"],
        },
        modelLabels: { "gpt-5.4-visible": "GPT-5.4" },
        configured: true,
        credentialed: true,
        available: true,
      },
    ];
    const capabilities: ConnectionModelCapabilities[] = [
      makeCap("chatgpt_subscription", "gpt-5.4-visible", "built_in_catalog"),
      makeCap("chatgpt_subscription", "stale-capability-only-slug", "manual_override"),
    ];

    expect(
      mergeConnectionModels(
        ["gpt-5.4-visible"],
        "chatgpt_subscription",
        capabilities,
        "extra-stale-slug"
      )
    ).toEqual(["gpt-5.4-visible"]);

    const groups = buildConnectionGroups(
      providers,
      capabilities,
      "chatgpt_subscription"
    );
    expect(groups).toEqual([
      {
        connectionId: "chatgpt_subscription",
        connectionName: "ChatGPT subscription",
        models: ["gpt-5.4-visible"],
        modelLabels: { "gpt-5.4-visible": "GPT-5.4" },
      },
    ]);
  });
});

describe("App — ModelPicker connectionGroups & selectConnectionModel integration", () => {
  beforeAll(() => {
    vi.stubGlobal(
      "ResizeObserver",
      class {
        observe(): void {}
        unobserve(): void {}
        disconnect(): void {}
      }
    );
  });

  afterAll(() => {
    vi.unstubAllGlobals();
  });

  it("surfaces openai_compat and discovered models in ModelPicker and resolves capabilities with the new connectionId", async () => {
    render(<App />);

    const pickerTrigger = await screen.findByRole("button", {
      name: "Agent mode and model",
    });
    expect(pickerTrigger).toHaveTextContent("gpt-5.2");
    fireEvent.click(pickerTrigger);

    await waitFor(() =>
      expect(
        screen.getByRole("option", { name: "llama-3.3-70b" })
      ).toBeInTheDocument()
    );
    expect(
      screen.getByRole("option", { name: "gpt-5.6-discovered" })
    ).toBeInTheDocument();
    expect(screen.getByText("OpenAI-Compatible")).toBeInTheDocument();

    mockInvoke.mockClear();
    fireEvent.click(screen.getByRole("option", { name: "claude-sonnet-4" }));

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("get_model_capabilities", {
        connectionId: "anthropic",
        modelId: "claude-sonnet-4",
      })
    );
    expect(mockInvoke).not.toHaveBeenCalledWith("get_model_capabilities", {
      connectionId: "openai",
      modelId: "claude-sonnet-4",
    });
  });
});

// Include the pointerdown observed by DockPanel before the subsequent click.
function clickWithPointer(target: HTMLElement) {
  fireEvent.pointerDown(target);
  target.focus();
  fireEvent.pointerUp(target);
  fireEvent.click(target);
}

describe("App — Stage5 PR5A shell integration", () => {
  beforeAll(() => {
    vi.stubGlobal(
      "ResizeObserver",
      class {
        observe() {}
        unobserve() {}
        disconnect() {}
      }
    );
  });
  afterAll(() => vi.unstubAllGlobals());

  it.each([
    { mode: "Agent", command: "start_agent_run", edit: false },
    { mode: "Chat", command: "send_message", edit: false },
    { mode: "Agent", command: "start_agent_run", edit: true },
    { mode: "Chat", command: "send_message", edit: true },
  ])("reconciles accepted $mode draft after unmount (concurrent edit: $edit)", async ({ mode, command, edit }) => {
    let accept!: () => void;
    state.pendingTurn = new Promise((resolve) => { accept = resolve; });
    render(<App />);
    const firstField = await screen.findByRole("textbox", { name: "Message" });
    if (mode === "Chat") {
      fireEvent.click(screen.getByRole("radio", { name: "Chat mode — one answer per message" }));
    }

    async function stageFile(field: HTMLElement, path: string, text: string) {
      fireEvent.change(field, { target: { value: `${text} @file` } });
      fireEvent.keyDown(field, { key: "Enter" });
      const dialog = await screen.findByRole("dialog", { name: "@file" });
      fireEvent.change(within(dialog).getByRole("textbox"), { target: { value: path } });
      fireEvent.click(within(dialog).getByRole("button", { name: "Preview" }));
      await within(dialog).findByText(`Preview of ${path}`);
      fireEvent.click(within(dialog).getByRole("button", { name: "Include in message" }));
    }

    await stageFile(firstField, "sent.txt", "Submitted");
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith(command,
      expect.objectContaining({ promptReferences: expect.objectContaining({ contexts: [
        { selector: { kind: "file", path: "sent.txt" }, revision: "sent.txt" },
      ] }) })));
    fireEvent.click(screen.getByRole("button", { name: "Sessions" }));
    fireEvent.click(await screen.findByText("Inactive session"));
    await waitFor(() => expect(firstField).not.toBeInTheDocument());

    const originalTab = screen.getByRole("tab", { name: "Test conversation" });
    if (edit) {
      fireEvent.click(originalTab);
      const nextField = await screen.findByRole("textbox", { name: "Message" });
      await stageFile(nextField, "new.txt", "Revised");
    }
    await act(async () => { accept(); });
    if (!edit) fireEvent.click(originalTab);
    expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue(edit ? "Revised " : "");
    expect(screen.queryByRole("button", { name: "Remove sent.txt from this message" })).toBeNull();
    if (edit) expect(screen.getByRole("button", { name: "Remove new.txt from this message" })).toBeInTheDocument();
  });

  it("keeps a pending review send with its tab across conversation remounts", async () => {
    let accept!: () => void;
    state.pendingTurn = new Promise((resolve) => { accept = resolve; });
    render(<App />);
    await screen.findByRole("textbox", { name: "Message" });
    fireEvent.click(screen.getByRole("button", { name: "Sessions" }));
    fireEvent.click(await screen.findByText("Inactive session"));
    fireEvent.click(screen.getByRole("tab", { name: "Test conversation" }));
    const originalField = await screen.findByRole("textbox", { name: "Message" });
    fireEvent.change(originalField, { target: { value: "/review" } });
    fireEvent.keyDown(originalField, { key: "Enter" });
    const changedLine = await screen.findByText("A changed line");
    fireEvent.click(within(changedLine.closest('[role="dialog"]') as HTMLElement)
      .getByRole("button", { name: "Send for review" }));

    fireEvent.keyDown(window, { key: "PageDown", ctrlKey: true });
    await waitFor(() => expect(originalField).not.toBeInTheDocument());
    fireEvent.keyDown(window, { key: "PageDown", ctrlKey: true });
    const returnedField = screen.getByRole("textbox", { name: "Message" });
    expect(returnedField).toHaveValue("/review");
    expect(returnedField).toBeDisabled();
    expect(screen.getByRole("button", { name: "Send message" })).toBeDisabled();
    expect(mockInvoke.mock.calls.filter(([command]) => command === "send_message")).toHaveLength(1);
    await act(async () => { accept(); });
    expect(returnedField).toBeEnabled();
    expect(returnedField).toHaveValue("");
  });

  it("preserves an edited tab draft when a pending /undo revert finishes after cycling tabs", async () => {
    let finishRevert!: () => void;
    state.undoFixture = true;
    state.pendingRevert = new Promise((resolve) => { finishRevert = resolve; });
    render(<App />);
    await screen.findByRole("textbox", { name: "Message" });
    fireEvent.click(screen.getByRole("button", { name: "Sessions" }));
    fireEvent.click(await screen.findByText("Inactive session"));
    fireEvent.click(screen.getByRole("tab", { name: "Test conversation" }));
    const originalField = await screen.findByRole("textbox", { name: "Message" });
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith(
      "list_conversation_run_events",
      expect.objectContaining({ conversationId: 1 })
    ));
    fireEvent.change(originalField, { target: { value: "/undo" } });
    fireEvent.keyDown(originalField, { key: "Enter" });
    const confirmDialog = await screen.findByRole("dialog", { name: "Revert the latest saved turn?" });
    fireEvent.click(within(confirmDialog).getByRole("button", { name: "Revert turn" }));
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith(
      "revert_turn_checkpoints",
      expect.objectContaining({ runId: 77, turnIndex: 0 })
    ));

    fireEvent.keyDown(window, { key: "PageDown", ctrlKey: true });
    await waitFor(() => expect(originalField).not.toBeInTheDocument());
    fireEvent.keyDown(window, { key: "PageDown", ctrlKey: true });
    const returnedField = await screen.findByRole("textbox", { name: "Message" });
    fireEvent.change(returnedField, { target: { value: "Edited draft while undo ran" } });
    await act(async () => { finishRevert(); });
    expect(returnedField).toHaveValue("Edited draft while undo ran");
  });

  it.each([
    [2, "Inactive session"],
    [3, "Archived session"],
  ] as const)(
    "exports exact Sessions row %i through the existing dialog and IPC route",
    async (id, title) => {
      render(<App />);
      await screen.findByRole("textbox", { name: "Message" });
      fireEvent.click(screen.getByRole("button", { name: "Sessions" }));
      const actions = await screen.findByRole("toolbar", {
        name: `Actions for ${title}`,
      });
      const exportButton = within(actions).getByRole("button", {
        name: "Export conversation",
      });
      exportButton.focus();
      mockInvoke.mockClear();
      clickWithPointer(exportButton);
      const dialog = await screen.findByRole("dialog", {
        name: "Export conversation",
      });
      expect(dialog).toHaveTextContent(`Saves “${title}”`);
      expect(
        screen.getByRole("tab", { name: "Test conversation" })
      ).toHaveAttribute("aria-selected", "true");
      expect(screen.getAllByRole("tab")).toHaveLength(1);
      vi.mocked(save).mockResolvedValueOnce("D:\\exports\\session.json");
      clickWithPointer(
        within(dialog).getByRole("button", { name: "Choose location" })
      );
      expect(exportButton.isConnected).toBe(true);
      await waitFor(() =>
        expect(mockInvoke).toHaveBeenCalledWith("export_conversation_to_file", {
          conversationId: id,
          path: "D:\\exports\\session.json",
          confirmationId: "export-confirmation",
        })
      );
      expect(mockInvoke).toHaveBeenCalledWith("request_confirmation", {
        scope: "export",
        operation: "export_conversation_to_file",
        cwd: null,
        targetId: id,
      });
      expect(
        mockInvoke.mock.calls.some(([cmd]) =>
          /^(create|rename|archive|restore|delete)_conversation$/.test(cmd)
        )
      ).toBe(false);
      expect(
        mockInvoke.mock.calls.some(([cmd]) => cmd === "conversation_history")
      ).toBe(false);
      clickWithPointer(within(dialog).getByRole("button", { name: "Done" }));
      await waitFor(() => expect(exportButton).toHaveFocus());
      expect(
        within(actions).getByRole("button", { name: "Export conversation" })
      ).toBe(exportButton);
    }
  );

  it.each([
    [2, "Inactive session"],
    [3, "Archived session"],
  ] as const)(
    "restores the exact row %i export invoker after pointer cancellation",
    async (_id, title) => {
      render(<App />);
      await screen.findByRole("textbox", { name: "Message" });
      clickWithPointer(screen.getByRole("button", { name: "Sessions" }));
      const actions = await screen.findByRole("toolbar", {
        name: `Actions for ${title}`,
      });
      const exportButton = within(actions).getByRole("button", {
        name: "Export conversation",
      });
      clickWithPointer(exportButton);
      const dialog = await screen.findByRole("dialog", {
        name: "Export conversation",
      });
      mockInvoke.mockClear();
      clickWithPointer(within(dialog).getByRole("button", { name: "Cancel" }));
      expect(exportButton.isConnected).toBe(true);
      await waitFor(() =>
        expect(
          screen.queryByRole("dialog", { name: "Export conversation" })
        ).toBeNull()
      );
      expect(exportButton).toHaveFocus();
      expect(
        within(actions).getByRole("button", { name: "Export conversation" })
      ).toBe(exportButton);
      expect(mockInvoke).not.toHaveBeenCalledWith(
        "export_conversation_to_file",
        expect.anything()
      );
      // Modal ownership is temporary: normal outside pointer dismissal resumes.
      const sessions = screen.getByRole("dialog", { name: "Sessions" });
      clickWithPointer(screen.getByRole("textbox", { name: "Message" }));
      expect(sessions.isConnected).toBe(false);
    }
  );

  it("keeps the same Split control after closing the last open session", async () => {
    render(<App />);
    await screen.findByRole("textbox", { name: "Message" });
    const split = screen.getByRole("button", { name: "Split" });
    fireEvent.click(
      screen.getByRole("button", { name: "Close Test conversation" })
    );
    expect(screen.getByRole("button", { name: "Split" })).toBe(split);
    expect(split).toBeDisabled();
    expect(screen.queryAllByRole("tab")).toHaveLength(0);
    expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull();
  });

  it("reaches the existing active export command after the palette closes", async () => {
    render(<App />);
    await screen.findByRole("textbox", { name: "Message" });
    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    const input = await screen.findByRole("combobox", {
      name: "Type a command",
    });
    fireEvent.change(input, {
      target: { value: "Export active conversation" },
    });
    expect(
      screen.getAllByRole("option", { name: "Export active conversation" })
    ).toHaveLength(1);
    fireEvent.click(
      screen.getByRole("option", { name: "Export active conversation" })
    );
    const dialog = await screen.findByRole("dialog", {
      name: "Export conversation",
    });
    expect(dialog).toHaveTextContent("Test conversation");
    expect(
      screen.queryByRole("combobox", { name: "Type a command" })
    ).toBeNull();
  });

  it("keeps separate drafts and archived pane identity through Alt+S and split close", async () => {
    render(<App />);
    const primary = await screen.findByRole("textbox", { name: "Message" });
    expect(screen.getByRole("button", { name: "Split" })).toBeDisabled();
    fireEvent.keyDown(window, { key: "s", altKey: true });
    expect(document.querySelector(".nex-split")).toBeNull();
    fireEvent.change(primary, { target: { value: "Primary draft" } });
    fireEvent.click(screen.getByRole("button", { name: "Sessions" }));
    fireEvent.click(
      await screen.findByRole("button", { name: /Archived session/ })
    );
    const archived = await screen.findByRole("textbox", { name: "Message" });
    expect(archived).toHaveValue("");
    fireEvent.change(archived, { target: { value: "Archived draft" } });
    const split = screen.getByRole("button", { name: "Split" });
    expect(split).toBeEnabled();
    fireEvent.keyDown(window, { key: "s", altKey: true });
    const messages = screen.getAllByRole("textbox", { name: "Message" });
    expect(
      messages.map((field) => (field as HTMLTextAreaElement).value)
    ).toEqual(["Archived draft", "Primary draft"]);
    expect(document.querySelectorAll(".nex-pane-title-badge")).toHaveLength(2);
    expect(
      screen.getByRole("region", { name: "Archived session (archived)" })
    ).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "Close split" })).toHaveLength(
      1
    );
    fireEvent.click(screen.getByRole("button", { name: "Close split" }));
    expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue(
      "Archived draft"
    );
    expect(document.querySelectorAll(".nex-pane-title-badge")).toHaveLength(0);
    expect(screen.getAllByRole("tab")).toHaveLength(2);
    fireEvent.click(screen.getByRole("tab", { name: "Test conversation" }));
    expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue(
      "Primary draft"
    );
  });
});
