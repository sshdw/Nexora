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

import { isCustomModelId, useProviders } from "./useProviders";

const { mockInvoke, failNext, state } = vi.hoisted(() => {
  const failNext = { command: "" };
  const state = {
    supported: [
      { name: "openai", display_name: "OpenAI", models: ["gpt-5.2", "gpt-5.2-mini"] },
      { name: "anthropic", display_name: "Anthropic", models: ["claude-sonnet-4"] },
    ],
    configured: [{ id: 7, name: "openai", display_name: "OpenAI" }],
    credentialed: new Set<string>(["openai"]),
    settings: new Map<string, string | null>([
      ["provider.selected", "openai"],
      ["provider.model", "gpt-5.2"],
    ]),
  };
  const mockInvoke = vi.fn(async (command: string, args?: Record<string, unknown>) => {
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
        return state.credentialed.has((args as { provider: string }).provider);
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
          (provider) => provider.id !== (args as { id: number }).id,
        );
        return null;
      default:
        throw new Error(`unexpected command ${command}`);
    }
  });
  return { mockInvoke, failNext, state };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

beforeEach(() => {
  mockInvoke.mockClear();
  failNext.command = "";
  state.supported = [
    { name: "openai", display_name: "OpenAI", models: ["gpt-5.2", "gpt-5.2-mini"] },
    { name: "anthropic", display_name: "Anthropic", models: ["claude-sonnet-4"] },
  ];
  state.configured = [{ id: 7, name: "openai", display_name: "OpenAI" }];
  state.credentialed = new Set<string>(["openai"]);
  state.settings = new Map<string, string | null>([
    ["provider.selected", "openai"],
    ["provider.model", "gpt-5.2"],
  ]);
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
    expect(mockInvoke).toHaveBeenCalledWith("get_setting", { key: "provider.selected" });
    expect(mockInvoke).toHaveBeenCalledWith("get_setting", { key: "provider.model" });
    expect(mockInvoke).toHaveBeenCalledWith("has_provider_credential", { provider: "openai" });
    expect(mockInvoke).toHaveBeenCalledWith("has_provider_credential", { provider: "anthropic" });

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
    expect(result.current.error).toEqual({ kind: "db", message: "backend refused" });
    expect(result.current.providers).toEqual([]);
    expect(result.current.selectedProvider).toBeNull();
  });
});

describe("useProviders — select", () => {
  it("persists the selected provider and keeps a custom-valid model", async () => {
    state.settings.set("provider.model", "team/custom-model:v2");
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.selectedModel).toBe("team/custom-model:v2"));

    await act(async () => {
      await result.current.selectProvider("anthropic");
    });
    expect(mockInvoke).toHaveBeenCalledWith("set_setting", {
      key: "provider.selected",
      value: "anthropic",
    });
    // The custom id is provider-independent: never dropped by a switch.
    expect(mockInvoke.mock.calls.some(([command, args]) =>
      command === "set_setting" && (args as { key: string }).key === "provider.model",
    )).toBe(false);
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
});

describe("useProviders — connect / disconnect (credential lifecycle)", () => {
  it("registers an unconfigured provider then adds its first credential", async () => {
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));

    let ok = false;
    await act(async () => {
      ok = await result.current.connect("anthropic", "Anthropic", "sk-anthropic");
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
      expect(result.current.providers[1]).toMatchObject({ available: true }),
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
    expect(mockInvoke.mock.calls.some(([command]) => command === "create_provider")).toBe(false);
    expect(mockInvoke).toHaveBeenCalledWith("update_provider_credential", {
      provider: "openai",
      credential: "sk-rotated",
    });
    expect(
      mockInvoke.mock.calls.some(([command]) => command === "add_provider_credential"),
    ).toBe(false);
  });

  it("keeps working=false and surfaces the error when the credential write fails", async () => {
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));
    failNext.command = "add_provider_credential";

    let ok = true;
    await act(async () => {
      ok = await result.current.connect("anthropic", "Anthropic", "sk-anthropic");
    });
    expect(ok).toBe(false);
    expect(result.current.error).toEqual({ kind: "db", message: "backend refused" });
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
    expect(mockInvoke).toHaveBeenCalledWith("remove_provider_credential", { provider: "openai" });
    expect(mockInvoke).toHaveBeenCalledWith("remove_provider", { id: 7 });
    expect(result.current.selectedProvider).toBeNull();
    await waitFor(() =>
      expect(result.current.providers[0]).toMatchObject({ available: false }),
    );
  });

  it("disconnect of an unselected provider leaves the selection alone", async () => {
    const { result } = renderHook(() => useProviders());
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.disconnect("anthropic");
    });
    expect(
      mockInvoke.mock.calls.some(([command, args]) =>
        command === "set_setting" && (args as { key: string }).key === "provider.selected",
      ),
    ).toBe(false);
    expect(result.current.selectedProvider).toBe("openai");
  });
});
