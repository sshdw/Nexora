import { cleanup, fireEvent, render, screen } from "@testing-library/react";
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
          name: "chatgpt_subscription",
          display_name: "ChatGPT subscription",
          models: ["gpt-6.2-sol"],
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
      problem: null,
      models: [],
    },
    chatgptOAuthWorking: false,
    startChatGptOAuth: vi.fn(async () => true),
    refreshChatGptOAuth: vi.fn(async () => true),
    disconnectChatGptOAuth: vi.fn(async () => {}),
    ...overrides,
  };
}

describe("CredentialsSection & ProviderSection — ChatGPT subscription", () => {
  it("gates ChatGPT OAuth sign-in behind explicit experimental/ToS acknowledgment and gives the subscription no API-key input", () => {
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

    expect(screen.getByLabelText("OpenAI API key")).toBeTruthy();
    expect(screen.queryByLabelText("ChatGPT subscription API key")).toBeNull();

    // Auto-detects external CLI adapters on mount.

    // Experimental/ToS notice is shown and sign-in requires opt-in acknowledgment.
    expect(screen.getByText(/Opt-in experimental feature/i)).toBeTruthy();
    expect(
      screen.getByRole("region", {
        name: "ChatGPT Plus / Pro OAuth (Experimental)",
      })
    ).toBeTruthy();
    const signInBtn = screen.getByRole("button", {
      name: "Sign in with ChatGPT",
    }) as HTMLButtonElement;
    expect(signInBtn.className).toContain("nex-siwc-btn");
    expect(signInBtn.querySelector("svg")).not.toBeNull();
    expect(signInBtn.disabled).toBe(true);

    fireEvent.click(
      screen.getByLabelText(/I understand this is experimental/i)
    );
    expect(signInBtn.disabled).toBe(false);

    fireEvent.click(signInBtn);
    expect(store.startChatGptOAuth).toHaveBeenCalledTimes(1);
  });

  it("announces Waiting for browser busy state with role=status", () => {
    const store = makeStore({ chatgptOAuthWorking: true });
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

    const statusNode = screen.getByRole("status");
    expect(statusNode.textContent).toBe("Waiting for browser\u2026");
  });

  it("renders connected ChatGPT OAuth controls independently of API keys", () => {
    const store = makeStore({
      chatgptOAuth: {
        connected: true,
        has_refresh_token: true,
        expires_at: 1_900_000_000,
        connected_at: 1_899_996_400,
        auth_mode: "oauth",
        problem: null,
        models: [],
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

    expect(screen.queryByRole("button", { name: "Disconnect" })).toBeNull();

    expect(screen.getByText("OAuth connected")).toBeTruthy();
    expect(
      screen.getByRole("link", { name: "Manage usage in ChatGPT" })
    ).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Refresh token" }));
    expect(store.refreshChatGptOAuth).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("button", { name: "Disconnect OAuth" }));
    expect(store.disconnectChatGptOAuth).toHaveBeenCalledTimes(1);
  });

  it("offers direct reauthorization with the preserved registration when reauthorization is required", () => {
    const base = makeStore();
    const store = makeStore({
      chatgptOAuth: {
        ...base.chatgptOAuth!,
        connected: false,
        problem: "reauthorization_required",
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
    expect(screen.getByText("Reauthorization required")).toBeTruthy();
    const signInBtn = screen.getByRole("button", {
      name: "Sign in with ChatGPT",
    }) as HTMLButtonElement;
    expect(signInBtn.disabled).toBe(false);
    fireEvent.click(signInBtn);
    expect(store.startChatGptOAuth).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: "Disconnect OAuth" }));
    expect(store.disconnectChatGptOAuth).toHaveBeenCalledTimes(1);
  });

  it("keeps a visible cleanup action when stored credentials cannot be used", () => {
    const base = makeStore();
    const store = makeStore({
      chatgptOAuth: { ...base.chatgptOAuth!, problem: "cleanup_required" },
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
    expect(
      screen.queryByRole("button", { name: "Sign in with ChatGPT" })
    ).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Retry cleanup" }));
    expect(store.disconnectChatGptOAuth).toHaveBeenCalledTimes(1);
  });

  it("can recheck a session in use without offering destructive cleanup", () => {
    const base = makeStore();
    const store = makeStore({
      chatgptOAuth: { ...base.chatgptOAuth!, problem: "other_instance" },
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
    expect(
      screen.queryByRole("button", { name: "Sign in with ChatGPT" })
    ).toBeNull();
    expect(screen.queryByRole("button", { name: "Retry cleanup" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Check again" }));
    expect(store.reload).toHaveBeenCalledTimes(1);
  });

  it("identifies plan usage when the ChatGPT subscription provider is selected", () => {
    const store = makeStore({
      chatgptOAuth: {
        connected: true,
        has_refresh_token: true,
        expires_at: 1_900_000_000,
        connected_at: 1_899_996_400,
        auth_mode: "oauth",
        problem: null,
        models: [],
      },
    });

    render(
      <LocaleProvider>
        <ProviderSection
          store={store}
          selectedDefinition={store.providers[1]!.supported}
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

    expect(screen.getByText(/use your ChatGPT plan allowance/)).toBeTruthy();
  });
});
