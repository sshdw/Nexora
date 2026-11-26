//! Store tests for `useProviders` (AC-211 minimum set, review 03 §6.3 item 3
//! — one of the two core-flow hooks).
//!
//! Pins the documented contract: the load composes supported providers,
//! configured rows, credential presence (presence only — values never leave
//! the keyring), and the persisted selection, with stale or hand-edited
//! selection values falling back to no selection while a syntactically valid
//! custom model ID persists. Writes: `selectProvider` persists the provider
//! and drops a model the new provider cannot serve unless it is custom-ID
//! valid, `connect` registers the provider only when needed and picks
//! add vs update from credential presence, and `disconnect` clears the
//! selection it is removing before removing credential + provider row.
//! `isCustomModelId` mirrors the backend rule exactly.

import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  isCustomModelId,
  useProviders,
  validateCompatBaseUrl,
} from "./useProviders";

const { mockInvoke, failNext, state } = vi.hoisted(() => {
  const failNext = { command: "" };
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
    ],
    configured: [{ id: 7, name: "openai", display_name: "OpenAI" }],
    credentialed: new Set<string>(["openai"]),
    settings: new Map<string, string | null>([
      ["provider.selected", "openai"],
      ["provider.model", "gpt-5.2"],
    ]),
    capabilities: [] as {
      connection_id: string;
      model_id: string;
      context_window: number | null;
      max_output_tokens: number | null;
      supported_efforts: string[];
      supports_thinking: boolean;
      supports_attachments: boolean;
      supports_tools: boolean;
      provenance: string;
      has_manual_override: boolean;
    }[],
    chatgptOAuth: {
      connected: false,
      has_refresh_token: false,
      expires_at: null as number | null,
      connected_at: null as number | null,
      auth_mode: "none" as "oauth" | "none",
      problem: null as
        | "cleanup_required"
        | "other_instance"
        | "reauthorization_required"
        | null,
      models: [] as { slug: string; display_name: string }[],
    },
    refreshedByConnection: new Map<
      string,
      {
        connection_id: string;
        model_id: string;
        context_window: number | null;
        max_output_tokens: number | null;
        supported_efforts: string[];
        supports_thinking: boolean;
        supports_attachments: boolean;
        supports_tools: boolean;
        provenance: string;
        has_manual_override: boolean;
      }[]
    >(),
  };
  const mockInvoke = vi.fn(
    async (command: string, args?: Record<string, unknown>) => {
      if (failNext.command === command) {
        failNext.command = "";
        throw { kind: "db", message: "backend refused" };
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
        case "has_provider_credential":
          return state.credentialed.has(
            (args as { provider: string }).provider
          );
        case "create_provider":
          state.configured.push({
            id: state.configured.length + 7,
            name: (args as { name: string }).name,
            display_name: (args as { displayName: string }).displayName,
          });
          return state.configured.length + 6;
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
        case "chatgpt_oauth_status":
          return JSON.parse(JSON.stringify(state.chatgptOAuth));
        case "chatgpt_oauth_disconnect":
          state.chatgptOAuth = {
            connected: false,
            has_refresh_token: false,
            expires_at: null,
            connected_at: null,
            auth_mode: "none",
            problem: null,
            models: [],
          };
          return null;
        case "list_model_capabilities":
          return JSON.parse(JSON.stringify(state.capabilities));
        case "get_model_capabilities": {
          const { connectionId, modelId } = args as {
            connectionId: string;
            modelId: string;
          };
          return (
            state.capabilities.find(
              (c) => c.connection_id === connectionId && c.model_id === modelId
            ) ?? {
              connection_id: connectionId,
              model_id: modelId,
              context_window: 200_000,
              max_output_tokens: 64_000,
              supported_efforts: ["low", "medium", "high"],
              supports_thinking: true,
              supports_attachments: true,
              supports_tools: true,
              provenance: "built_in_catalog",
              has_manual_override: false,
            }
          );
        }
        case "refresh_provider_model_metadata": {
          const { connectionId } = args as { connectionId: string };
          return JSON.parse(
            JSON.stringify(state.refreshedByConnection.get(connectionId) ?? [])
          );
        }
        case "get_compat_config":
          return {
            base_url: state.settings.get("openai_compat.base_url") ?? "",
            model: state.settings.get("openai_compat.model") ?? "",
            organization:
              state.settings.get("openai_compat.organization") ?? null,
            headers: [],
            supports_tools:
              (state.settings.get("openai_compat.supports_tools") ?? "true") !==
              "false",
          };
        case "set_compat_config": {
          const cfg = (
            args as {
              config: {
                base_url: string;
                model: string;
                organization: string | null;
                supports_tools: boolean;
              };
            }
          ).config;
          state.settings.set("openai_compat.base_url", cfg.base_url);
          state.settings.set("openai_compat.model", cfg.model);
          if (cfg.organization) {
            state.settings.set("openai_compat.organization", cfg.organization);
          } else {
            state.settings.delete("openai_compat.organization");
          }
          state.settings.set(
            "openai_compat.supports_tools",
            String(cfg.supports_tools)
          );
          return null;
        }
        case "compat_status": {
          const baseUrl = state.settings.get("openai_compat.base_url") ?? "";
          const model = state.settings.get("openai_compat.model") ?? "";
          const baseValid = validateCompatBaseUrl(baseUrl) === null;
          const hasCred = state.credentialed.has("openai_compat");
          return {
            has_base_url: baseUrl.length > 0,
            base_url_valid: baseValid,
            has_model: model.length > 0,
            has_organization: Boolean(
              state.settings.get("openai_compat.organization")
            ),
            header_count: 0,
            supports_tools:
              (state.settings.get("openai_compat.supports_tools") ?? "true") !==
              "false",
            has_credential: hasCred,
            ready: baseValid && model.length > 0 && hasCred,
          };
        }
        default:
          throw new Error(`unexpected command ${command}`);
      }
    }
  );
  return { mockInvoke, failNext, state };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

beforeEach(() => {
  mockInvoke.mockClear();
  failNext.command = "";
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
  state.configured = [{ id: 7, name: "openai", display_name: "OpenAI" }];
  state.credentialed = new Set<string>(["openai"]);
  state.settings = new Map<string, string | null>([
    ["provider.selected", "openai"],
    ["provider.model", "gpt-5.2"],
  ]);
  state.capabilities = [];
  state.chatgptOAuth = {
    connected: false,
    has_refresh_token: false,
    expires_at: null,
    connected_at: null,
    auth_mode: "none",
    problem: null,
    models: [],
  };
  state.refreshedByConnection = new Map();
});

describe("useProviders — isCustomModelId (backend rule mirror)", () => {
  it("accepts listed-shape ids within 1..=200 chars", () => {
    expect(isCustomModelId("team/custom-model:v2+beta")).toBe(true);
    expect(isCustomModelId("a")).toBe(true);
    expect(isCustomModelId("a".repeat(200))).toBe(true);
  });

  it("rejects empty, oversized, path-traversal, and off-vocabulary ids", () => {
    expect(isCustomModelId("")).toBe(false);
    expect(isCustomModelId("a".repeat(201))).toBe(false);
    expect(isCustomModelId("bad..id")).toBe(false);
    expect(isCustomModelId("has space")).toBe(false);
  });
});

describe("useProviders — load", () => {
  it("composes supported, configured, credentialed, and the persisted selection", async () => {
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));

    expect(mockInvoke).toHaveBeenCalledWith("supported_providers");
    expect(mockInvoke).toHaveBeenCalledWith("list_providers");
    expect(mockInvoke).toHaveBeenCalledWith("get_setting", {
      key: "provider.selected",
    });
    expect(mockInvoke).toHaveBeenCalledWith("get_setting", {
      key: "provider.model",
    });
    expect(mockInvoke).toHaveBeenCalledWith("has_provider_credential", {
      provider: "openai",
    });
    expect(mockInvoke).toHaveBeenCalledWith("has_provider_credential", {
      provider: "anthropic",
    });

    expect(result.current.providers).toHaveLength(2);
    expect(result.current.providers[0]).toMatchObject({
      configured: true,
      credentialed: true,
      available: true,
    });
    expect(result.current.providers[1]).toMatchObject({
      configured: false,
      credentialed: false,
      available: false,
    });
    expect(result.current.selectedProvider).toBe("openai");
    expect(result.current.selectedModel).toBe("gpt-5.2");
    expect(result.current.error).toBeNull();
  });

  it("falls back to no selection when the persisted model is stale or hand-edited", async () => {
    state.settings.set("provider.model", "removed model");
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.selectedProvider).toBe("openai");
    expect(result.current.selectedModel).toBeNull();
  });

  it("keeps a syntactically valid custom model id that no shortlist lists", async () => {
    state.settings.set("provider.model", "team/custom-model:v2");
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.selectedModel).toBe("team/custom-model:v2");
  });

  it("falls back to an unknown provider name being ignored", async () => {
    state.settings.set("provider.selected", "not-a-provider");
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.selectedProvider).toBeNull();
  });

  it("surfaces a failed load and leaves the selection unset", async () => {
    failNext.command = "supported_providers";
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.error).toEqual({
      kind: "db",
      message: "backend refused",
    });
    expect(result.current.providers).toEqual([]);
    expect(result.current.selectedProvider).toBeNull();
  });
});

describe("useProviders — select", () => {
  it("persists the selected provider and keeps a custom-valid model", async () => {
    state.settings.set("provider.model", "team/custom-model:v2");
    const { result } = renderHook(() => useProviders());
    await waitFor(() =>
      expect(result.current.selectedModel).toBe("team/custom-model:v2")
    );

    await act(async () => {
      await result.current.selectProvider("anthropic");
    });
    expect(mockInvoke).toHaveBeenCalledWith("set_setting", {
      key: "provider.selected",
      value: "anthropic",
    });
    // The custom id is provider-independent: never dropped by a switch.
    expect(
      mockInvoke.mock.calls.some(
        ([command, args]) =>
          command === "set_setting" &&
          (args as { key: string }).key === "provider.model"
      )
    ).toBe(false);
    expect(result.current.selectedProvider).toBe("anthropic");
    expect(result.current.selectedModel).toBe("team/custom-model:v2");
  });

  it("drops a model the newly selected provider cannot serve", async () => {
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));
    // Force a non-custom-valid model into state (selectModel persists raw).
    await act(async () => {
      await result.current.selectModel("not a valid id");
    });
    expect(result.current.selectedModel).toBe("not a valid id");

    await act(async () => {
      await result.current.selectProvider("anthropic");
    });
    expect(mockInvoke).toHaveBeenCalledWith("set_setting", {
      key: "provider.model",
      value: null,
    });
    expect(result.current.selectedModel).toBeNull();
    expect(result.current.selectedProvider).toBe("anthropic");
  });

  it("persists an explicit model selection", async () => {
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.selectModel("gpt-5.2-mini");
    });
    expect(mockInvoke).toHaveBeenCalledWith("set_setting", {
      key: "provider.model",
      value: "gpt-5.2-mini",
    });
    expect(result.current.selectedModel).toBe("gpt-5.2-mini");
  });

  it("queries getModelCapabilities with the new connectionId in selectProviderAndModel", async () => {
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));
    mockInvoke.mockClear();

    await act(async () => {
      await result.current.selectProviderAndModel(
        "anthropic",
        "claude-sonnet-4"
      );
    });

    expect(mockInvoke).toHaveBeenCalledWith("set_setting", {
      key: "provider.selected",
      value: "anthropic",
    });
    expect(mockInvoke).toHaveBeenCalledWith("set_setting", {
      key: "provider.model",
      value: "claude-sonnet-4",
    });
    expect(mockInvoke).toHaveBeenCalledWith("get_model_capabilities", {
      connectionId: "anthropic",
      modelId: "claude-sonnet-4",
    });
    expect(mockInvoke).not.toHaveBeenCalledWith("get_model_capabilities", {
      connectionId: "openai",
      modelId: "claude-sonnet-4",
    });
    expect(result.current.selectedProvider).toBe("anthropic");
    expect(result.current.selectedModel).toBe("claude-sonnet-4");
    expect(result.current.selectedCapabilities).toMatchObject({
      connection_id: "anthropic",
      model_id: "claude-sonnet-4",
    });
  });

  it("uses explicit providerId on selectModel even when called right after selectProvider in the same closure", async () => {
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));
    mockInvoke.mockClear();

    const preRenderStore = result.current;
    await act(async () => {
      await preRenderStore.selectProvider("anthropic");
      await preRenderStore.selectModel("claude-sonnet-4", "anthropic");
    });

    expect(mockInvoke).toHaveBeenCalledWith("get_model_capabilities", {
      connectionId: "anthropic",
      modelId: "claude-sonnet-4",
    });
    expect(mockInvoke).not.toHaveBeenCalledWith("get_model_capabilities", {
      connectionId: "openai",
      modelId: "claude-sonnet-4",
    });
    expect(result.current.selectedProvider).toBe("anthropic");
    expect(result.current.selectedModel).toBe("claude-sonnet-4");
  });

  it("merges discovered models into capabilities on refreshModelMetadata", async () => {
    state.refreshedByConnection.set("openai_compat", [
      {
        connection_id: "openai_compat",
        model_id: "discovered-llama-3.3",
        context_window: 131_072,
        max_output_tokens: 32_768,
        supported_efforts: ["low", "high"],
        supports_thinking: true,
        supports_attachments: false,
        supports_tools: true,
        provenance: "api_metadata",
        has_manual_override: false,
      },
    ]);
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.refreshModelMetadata("openai_compat");
    });

    expect(mockInvoke).toHaveBeenCalledWith("refresh_provider_model_metadata", {
      connectionId: "openai_compat",
    });
    expect(
      result.current.capabilities.some(
        (c) =>
          c.connection_id === "openai_compat" &&
          c.model_id === "discovered-llama-3.3"
      )
    ).toBe(true);
  });
});

describe("useProviders — connect / disconnect (credential lifecycle)", () => {
  it("registers an unconfigured provider then adds its first credential", async () => {
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));

    let ok = false;
    await act(async () => {
      ok = await result.current.connect(
        "anthropic",
        "Anthropic",
        "sk-anthropic"
      );
    });
    expect(ok).toBe(true);
    expect(mockInvoke).toHaveBeenCalledWith("create_provider", {
      name: "anthropic",
      displayName: "Anthropic",
    });
    expect(mockInvoke).toHaveBeenCalledWith("add_provider_credential", {
      provider: "anthropic",
      credential: "sk-anthropic",
    });
    // The store reloads, so the new provider reads as usable.
    await waitFor(() =>
      expect(result.current.providers[1]).toMatchObject({ available: true })
    );
    expect(result.current.working).toBe(false);
  });

  it("updates in place when a credential already exists", async () => {
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));

    let ok = false;
    await act(async () => {
      ok = await result.current.connect("openai", "OpenAI", "sk-rotated");
    });
    expect(ok).toBe(true);
    // Already configured and credentialed: no create, no add.
    expect(
      mockInvoke.mock.calls.some(([command]) => command === "create_provider")
    ).toBe(false);
    expect(mockInvoke).toHaveBeenCalledWith("update_provider_credential", {
      provider: "openai",
      credential: "sk-rotated",
    });
    expect(
      mockInvoke.mock.calls.some(
        ([command]) => command === "add_provider_credential"
      )
    ).toBe(false);
  });

  it("keeps working=false and surfaces the error when the credential write fails", async () => {
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));
    failNext.command = "add_provider_credential";

    let ok = true;
    await act(async () => {
      ok = await result.current.connect(
        "anthropic",
        "Anthropic",
        "sk-anthropic"
      );
    });
    expect(ok).toBe(false);
    expect(result.current.error).toEqual({
      kind: "db",
      message: "backend refused",
    });
    expect(result.current.working).toBe(false);
  });

  it("disconnect clears the selection it removes, then credential and row", async () => {
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.disconnect("openai");
    });
    // Selection cleared first so it never points at a removed provider.
    expect(mockInvoke).toHaveBeenCalledWith("set_setting", {
      key: "provider.selected",
      value: null,
    });
    expect(mockInvoke).toHaveBeenCalledWith("set_setting", {
      key: "provider.model",
      value: null,
    });
    expect(mockInvoke).toHaveBeenCalledWith("remove_provider_credential", {
      provider: "openai",
    });
    expect(mockInvoke).toHaveBeenCalledWith("remove_provider", { id: 7 });
    expect(result.current.selectedProvider).toBeNull();
    await waitFor(() =>
      expect(result.current.providers[0]).toMatchObject({ available: false })
    );
  });

  it("disconnect of an unselected provider leaves the selection alone", async () => {
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.disconnect("anthropic");
    });
    expect(
      mockInvoke.mock.calls.some(
        ([command, args]) =>
          command === "set_setting" &&
          (args as { key: string }).key === "provider.selected"
      )
    ).toBe(false);
    expect(result.current.selectedProvider).toBe("openai");
  });

  it("disconnects ChatGPT without removing the OpenAI API key, row, or selection", async () => {
    state.chatgptOAuth = {
      connected: true,
      has_refresh_token: true,
      expires_at: 1_900_000_000,
      connected_at: 1_899_996_400,
      auth_mode: "oauth",
      problem: null,
      models: [],
    };
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));
    mockInvoke.mockClear();

    await act(async () => {
      await result.current.disconnectChatGptOAuth?.();
    });

    expect(mockInvoke).toHaveBeenCalledWith("chatgpt_oauth_disconnect");
    expect(
      mockInvoke.mock.calls.some(
        ([command]) => command === "remove_provider_credential"
      )
    ).toBe(false);
    expect(
      mockInvoke.mock.calls.some(([command]) => command === "remove_provider")
    ).toBe(false);
    expect(result.current.selectedProvider).toBe("openai");
    expect(result.current.providers[0]).toMatchObject({ available: true });
    expect(result.current.chatgptOAuth?.connected).toBe(false);
  });

  it("keeps cleanup available when connection status cannot be read", async () => {
    failNext.command = "chatgpt_oauth_status";
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.chatgptOAuth).toMatchObject({
      connected: false,
      problem: "cleanup_required" as const,
    });
    expect(result.current.providers[0]).toMatchObject({ available: true });
  });

  it("rejects stale persisted ChatGPT subscription slugs and exposes returned display names", async () => {
    state.supported.push({
      name: "chatgpt_subscription",
      display_name: "ChatGPT subscription",
      models: [],
    });
    state.configured.push({
      id: 99,
      name: "chatgpt_subscription",
      display_name: "ChatGPT subscription",
    });
    state.chatgptOAuth = {
      connected: true,
      has_refresh_token: true,
      expires_at: 1_900_000_000,
      connected_at: 1_899_996_400,
      auth_mode: "oauth",
      problem: null,
      models: [{ slug: "gpt-5.4-plan", display_name: "GPT-5.4" }],
    };
    state.settings.set("provider.selected", "chatgpt_subscription");
    state.settings.set("provider.model", "stale-retired-slug");

    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));

    expect(result.current.selectedProvider).toBe("chatgpt_subscription");
    // Stale persisted slug not in the live visible catalog must be rejected.
    expect(result.current.selectedModel).toBeNull();
    const planProvider = result.current.providers.find(
      (p) => p.supported.name === "chatgpt_subscription"
    );
    expect(planProvider?.supported.models).toEqual(["gpt-5.4-plan"]);
    expect(planProvider?.modelLabels).toEqual({ "gpt-5.4-plan": "GPT-5.4" });

    // Attempting to select a slug outside the visible catalog is rejected.
    mockInvoke.mockClear();
    await act(async () => {
      await result.current.selectModel("unlisted-capability-slug");
    });
    expect(result.current.selectedModel).toBeNull();
    expect(mockInvoke).not.toHaveBeenCalledWith("set_setting", {
      key: "provider.model",
      value: "unlisted-capability-slug",
    });

    // Selecting a slug present in the visible catalog succeeds.
    await act(async () => {
      await result.current.selectModel("gpt-5.4-plan");
    });
    expect(result.current.selectedModel).toBe("gpt-5.4-plan");
  });
});

describe("useProviders — OpenAI-compatible validation & config", () => {
  it("enforces strict HTTPS except explicit localhost and rejects embedded credentials", () => {
    expect(
      validateCompatBaseUrl("https://api.deepseek.com/v1/chat/completions")
    ).toBeNull();
    expect(
      validateCompatBaseUrl("http://localhost:11434/v1/chat/completions")
    ).toBeNull();
    expect(
      validateCompatBaseUrl("http://127.0.0.1:8080/v1/chat/completions")
    ).toBeNull();
    expect(
      validateCompatBaseUrl("http://[::1]:8080/v1/chat/completions")
    ).toBeNull();

    expect(validateCompatBaseUrl("")).toBe("settings.compatErrMissingUrl");
    expect(validateCompatBaseUrl("http://remote.example.com/v1")).toBe(
      "settings.compatErrInsecureHttp"
    );
    expect(
      validateCompatBaseUrl("https://user:pass@remote.example.com/v1")
    ).toBe("settings.compatErrCredentials");
    expect(validateCompatBaseUrl("ftp://remote.example.com/v1")).toBe(
      "settings.compatErrInvalidUrl"
    );
  });

  it("loads and saves openai_compat config, updates readiness, and selects its model", async () => {
    state.supported.push({
      name: "openai_compat",
      display_name: "OpenAI Compatible",
      models: [],
    });
    state.credentialed.add("openai_compat");
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.selectProvider("openai_compat");
    });

    await waitFor(() => expect(result.current.compatStatus?.ready).toBe(false));

    let saved = false;
    await act(async () => {
      saved =
        (await result.current.saveCompatConfig?.({
          base_url: "https://proxy.example.com/v1/chat/completions",
          model: "custom-deepseek-v4",
          organization: "org-1",
          headers: [],
          supports_tools: true,
        })) ?? false;
    });

    expect(saved).toBe(true);
    expect(result.current.compatConfig?.model).toBe("custom-deepseek-v4");
    expect(result.current.compatStatus?.ready).toBe(true);
    expect(result.current.selectedModel).toBe("custom-deepseek-v4");
  });
});
