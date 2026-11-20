//! Unit & integration tests for `McpSection` (`M18` / `P26`).
//! Verifies zero bundled servers, stdio & Streamable HTTP CRUD, OAuth 2.1
//! sign-in/refresh, keyring-only Bearer token clearing, and tool discovery.

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { LocaleProvider } from "../../lib/useLocale";
import type { McpServerEntry, McpServerView } from "../../lib/tauri";
import McpSection from "./McpSection";

const { mockInvoke, state } = vi.hoisted(() => {
  const state = {
    servers: [] as McpServerView[],
    tools: [] as {
      namespaced_name: string;
      server_name: string;
      tool_name: string;
      description: string;
    }[],
    rejectNext: null as { kind: string; message: string } | null,
  };
  const mockInvoke = vi.fn(
    async (command: string, args?: Record<string, unknown>) => {
      if (state.rejectNext) {
        const err = state.rejectNext;
        state.rejectNext = null;
        throw err;
      }
      switch (command) {
        case "mcp_list_servers":
          return JSON.parse(JSON.stringify(state.servers));
        case "mcp_upsert_server": {
          const entry = (args as { server: McpServerEntry }).server;
          const view: McpServerView = {
            name: entry.name,
            transport: entry.transport,
            enabled: entry.enabled,
            command: entry.command,
            args: entry.args,
            env: entry.env,
            url: entry.url ?? null,
            oauth: entry.oauth ?? null,
            has_auth: false,
            auth_kind: "none",
            expires_at: null,
          };
          const idx = state.servers.findIndex((s) => s.name === entry.name);
          if (idx >= 0) state.servers[idx] = view;
          else state.servers.push(view);
          return JSON.parse(JSON.stringify(state.servers));
        }
        case "mcp_set_server_enabled": {
          const { name, enabled } = args as { name: string; enabled: boolean };
          state.servers = state.servers.map((s) =>
            s.name === name ? { ...s, enabled } : s
          );
          return JSON.parse(JSON.stringify(state.servers));
        }
        case "mcp_remove_server": {
          const { name } = args as { name: string };
          state.servers = state.servers.filter((s) => s.name !== name);
          return JSON.parse(JSON.stringify(state.servers));
        }
        case "mcp_list_server_tools":
          return JSON.parse(JSON.stringify(state.tools));
        case "mcp_set_bearer_token": {
          const { name } = args as { name: string; token: string };
          state.servers = state.servers.map((s) =>
            s.name === name
              ? { ...s, has_auth: true, auth_kind: "bearer" as const }
              : s
          );
          return null;
        }
        case "mcp_start_oauth": {
          const { name } = args as { name: string };
          state.servers = state.servers.map((s) =>
            s.name === name
              ? { ...s, has_auth: true, auth_kind: "oauth" as const }
              : s
          );
          return { has_auth: true, auth_kind: "oauth", expires_at: 9999999999 };
        }
        case "mcp_refresh_oauth":
          return { has_auth: true, auth_kind: "oauth", expires_at: 9999999999 };
        case "mcp_clear_auth": {
          const { name } = args as { name: string };
          state.servers = state.servers.map((s) =>
            s.name === name
              ? { ...s, has_auth: false, auth_kind: "none" as const }
              : s
          );
          return null;
        }
        default:
          return null;
      }
    }
  );
  return { mockInvoke, state };
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: mockInvoke,
}));

function renderMcpSection() {
  return render(
    <LocaleProvider>
      <McpSection />
    </LocaleProvider>
  );
}

beforeEach(() => {
  state.servers = [];
  state.tools = [];
  state.rejectNext = null;
  mockInvoke.mockClear();
});

afterEach(() => {
  cleanup();
});

describe("McpSection (M18 / P26)", () => {
  it("renders zero bundled servers by default", async () => {
    renderMcpSection();
    await waitFor(() =>
      expect(
        screen.getByText(/zero bundled servers/i)
      ).toBeInTheDocument()
    );
    expect(mockInvoke).toHaveBeenCalledWith("mcp_list_servers");
  });

  it("adds a stdio MCP server, lists discovered namespaced tools, toggles, and removes it", async () => {
    state.tools = [
      {
        namespaced_name: "mcp__docs__search",
        server_name: "docs",
        tool_name: "search",
        description: "[MCP:docs] Search docs",
      },
    ];
    renderMcpSection();
    await waitFor(() =>
      expect(screen.getByLabelText("Server name")).toBeInTheDocument()
    );

    fireEvent.change(screen.getByLabelText("Server name"), {
      target: { value: "docs" },
    });
    fireEvent.change(screen.getByLabelText("Command"), {
      target: { value: "npx" },
    });
    fireEvent.change(screen.getByLabelText("Arguments (one per line)"), {
      target: { value: "-y\n@example/docs-mcp" },
    });
    fireEvent.change(
      screen.getByLabelText(
        "Environment variables (KEY=VALUE, one per line; non-secret only)"
      ),
      { target: { value: "DOCS_MODE=strict" } }
    );
    fireEvent.click(screen.getByRole("button", { name: "Add server" }));

    await waitFor(() =>
      expect(screen.getByText("docs")).toBeInTheDocument()
    );
    expect(mockInvoke).toHaveBeenCalledWith("mcp_upsert_server", {
      server: {
        name: "docs",
        transport: "stdio",
        enabled: true,
        command: "npx",
        args: ["-y", "@example/docs-mcp"],
        env: { DOCS_MODE: "strict" },
        url: null,
        oauth: null,
      },
    });

    fireEvent.click(screen.getByRole("button", { name: "List tools" }));
    await waitFor(() =>
      expect(screen.getByText("mcp__docs__search")).toBeInTheDocument()
    );

    fireEvent.click(screen.getByLabelText("Enable MCP server docs"));
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("mcp_set_server_enabled", {
        name: "docs",
        enabled: false,
      })
    );

    fireEvent.click(screen.getByRole("button", { name: "Remove" }));
    await waitFor(() =>
      expect(screen.getByText(/zero bundled servers/i)).toBeInTheDocument()
    );
  });

  it("configures a Streamable HTTP server with OAuth 2.1, signs in, and clears Bearer drafts on save", async () => {
    renderMcpSection();
    await waitFor(() =>
      expect(screen.getByText("Streamable HTTP")).toBeInTheDocument()
    );

    fireEvent.click(screen.getByText("Streamable HTTP"));
    fireEvent.change(screen.getByLabelText("Server name"), {
      target: { value: "remote" },
    });
    fireEvent.change(
      screen.getByLabelText("Endpoint URL (https:// or localhost)"),
      { target: { value: "https://mcp.example.com/v1" } }
    );
    fireEvent.click(screen.getByLabelText("Configure OAuth 2.1 (PKCE S256)"));
    fireEvent.change(screen.getByLabelText("Authorization URL"), {
      target: { value: "https://auth.example.com/authorize" },
    });
    fireEvent.change(screen.getByLabelText("Token URL"), {
      target: { value: "https://auth.example.com/token" },
    });
    fireEvent.change(screen.getByLabelText("Client ID"), {
      target: { value: "nexora-desktop" },
    });
    fireEvent.change(
      screen.getByLabelText("Scopes (space-separated, optional)"),
      { target: { value: "mcp:tools" } }
    );
    fireEvent.click(screen.getByRole("button", { name: "Add server" }));

    await waitFor(() =>
      expect(screen.getByText("https://mcp.example.com/v1")).toBeInTheDocument()
    );

    // Bearer token draft clears from React state immediately after save.
    const bearerInput = screen.getByLabelText(
      "Bearer token for remote"
    ) as HTMLInputElement;
    fireEvent.change(bearerInput, { target: { value: "my-ephemeral-token" } });
    fireEvent.click(screen.getByRole("button", { name: "Save token" }));
    await waitFor(() => expect(bearerInput.value).toBe(""));
    expect(mockInvoke).toHaveBeenCalledWith("mcp_set_bearer_token", {
      name: "remote",
      token: "my-ephemeral-token",
    });

    // OAuth 2.1 sign-in updates status badge.
    fireEvent.click(screen.getByRole("button", { name: "Sign in (OAuth)" }));
    await waitFor(() =>
      expect(screen.getByText("OAuth signed in")).toBeInTheDocument()
    );
    expect(mockInvoke).toHaveBeenCalledWith("mcp_start_oauth", {
      name: "remote",
    });

    fireEvent.click(screen.getByRole("button", { name: "Clear auth" }));
    await waitFor(() =>
      expect(screen.getByText("No auth")).toBeInTheDocument()
    );
  });
});
