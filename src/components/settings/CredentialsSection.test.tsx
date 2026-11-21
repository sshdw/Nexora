import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { LocaleProvider } from "../../lib/useLocale";
import type { ProvidersStore } from "../../lib/useProviders";
import CredentialsSection from "./CredentialsSection";
import ProviderSection from "./ProviderSection";

afterEach(cleanup);

function makeStore(overrides: Partial<ProvidersStore> = {}): ProvidersStore {
  return {
    providers: [
      {
        supported: {
          name: "openai",
          display_name: "OpenAI",
          models: ["gpt-5.6-terra"],
        },
        configured: false,
        credentialed: false,
        available: false,
      },
      {
        supported: {
          name: "cli_codex",
          display_name: "Codex CLI",
          models: ["codex-cli-default"],
        },
        configured: false,
        credentialed: false,
        available: false,
      },
      {
        supported: {
          name: "cli_claude",
          display_name: "Claude CLI",
          models: ["claude-cli-default"],
        },
        configured: false,
        credentialed: false,
        available: false,
      },
    ],
    selectedProvider: "openai",
    selectedModel: "gpt-5.6-terra",
    selectedEffort: null,
    capabilities: [],
    selectedCapabilities: null,
    loading: false,
    error: null,
    working: false,
    reload: vi.fn(async () => {}),
    selectProvider: vi.fn(async () => {}),
    selectModel: vi.fn(async () => {}),
    selectProviderAndModel: vi.fn(async () => {}),
    selectEffort: vi.fn(async () => {}),
    setCapabilityOverride: vi.fn(async () => null),
    refreshModelMetadata: vi.fn(async () => {}),
    connect: vi.fn(async () => true),
    disconnect: vi.fn(async () => {}),
    chatgptOAuth: {
      connected: false,
      has_refresh_token: false,
      expires_at: null,
      connected_at: null,
      auth_mode: "none",
    },
    chatgptOAuthWorking: false,
    startChatGptOAuth: vi.fn(async () => true),
    refreshChatGptOAuth: vi.fn(async () => true),
    disconnectChatGptOAuth: vi.fn(async () => {}),
    cliAdapters: [
      {
        id: "codex",
        provider_name: "cli_codex",
        display_name: "Codex CLI",
        installed: true,
        version: "codex 0.50.0",
        enabled: false,
      },
      {
        id: "claude",
        provider_name: "cli_claude",
        display_name: "Claude CLI",
        installed: false,
        version: null,
        enabled: false,
      },
    ],
    cliAdaptersLoading: false,
    refreshCliAdapters: vi.fn(async () => {}),
    toggleCliAdapter: vi.fn(async () => true),
    ...overrides,
  };
}

describe("CredentialsSection & ProviderSection — S11-11 Opt-in OAuth & External CLI Adapters", () => {
  it("gates ChatGPT OAuth sign-in behind explicit experimental/ToS acknowledgment and excludes CLI adapters from API-key inputs", async () => {
    const store = makeStore();
    render(
      <LocaleProvider>
        <CredentialsSection
          store={store}
          draftKeys={{}}
          onDraftChange={vi.fn()}
          onConnect={vi.fn()}
          onDisconnect={vi.fn()}
        />
      </LocaleProvider>
    );

    // OpenAI API key input exists, but external CLI adapters never render API key inputs.
    expect(screen.getByLabelText("OpenAI API key")).toBeTruthy();
    expect(screen.queryByLabelText("Codex CLI API key")).toBeNull();
    expect(screen.queryByLabelText("Claude CLI API key")).toBeNull();

    // Auto-detects external CLI adapters on mount.
    await waitFor(() => {
      expect(store.refreshCliAdapters).toHaveBeenCalledTimes(1);
    });

    // Experimental/ToS notice is shown and sign-in requires opt-in acknowledgment.
    expect(screen.getByText(/Opt-in experimental feature/i)).toBeTruthy();
    const signInBtn = screen.getByRole("button", {
      name: "Sign in with ChatGPT (OAuth)",
    }) as HTMLButtonElement;
    expect(signInBtn.disabled).toBe(true);

    fireEvent.click(
      screen.getByLabelText(/I understand this is experimental/i)
    );
    expect(signInBtn.disabled).toBe(false);

    fireEvent.click(signInBtn);
    expect(store.startChatGptOAuth).toHaveBeenCalledTimes(1);
  });

  it("renders connected ChatGPT OAuth controls (refresh + disconnect) and CLI adapter enable/disable state", () => {
    const store = makeStore({
      chatgptOAuth: {
        connected: true,
        has_refresh_token: true,
        expires_at: 1_900_000_000,
        connected_at: 1_899_996_400,
        auth_mode: "oauth",
      },
    });
    render(
      <LocaleProvider>
        <CredentialsSection
          store={store}
          draftKeys={{}}
          onDraftChange={vi.fn()}
          onConnect={vi.fn()}
          onDisconnect={vi.fn()}
        />
      </LocaleProvider>
    );

    expect(screen.getByText("OAuth connected")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Refresh token" }));
    expect(store.refreshChatGptOAuth).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("button", { name: "Disconnect OAuth" }));
    expect(store.disconnectChatGptOAuth).toHaveBeenCalledTimes(1);

    // Installed codex adapter can be enabled; not-detected claude adapter cannot.
    expect(screen.getByText("codex 0.50.0")).toBeTruthy();
    const enableBtns = screen.getAllByRole("button", {
      name: "Enable",
    }) as HTMLButtonElement[];
    expect(enableBtns).toHaveLength(2);
    expect(enableBtns[0]!.disabled).toBe(false);
    expect(enableBtns[1]!.disabled).toBe(true);

    fireEvent.click(enableBtns[0]!);
    expect(store.toggleCliAdapter).toHaveBeenCalledWith("codex", true);
  });

  it("shows honest status notices in ProviderSection for active ChatGPT OAuth and external CLI adapters", () => {
    const store = makeStore({
      chatgptOAuth: {
        connected: true,
        has_refresh_token: true,
        expires_at: 1_900_000_000,
        connected_at: 1_899_996_400,
        auth_mode: "oauth",
      },
    });

    const { rerender } = render(
      <LocaleProvider>
        <ProviderSection
          store={store}
          selectedDefinition={store.providers[0]!.supported}
          selectedModels={["gpt-5.6-terra"]}
          effectiveModel="gpt-5.6-terra"
          customActive={false}
          customValue=""
          onProviderChange={vi.fn()}
          onModelSelect={vi.fn()}
          onCustomDraftChange={vi.fn()}
          onCustomCommit={vi.fn()}
        />
      </LocaleProvider>
    );

    expect(
      screen.getByText(/authenticated via opt-in ChatGPT Plus\/Pro OAuth/i)
    ).toBeTruthy();

    rerender(
      <LocaleProvider>
        <ProviderSection
          store={store}
          selectedDefinition={store.providers[1]!.supported}
          selectedModels={["codex-cli-default"]}
          effectiveModel="codex-cli-default"
          customActive={false}
          customValue=""
          onProviderChange={vi.fn()}
          onModelSelect={vi.fn()}
          onCustomDraftChange={vi.fn()}
          onCustomCommit={vi.fn()}
        />
      </LocaleProvider>
    );

    expect(
      screen.getByText(/officially installed external CLI in a clean isolated subprocess/i)
    ).toBeTruthy();
  });
});
