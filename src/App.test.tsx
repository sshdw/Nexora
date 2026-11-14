import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
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
              (c) =>
                c.connection_id === connectionId && c.model_id === modelId
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

beforeEach(() => {
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
