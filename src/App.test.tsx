import {
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
import type { ConnectionModelCapabilities } from "./lib/tauri";
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
          return [];
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
      fireEvent.click(exportButton);
      const dialog = await screen.findByRole("dialog", {
        name: "Export conversation",
      });
      expect(dialog).toHaveTextContent(`Saves “${title}”`);
      expect(
        screen.getByRole("tab", { name: "Test conversation" })
      ).toHaveAttribute("aria-selected", "true");
      expect(screen.getAllByRole("tab")).toHaveLength(1);
      vi.mocked(save).mockResolvedValueOnce("D:\\exports\\session.json");
      fireEvent.click(
        within(dialog).getByRole("button", { name: "Choose location" })
      );
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
      fireEvent.click(within(dialog).getByRole("button", { name: "Done" }));
      await waitFor(() => expect(exportButton).toHaveFocus());
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
