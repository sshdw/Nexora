//! Panel pins for `SettingsView` (AC-211 minimum set, review 03 §6.3 item 2
//! — "writes ~12 setting keys incl. credential add/remove and autonomy
//! default; the value-domain rejection UX is unverified").
//!
//! The panel is rendered with its REAL shared stores (`useProviders`,
//! `useSpendLimit`, `useAutonomy`, `useAppearance`) so the pins exercise the
//! true data flow with invoke mocked at `@tauri-apps/api/core`: the mount
//! loads every power key once, the credential add path registers the
//! provider and stores the key via the wrappers and then discards the typed
//! secret from component state (a failed connect keeps it for correction),
//! the autonomy default persists through its key, the spend-limit field
//! rejects off-domain values locally (no IPC) and accepts integers, the
//! clear-all danger path reports a cancelled native confirmation honestly
//! and deletes nothing, and the in-page search jumps sections on Enter.

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useAppearance } from "../lib/useAppearance";
import { AUTONOMY_KEY, useAutonomy } from "../lib/useAutonomy";
import { LocaleProvider } from "../lib/useLocale";
import { useProviders } from "../lib/useProviders";
import { SPEND_LIMIT_KEY, useSpendLimit } from "../lib/useSpendLimit";
import SettingsView from "./SettingsView";
import {
  FLAG_DEFS,
  MCP_SERVERS_KEY,
  PRESET_KEY,
  RECENT_KEY,
  ROUTING_AGENT_KEY,
  ROUTING_CHAT_KEY,
} from "./settings/model";

const { mockInvoke, failNext, confirmFail, state } = vi.hoisted(() => {
  const failNext = { command: "" };
  const confirmFail = { active: false };
  const state = {
    supported: [] as { name: string; display_name: string; models: string[] }[],
    configured: [] as { id: number; name: string; display_name: string }[],
    credentialed: new Set<string>(),
    settings: new Map<string, string | null>(),
    projectSettings: new Map<string, string | null>(),
    blocked: [] as { key: string; reason: string; detail: string }[],
    configNotice: null as string | null,
    workspaceRootOverride: "/tmp/workspace" as string | null,
    capOverrides: new Map<
      string,
      {
        context_window?: number | null;
        supported_efforts?: string[] | null;
        supports_thinking?: boolean | null;
        supports_attachments?: boolean | null;
        supports_tools?: boolean | null;
      }
    >(),
    nextProviderId: 7,
  };
  const buildResolved = () => {
    const knownKeys = new Set<string>([
      "appearance.theme",
      "agent.access_profile",
      "agent.autonomy",
      "agent.spend_limit_micro_usd",
      "provider.selected",
      "provider.model",
      "provider.capability_overrides",
      ...state.settings.keys(),
      ...state.projectSettings.keys(),
      ...state.blocked.map((b) => b.key),
    ]);
    const entries: Record<
      string,
      {
        key: string;
        value: string | null;
        source: "session" | "project" | "personal" | "default";
        blocked_project_value: boolean;
        blocked_reason: string | null;
      }
    > = {};
    for (const key of knownKeys) {
      const blockedItem = state.blocked.find((b) => b.key === key);
      const hasProject = state.projectSettings.has(key) && !blockedItem;
      const hasPersonal = state.settings.has(key);
      const value = hasProject
        ? (state.projectSettings.get(key) ?? null)
        : hasPersonal
          ? (state.settings.get(key) ?? null)
          : null;
      const source = hasProject
        ? "project"
        : hasPersonal
          ? "personal"
          : "default";
      entries[key] = {
        key,
        value,
        source,
        blocked_project_value: Boolean(blockedItem),
        blocked_reason: blockedItem?.reason ?? null,
      };
    }
    return {
      workspace_root: state.workspaceRootOverride,
      has_project_config:
        state.projectSettings.size > 0 || state.blocked.length > 0,
      entries,
      blocked: [...state.blocked],
      config_notice: state.configNotice,
    };
  };
  const buildCap = (connectionId: string, modelId: string) => {
    const overrideSpec = state.capOverrides.get(`${connectionId}::${modelId}`);
    return {
      connection_id: connectionId,
      model_id: modelId,
      context_window: overrideSpec?.context_window ?? 128_000,
      supported_efforts: overrideSpec?.supported_efforts ?? [
        "low",
        "medium",
        "high",
      ],
      supports_thinking: overrideSpec?.supports_thinking ?? true,
      supports_attachments: overrideSpec?.supports_attachments ?? false,
      supports_tools: overrideSpec?.supports_tools ?? true,
      provenance: overrideSpec ? "manual_override" : "built_in_catalog",
      has_manual_override: Boolean(overrideSpec),
    };
  };
  const mockInvoke = vi.fn(
    async (command: string, args?: Record<string, unknown>) => {
      if (failNext.command === command) {
        failNext.command = "";
        throw { kind: "db", message: "backend refused" };
      }
      if (command === "request_confirmation" && confirmFail.active) {
        // The user cancelled at the blocking native OS dialog.
        throw {
          kind: "confirmationRequired",
          message: "cancelled at the native prompt",
        };
      }
      switch (command) {
        case "supported_providers":
          return JSON.parse(JSON.stringify(state.supported));
        case "list_providers":
          return JSON.parse(JSON.stringify(state.configured));
        case "get_setting":
          return state.settings.get((args as { key: string }).key) ?? null;
        case "set_setting": {
          const { key, value } = args as { key: string; value: string | null };
          if (value === null) state.settings.delete(key);
          else state.settings.set(key, value);
          return null;
        }
        case "delete_setting":
          state.settings.delete((args as { key: string }).key);
          return null;
        case "has_provider_credential":
          return state.credentialed.has(
            (args as { provider: string }).provider
          );
        case "create_provider":
          state.configured.push({
            id: state.nextProviderId,
            name: (args as { name: string }).name,
            display_name: (args as { displayName: string }).displayName,
          });
          state.nextProviderId += 1;
          return state.nextProviderId - 1;
        case "add_provider_credential":
        case "update_provider_credential":
          state.credentialed.add((args as { provider: string }).provider);
          return null;
        case "remove_provider_credential":
          state.credentialed.delete((args as { provider: string }).provider);
          return null;
        case "remove_provider":
          state.configured = state.configured.filter(
            (provider) => provider.id !== (args as { id: number }).id
          );
          return null;
        case "request_confirmation":
          return "confirm-id-1";
        case "clear_application_data":
          return null;
        case "get_compat_config":
          return {
            base_url: state.settings.get("openai_compat.base_url") ?? "",
            model: state.settings.get("openai_compat.model") ?? "",
            organization: state.settings.get("openai_compat.organization") ?? null,
            headers: [],
            supports_tools: true,
          };
        case "set_compat_config": {
          const cfg = (args as {
            config: { base_url: string; model: string; organization: string | null };
          }).config;
          state.settings.set("openai_compat.base_url", cfg.base_url);
          state.settings.set("openai_compat.model", cfg.model);
          if (cfg.organization) {
            state.settings.set("openai_compat.organization", cfg.organization);
          }
          return null;
        }
        case "compat_status": {
          const baseUrl = state.settings.get("openai_compat.base_url") ?? "";
          const model = state.settings.get("openai_compat.model") ?? "";
          return {
            has_base_url: baseUrl.length > 0,
            base_url_valid: baseUrl.startsWith("https://"),
            has_model: model.length > 0,
            has_organization: false,
            header_count: 0,
            supports_tools: true,
            has_credential: state.credentialed.has("openai_compat"),
            ready:
              baseUrl.startsWith("https://") &&
              model.length > 0 &&
              state.credentialed.has("openai_compat"),
          };
        }
        case "list_model_capabilities":
          return Array.from(state.capOverrides.keys()).map((key) => {
            const [conn, model] = key.split("::");
            return buildCap(conn, model);
          });
        case "get_model_capabilities": {
          const { connectionId, modelId } = args as {
            connectionId: string;
            modelId: string;
          };
          return buildCap(connectionId, modelId);
        }
        case "set_model_capability_override": {
          const { connectionId, modelId, overrideSpec, scope } = args as {
            connectionId: string;
            modelId: string;
            overrideSpec: {
              context_window?: number | null;
              supported_efforts?: string[] | null;
              supports_thinking?: boolean | null;
              supports_attachments?: boolean | null;
              supports_tools?: boolean | null;
            } | null;
            scope?: "personal" | "project" | null;
          };
          if (scope === "project" && !state.workspaceRootOverride) {
            throw { kind: "validation", message: "no workspace folder is open" };
          }
          const capKey = `${connectionId}::${modelId}`;
          if (overrideSpec === null) {
            state.capOverrides.delete(capKey);
            if (scope === "project") {
              state.projectSettings.delete("provider.capability_overrides");
            }
          } else {
            state.capOverrides.set(capKey, overrideSpec);
            const target =
              scope === "project" ? state.projectSettings : state.settings;
            target.set(
              "provider.capability_overrides",
              JSON.stringify(overrideSpec)
            );
          }
          return buildCap(connectionId, modelId);
        }
        case "clear_model_capability_override": {
          const { connectionId, modelId, scope } = args as {
            connectionId: string;
            modelId: string;
            scope?: "personal" | "project" | null;
          };
          const capKey = `${connectionId}::${modelId}`;
          state.capOverrides.delete(capKey);
          if (scope === "project") {
            state.projectSettings.delete("provider.capability_overrides");
          } else {
            state.settings.delete("provider.capability_overrides");
          }
          return buildCap(connectionId, modelId);
        }
        case "get_resolved_settings":
          return buildResolved();
        case "save_scoped_setting": {
          const { scope, key, value } = args as {
            scope: "personal" | "project";
            key: string;
            value: string | null;
          };
          if (scope === "project" && !state.workspaceRootOverride) {
            throw {
              kind: "validation",
              message: "no workspace folder is open",
            };
          }
          if (
            scope === "project" &&
            key === "agent.access_profile" &&
            value === "standard"
          ) {
            throw {
              kind: "permissionDenied",
              message:
                "project config cannot elevate access profile above personal ceiling",
            };
          }
          const target =
            scope === "project" ? state.projectSettings : state.settings;
          if (value === null) target.delete(key);
          else target.set(key, value);
          return buildResolved();
        }
        case "refresh_provider_model_metadata":
          return [];
        default:
          throw new Error(`unexpected command ${command}`);
      }
    }
  );
  return { mockInvoke, failNext, confirmFail, state, buildResolved, buildCap };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

beforeEach(() => {
  mockInvoke.mockClear();
  failNext.command = "";
  confirmFail.active = false;
  state.supported = [
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
  ];
  state.configured = [];
  state.credentialed = new Set<string>();
  state.settings = new Map<string, string | null>();
  state.projectSettings = new Map<string, string | null>();
  state.blocked = [];
  state.configNotice = null;
  state.workspaceRootOverride = "/tmp/workspace";
  state.capOverrides = new Map();
  state.nextProviderId = 7;
});

afterEach(() => {
  cleanup();
});

function Harness({
  workspaceRoot = "/tmp/workspace",
}: {
  workspaceRoot?: string | null;
}) {
  const store = useProviders();
  const spendLimit = useSpendLimit();
  const autonomy = useAutonomy();
  const appearance = useAppearance();
  return (
    <SettingsView
      onClose={() => {}}
      store={store}
      appearance={appearance}
      locale="en"
      onLocaleChange={() => {}}
      workspaceRoot={workspaceRoot}
      workspaceLoading={false}
      spendLimit={spendLimit}
      autonomy={autonomy}
      onDataCleared={() => {}}
    />
  );
}

function renderSettings(props?: { workspaceRoot?: string | null }) {
  return render(
    <LocaleProvider>
      <Harness workspaceRoot={props?.workspaceRoot} />
    </LocaleProvider>
  );
}

const railButton = (label: string) =>
  screen.getByRole("button", { name: label });

async function openAgentSection() {
  renderSettings();
  await waitFor(() =>
    expect(railButton("Agent & budgets")).toBeInTheDocument()
  );
  fireEvent.click(railButton("Agent & budgets"));
  await waitFor(() =>
    expect(
      screen.getByLabelText("Run cost limit (micro-USD)")
    ).toBeInTheDocument()
  );
}

describe("SettingsView — mount load", () => {
  it("loads every power key once on mount", async () => {
    renderSettings();
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("get_setting", {
        key: RECENT_KEY,
      })
    );
    for (const key of [
      RECENT_KEY,
      ROUTING_CHAT_KEY,
      ROUTING_AGENT_KEY,
      MCP_SERVERS_KEY,
      PRESET_KEY,
      ...FLAG_DEFS.map((flag) => `flags.${flag.name}`),
    ]) {
      expect(mockInvoke).toHaveBeenCalledWith("get_setting", { key });
    }
  });
});

describe("SettingsView — credential add/remove", () => {
  it("registers the provider, stores the key, and discards the typed secret", async () => {
    renderSettings();
    await waitFor(() => expect(railButton("Credentials")).toBeInTheDocument());
    fireEvent.click(railButton("Credentials"));
    const keyInput = screen.getByLabelText("OpenAI API key");
    fireEvent.change(keyInput, { target: { value: "sk-openai-test" } });
    // Both provider rows offer Connect; press the one in the OpenAI row.
    fireEvent.click(
      within(keyInput.closest("li") as HTMLElement).getByRole("button", {
        name: "Connect",
      })
    );

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("create_provider", {
        name: "openai",
        displayName: "OpenAI",
      })
    );
    expect(mockInvoke).toHaveBeenCalledWith("add_provider_credential", {
      provider: "openai",
      credential: "sk-openai-test",
    });
    // Success clears the draft: the secret never lingers in component state.
    await waitFor(() => expect(keyInput).toHaveValue(""));
    // The store reloaded, so the provider now reads as usable.
    await waitFor(() =>
      expect(screen.getAllByText("Connected").length).toBeGreaterThan(0)
    );
  });

  it("keeps the typed key for correction when the connect fails", async () => {
    renderSettings();
    await waitFor(() => expect(railButton("Credentials")).toBeInTheDocument());
    fireEvent.click(railButton("Credentials"));
    const keyInput = screen.getByLabelText("OpenAI API key");
    fireEvent.change(keyInput, { target: { value: "sk-openai-test" } });
    failNext.command = "add_provider_credential";
    fireEvent.click(
      within(keyInput.closest("li") as HTMLElement).getByRole("button", {
        name: "Connect",
      })
    );

    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent("backend refused")
    );
    expect(keyInput).toHaveValue("sk-openai-test");
  });
});

describe("SettingsView — agent defaults", () => {
  it("persists the autonomy default through its setting key", async () => {
    await openAgentSection();

    // The unpersisted default renders as the checked semi-autonomous option.
    const semi = screen.getByRole("radio", { name: "Semi-autonomous" });
    expect(semi).toHaveAttribute("aria-checked", "true");

    fireEvent.click(
      within(
        screen.getByRole("radiogroup", { name: "Autonomy mode" })
      ).getByRole("radio", { name: "Supervised" })
    );
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("set_setting", {
        key: AUTONOMY_KEY,
        value: "supervised",
      })
    );
  });

  it("rejects an off-domain spend limit locally without any IPC", async () => {
    await openAgentSection();

    const spend = screen.getByLabelText("Run cost limit (micro-USD)");
    fireEvent.change(spend, { target: { value: "abc" } });
    fireEvent.blur(spend);

    await waitFor(() =>
      expect(
        screen.getByText("The spend limit must be a non-negative integer.")
      ).toBeInTheDocument()
    );
    expect(
      mockInvoke.mock.calls.some(
        ([command, args]) =>
          command === "set_setting" &&
          (args as { key: string }).key === SPEND_LIMIT_KEY
      )
    ).toBe(false);
    expect(
      mockInvoke.mock.calls.some(
        ([command, args]) =>
          command === "delete_setting" &&
          (args as { key: string }).key === SPEND_LIMIT_KEY
      )
    ).toBe(false);
    // The rejected draft stays on screen for correction.
    expect(spend).toHaveValue("abc");
  });

  it("persists an integer spend limit under the spend key", async () => {
    await openAgentSection();

    const spend = screen.getByLabelText("Run cost limit (micro-USD)");
    fireEvent.change(spend, { target: { value: "2500" } });
    fireEvent.keyDown(spend, { key: "Enter" });

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("set_setting", {
        key: SPEND_LIMIT_KEY,
        value: "2500",
      })
    );
    // The draft clears and the field now mirrors the persisted value.
    await waitFor(() => expect(spend).toHaveValue("2500"));
  });
});

describe("SettingsView — clear-all data", () => {
  it("reports a cancelled native confirmation and deletes nothing", async () => {
    renderSettings();
    await waitFor(() =>
      expect(railButton("Data management")).toBeInTheDocument()
    );

    fireEvent.click(railButton("Data management"));
    confirmFail.active = true;
    fireEvent.click(screen.getByRole("button", { name: "Clear all data…" }));

    await waitFor(() =>
      expect(
        screen.getByText(
          "Cancelled at the system confirmation prompt — no data was deleted."
        )
      ).toBeInTheDocument()
    );
    expect(
      mockInvoke.mock.calls.some(
        ([command]) => command === "clear_application_data"
      )
    ).toBe(false);
  });
});

describe("SettingsView — in-page search", () => {
  it("jumps to the best-matching section on Enter", async () => {
    renderSettings();
    await waitFor(() =>
      expect(railButton("Agent & budgets")).toBeInTheDocument()
    );

    const search = screen.getByLabelText("Search settings");
    fireEvent.change(search, { target: { value: "spend" } });
    expect(within(screen.getByRole("search")).getByRole("status")).toBeInTheDocument();
    fireEvent.keyDown(search, { key: "Enter" });

    await waitFor(() =>
      expect(
        screen.getByRole("heading", { name: "Agent & budgets" })
      ).toBeInTheDocument()
    );
  });
});

describe("SettingsView — 16 provider presets & openai_compat endpoint validation", () => {
  it("lists all 16 connections and enforces strict HTTPS endpoint validation for openai_compat", async () => {
    state.supported = [
      { name: "openai", display_name: "OpenAI", models: ["gpt-5.6-terra"] },
      { name: "anthropic", display_name: "Anthropic", models: ["claude-sonnet-5"] },
      { name: "gemini", display_name: "Gemini", models: ["gemini-3.6-flash"] },
      { name: "xkiro", display_name: "xKiro", models: ["deepseek/deepseek-v4-flash"] },
      { name: "openrouter", display_name: "OpenRouter", models: ["inclusionai/ling-3.0-flash-fin:free"] },
      { name: "nvidia", display_name: "NVIDIA NIM", models: ["nvidia/nemotron-3-super-120b-a12b"] },
      { name: "opencode_zen", display_name: "OpenCode Zen", models: ["ling-3.0-flash-fin-free"] },
      { name: "deepseek", display_name: "DeepSeek", models: ["deepseek-chat", "deepseek-reasoner"] },
      { name: "qwen", display_name: "Alibaba / Qwen", models: ["qwen-max"] },
      { name: "zai", display_name: "ZAI", models: ["glm-4.5"] },
      { name: "minimax", display_name: "MiniMax", models: ["MiniMax-M1"] },
      { name: "moonshot", display_name: "Moonshot / Kimi", models: ["kimi-k2-0711-preview"] },
      { name: "groq", display_name: "Groq", models: ["llama-3.3-70b-versatile"] },
      { name: "xai", display_name: "xAI", models: ["grok-3"] },
      { name: "mistral", display_name: "Mistral", models: ["mistral-large-latest"] },
      { name: "openai_compat", display_name: "OpenAI Compatible", models: [] },
    ];

    renderSettings();
    await waitFor(() =>
      expect(railButton("Provider & model")).toBeInTheDocument()
    );
    fireEvent.click(railButton("Provider & model"));
    await waitFor(() =>
      expect(screen.getByLabelText("Provider")).toBeInTheDocument()
    );

    const providerSelect = screen.getByLabelText("Provider") as HTMLSelectElement;
    // 1 placeholder + 16 connections = 17 options.
    expect(providerSelect.options.length).toBe(17);
    for (const label of [
      "DeepSeek",
      "Alibaba / Qwen",
      "ZAI",
      "MiniMax",
      "Moonshot / Kimi",
      "Groq",
      "xAI",
      "Mistral",
      "OpenAI Compatible",
    ]) {
      expect(
        Array.from(providerSelect.options).some((opt) =>
          opt.text.includes(label)
        )
      ).toBe(true);
    }

    fireEvent.change(providerSelect, { target: { value: "openai_compat" } });
    await waitFor(() =>
      expect(
        screen.getByText("Configure a valid URL, model ID, and API key.")
      ).toBeInTheDocument()
    );

    // Remote http:// is rejected inline before any IPC write.
    const urlInput = screen.getByLabelText("Endpoint URL") as HTMLInputElement;
    fireEvent.change(urlInput, {
      target: { value: "http://remote.example.com/v1/chat/completions" },
    });
    expect(urlInput.value).toBe("http://remote.example.com/v1/chat/completions");
    fireEvent.change(screen.getByLabelText("Default model ID"), {
      target: { value: "custom-model" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save endpoint" }));
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Remote endpoints must use https://"
    );
    expect(
      mockInvoke.mock.calls.some(([cmd]) => cmd === "set_compat_config")
    ).toBe(false);

    // Valid https:// saves through set_compat_config and unlocks model selector.
    fireEvent.change(screen.getByLabelText("Endpoint URL"), {
      target: { value: "https://proxy.example.com/v1/chat/completions" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save endpoint" }));

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("set_compat_config", {
        config: {
          base_url: "https://proxy.example.com/v1/chat/completions",
          model: "custom-model",
          organization: null,
          headers: [],
          supports_tools: true,
        },
      })
    );
  });
});

describe("SettingsView — Stage 4 provenance, scope & capability overrides", () => {
  it("renders source badges and blocked untrusted-repo diagnostics banner", async () => {
    state.settings.set("appearance.theme", "light");
    state.projectSettings.set("agent.spend_limit_micro_usd", "9000");
    state.blocked = [
      {
        key: "agent.access_profile",
        reason: "elevation_blocked",
        detail: "project config cannot elevate access profile above personal",
      },
    ];
    state.configNotice = "stripped raw credential field in .nexora/config.json";

    renderSettings();
    await waitFor(() =>
      expect(screen.getByText("Blocked untrusted project overrides")).toBeInTheDocument()
    );
    expect(
      screen.getByText("stripped raw credential field in .nexora/config.json")
    ).toBeInTheDocument();
    expect(screen.getByText("agent.access_profile")).toBeInTheDocument();

    // Theme came from Personal; check provenance badge in Appearance section.
    const appearanceSection = screen
      .getByRole("heading", { name: "Appearance" })
      .closest("section") as HTMLElement;
    expect(within(appearanceSection).getByText("Personal")).toBeInTheDocument();

    // Agent section shows Project for spend limit and Blocked for access profile.
    fireEvent.click(railButton("Agent & budgets"));
    await waitFor(() =>
      expect(
        screen.getByLabelText("Run cost limit (micro-USD)")
      ).toBeInTheDocument()
    );
    const agentSection = screen
      .getByRole("heading", { name: "Agent & budgets" })
      .closest("section") as HTMLElement;
    expect(within(agentSection).getByText("Project")).toBeInTheDocument();
    expect(within(agentSection).getByText("Blocked")).toBeInTheDocument();
  });

  it("enforces workspace guard on Project (.nexora) scope and routes scoped writes", async () => {
    state.workspaceRootOverride = null;
    renderSettings({ workspaceRoot: null });

    await waitFor(() =>
      expect(
        screen.getByText(
          "Open a project workspace folder to save Project (.nexora) overrides."
        )
      ).toBeInTheDocument()
    );
    const scopeGroup = screen.getByRole("radiogroup", { name: "Save to" });
    const projectRadio = within(scopeGroup).getByRole("radio", {
      name: "Project (.nexora)",
    });
    expect(projectRadio).toBeDisabled();
    fireEvent.click(projectRadio);
    expect(projectRadio).toHaveAttribute("aria-checked", "false");

    cleanup();

    // With an active workspace, switching to Project (.nexora) persists via save_scoped_setting.
    state.workspaceRootOverride = "/tmp/workspace";
    state.settings.set("agent.access_profile", "read_only");
    renderSettings({ workspaceRoot: "/tmp/workspace" });

    await waitFor(() =>
      expect(railButton("Agent & budgets")).toBeInTheDocument()
    );
    const activeScopeGroup = screen.getByRole("radiogroup", {
      name: "Save to",
    });
    const activeProjectRadio = within(activeScopeGroup).getByRole("radio", {
      name: "Project (.nexora)",
    });
    fireEvent.click(activeProjectRadio);
    expect(activeProjectRadio).toHaveAttribute("aria-checked", "true");

    fireEvent.click(railButton("Agent & budgets"));
    await waitFor(() =>
      expect(
        screen.getByRole("radiogroup", { name: "Autonomy mode" })
      ).toBeInTheDocument()
    );

    fireEvent.click(
      within(
        screen.getByRole("radiogroup", { name: "Autonomy mode" })
      ).getByRole("radio", { name: "Supervised" })
    );
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("save_scoped_setting", {
        scope: "project",
        key: AUTONOMY_KEY,
        value: "supervised",
        sessionOverrides: null,
      })
    );

    // Attempting to elevate access profile in Project scope surfaces the backend denial.
    fireEvent.click(
      within(
        screen.getByRole("radiogroup", { name: "Access profile" })
      ).getByRole("radio", { name: "Standard" })
    );
    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(
        "project config cannot elevate access profile"
      )
    );
  });

  it("saves and clears manual model capability overrides in active scope", async () => {
    state.configured = [{ id: 1, name: "openai", display_name: "OpenAI" }];
    state.credentialed = new Set(["openai"]);
    state.settings.set("provider.selected", "openai");
    state.settings.set("provider.model", "gpt-5.2");

    renderSettings({ workspaceRoot: "/tmp/workspace" });
    await waitFor(() =>
      expect(railButton("Provider & model")).toBeInTheDocument()
    );
    fireEvent.click(railButton("Provider & model"));
    await waitFor(() =>
      expect(
        screen.getByText("Model capabilities & overrides")
      ).toBeInTheDocument()
    );

    // Switch scope to Project (.nexora) before saving capability override.
    fireEvent.click(
      within(screen.getByRole("radiogroup", { name: "Save to" })).getByRole(
        "radio",
        { name: "Project (.nexora)" }
      )
    );

    fireEvent.click(screen.getByText("Model capabilities & overrides"));
    const ctxInput = await screen.findByLabelText("Context window (tokens)");
    fireEvent.change(ctxInput, { target: { value: "256000" } });
    fireEvent.click(screen.getByLabelText("xhigh"));
    fireEvent.click(screen.getByLabelText("Attachments"));
    fireEvent.click(screen.getByRole("button", { name: "Save override" }));

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("set_model_capability_override", {
        connectionId: "openai",
        modelId: "gpt-5.2",
        overrideSpec: {
          context_window: 256000,
          supported_efforts: ["low", "medium", "high", "xhigh"],
          supports_thinking: true,
          supports_tools: true,
          supports_attachments: true,
        },
        scope: "project",
      })
    );
    await waitFor(() =>
      expect(screen.getByText("Source: Manual")).toBeInTheDocument()
    );

    fireEvent.click(screen.getByRole("button", { name: "Reset to detected" }));
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith(
        "clear_model_capability_override",
        {
          connectionId: "openai",
          modelId: "gpt-5.2",
          scope: "project",
        }
      )
    );
    await waitFor(() =>
      expect(screen.getByText("Source: Catalog")).toBeInTheDocument()
    );
  });
});
