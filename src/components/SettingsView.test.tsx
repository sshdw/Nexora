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
    nextProviderId: 7,
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
        default:
          throw new Error(`unexpected command ${command}`);
      }
    }
  );
  return { mockInvoke, failNext, confirmFail, state };
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
  state.nextProviderId = 7;
});

afterEach(() => {
  cleanup();
});

function Harness() {
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
      workspaceRoot="/tmp/workspace"
      workspaceLoading={false}
      spendLimit={spendLimit}
      autonomy={autonomy}
      onDataCleared={() => {}}
    />
  );
}

function renderSettings() {
  return render(
    <LocaleProvider>
      <Harness />
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

    fireEvent.click(screen.getByRole("radio", { name: "Supervised" }));
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
    expect(screen.getByRole("status")).toBeInTheDocument();
    fireEvent.keyDown(search, { key: "Enter" });

    await waitFor(() =>
      expect(
        screen.getByRole("heading", { name: "Agent & budgets" })
      ).toBeInTheDocument()
    );
  });
});
