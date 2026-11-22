import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

import { LocaleProvider } from "../lib/useLocale";
import type { ProvidersStore } from "../lib/useProviders";
import type { WorkspaceStore } from "../lib/useWorkspace";
import OnboardingFlow from "./OnboardingFlow";

afterEach(cleanup);

function makeProviders(
  overrides: Partial<ProvidersStore> = {}
): ProvidersStore {
  return {
    providers: [
      {
        supported: {
          name: "openai",
          display_name: "OpenAI",
          models: ["gpt-5.4"],
        },
        configured: false,
        credentialed: false,
        available: false,
      },
      {
        supported: {
          name: "anthropic",
          display_name: "Anthropic",
          models: ["claude-sonnet-4-6"],
        },
        configured: false,
        credentialed: false,
        available: false,
      },
    ],
    selectedProvider: "openai",
    selectedModel: "gpt-5.4",
    selectedEffort: null,
    capabilities: [],
    selectedCapabilities: null,
    compatConfig: null,
    compatStatus: null,
    compatSaving: false,
    chatgptOAuth: null,
    chatgptOAuthWorking: false,
    chatgptOAuthError: null,
    clearChatGptOAuthError: vi.fn(),
    loading: false,
    error: null,
    working: false,
    reload: vi.fn(async () => {}),
    selectProvider: vi.fn(async () => {}),
    selectModel: vi.fn(async () => {}),
    selectProviderAndModel: vi.fn(async () => {}),
    selectEffort: vi.fn(async () => {}),
    setCapabilityOverride: vi.fn(async () => null),
    clearCapabilityOverride: vi.fn(async () => null),
    refreshModelMetadata: vi.fn(async () => {}),
    saveCompatConfig: vi.fn(async () => true),
    connect: vi.fn(async () => true),
    disconnect: vi.fn(async () => {}),
    ...overrides,
  };
}

function makeWorkspace(): WorkspaceStore {
  return {
    root: null,
    roots: [],
    recent: [],
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

describe("OnboardingFlow", () => {
  it("uses the custom monochrome Select and never leaks ChatGPT OAuth errors onto the Welcome modal", () => {
    const clearChatGptOAuthError = vi.fn();
    const providers = makeProviders({
      chatgptOAuthError: {
        kind: "providerError",
        message: "ChatGPT OAuth token response exceeded the safety limit",
      },
      clearChatGptOAuthError,
    });

    const { container } = render(
      <LocaleProvider>
        <OnboardingFlow
          providers={providers}
          workspace={makeWorkspace()}
          onClose={vi.fn()}
          onOpenSettings={vi.fn()}
        />
      </LocaleProvider>
    );

    // No native <select> element inside the Welcome modal.
    expect(container.querySelector("select")).toBeNull();

    // Scoped OAuth error must never appear on the Welcome modal, and opening clears it.
    expect(
      screen.queryByText(
        "ChatGPT OAuth token response exceeded the safety limit"
      )
    ).toBeNull();
    expect(clearChatGptOAuthError).toHaveBeenCalledTimes(1);

    // Custom monochrome Select combobox switches providers cleanly.
    const trigger = screen.getByLabelText("Provider");
    expect(trigger).toHaveAttribute("role", "combobox");
    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole("option", { name: "Anthropic" }));
    expect(trigger).toHaveTextContent("Anthropic");
  });
});
