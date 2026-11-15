import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { TerminalOutputEvent } from "../lib/tauri";
import { LOCALE_KEY, LocaleProvider } from "../lib/useLocale";
import TerminalDrawer, { type TerminalDrawerProps } from "./TerminalDrawer";

type TauriEvent = { payload: TerminalOutputEvent };

const { mockInvoke, mockListen, emitTerminal } = vi.hoisted(() => {
  const listeners: Array<(event: TauriEvent) => void> = [];
  const mockInvoke = vi.fn();
  const mockListen = vi.fn(
    async (
      _eventName: string,
      handler: (event: TauriEvent) => void
    ): Promise<() => void> => {
      listeners.push(handler);
      return () => {
        const i = listeners.indexOf(handler);
        if (i >= 0) listeners.splice(i, 1);
      };
    }
  );
  const emitTerminal = (payload: TerminalOutputEvent): void => {
    for (const handler of [...listeners]) handler({ payload });
  };
  return { mockInvoke, mockListen, emitTerminal };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mockListen }));

function renderDrawer(overrides: Partial<TerminalDrawerProps> = {}) {
  const props: TerminalDrawerProps = {
    open: true,
    onClose: vi.fn(),
    selectedProvider: "openai",
    selectedModel: "gpt-5.6-terra",
    ...overrides,
  };
  const view = render(
    <LocaleProvider>
      <TerminalDrawer {...props} />
    </LocaleProvider>
  );
  return { ...view, props };
}

describe("TerminalDrawer", () => {
  beforeEach(() => {
    localStorage.setItem(LOCALE_KEY, "en");
    mockInvoke.mockReset();
    mockListen.mockClear();
  });

  afterEach(() => {
    cleanup();
  });

  it("renders nothing when closed and focuses command input when opened", () => {
    const { container, rerender } = renderDrawer({ open: false });
    expect(container.firstChild).toBeNull();

    rerender(
      <LocaleProvider>
        <TerminalDrawer
          open
          onClose={vi.fn()}
          selectedProvider="openai"
          selectedModel="gpt-5.6-terra"
        />
      </LocaleProvider>
    );
    const input = screen.getByLabelText("Command");
    expect(input).toHaveFocus();
  });

  it("streams redacted stdout and stderr chunks and shows Truncated badge", async () => {
    let resolveRun!: (value: unknown) => void;
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "request_confirmation") return "confirm-1";
      if (cmd === "terminal_run") {
        return new Promise((resolve) => {
          resolveRun = resolve;
        });
      }
      return null;
    });

    renderDrawer();

    fireEvent.change(screen.getByLabelText("Command"), {
      target: { value: "cargo test" },
    });
    fireEvent.change(screen.getByLabelText("Working directory (optional)"), {
      target: { value: "src-tauri" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("terminal_run", {
        command: "cargo test",
        cwd: "src-tauri",
        confirmationId: "confirm-1",
      })
    );

    act(() => {
      emitTerminal({
        run_id: 1,
        stream: "stdout",
        chunk: "hello [REDACTED]\n",
        done: false,
        truncated: false,
        killed: false,
        success: null,
      });
      emitTerminal({
        run_id: 1,
        stream: "stderr",
        chunk: "minor warn\n",
        done: false,
        truncated: true,
        killed: false,
        success: null,
      });
    });

    expect(
      screen.getByText((txt) => txt.includes("hello [REDACTED]"))
    ).toBeInTheDocument();
    expect(
      screen.getByText((txt) => txt.includes("minor warn"))
    ).toBeInTheDocument();
    expect(screen.getByText("Truncated")).toBeInTheDocument();

    await act(async () => {
      emitTerminal({
        run_id: 1,
        stream: "status",
        chunk: "",
        done: true,
        truncated: true,
        killed: false,
        success: true,
      });
      resolveRun({
        run_id: 1,
        output:
          "hello [REDACTED]\n--- stderr ---\nminor warn\n[warning: command stream capture truncated at 1048576 bytes (MAX_CAPTURE_BYTES); 32 additional bytes discarded]",
        truncated: true,
        success: true,
      });
    });

    expect(screen.getByText("Exited OK")).toBeInTheDocument();
  });

  it("calls terminal_kill when Stop is clicked and marks Stopped status", async () => {
    let rejectRun!: (err: unknown) => void;
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "request_confirmation") return "confirm-2";
      if (cmd === "terminal_run") {
        return new Promise((_resolve, reject) => {
          rejectRun = reject;
        });
      }
      if (cmd === "terminal_kill") return true;
      return null;
    });

    renderDrawer();
    fireEvent.change(screen.getByLabelText("Command"), {
      target: { value: "sleep 30" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    const stopBtn = await screen.findByRole("button", { name: "Stop" });
    fireEvent.click(stopBtn);

    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith("terminal_kill"));

    await act(async () => {
      emitTerminal({
        run_id: 2,
        stream: "status",
        chunk: "",
        done: true,
        truncated: false,
        killed: true,
        success: false,
      });
      rejectRun({
        kind: "cancelled",
        message: "the terminal command was stopped",
      });
    });

    expect(screen.getByText("Stopped")).toBeInTheDocument();
  });

  it("explains failed commands via terminal_explain and copies suggested fix", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });

    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "request_confirmation") return "confirm-3";
      if (cmd === "terminal_run") {
        return {
          run_id: 3,
          output: "--- stderr ---\ncargo test failed\n",
          truncated: false,
          success: false,
        };
      }
      if (cmd === "terminal_explain") {
        return {
          explanation: "Missing dependency in workspace.",
          suggested_fix: "cargo check --workspace",
          truncated_input: false,
        };
      }
      return null;
    });

    renderDrawer();
    fireEvent.change(screen.getByLabelText("Command"), {
      target: { value: "cargo test" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    const explainBtn = await screen.findByRole("button", {
      name: "Explain error",
    });
    fireEvent.click(explainBtn);

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("terminal_explain", {
        output: "--- stderr ---\ncargo test failed\n",
        exitContext: "Command failed",
        provider: "openai",
        model: "gpt-5.6-terra",
      })
    );

    expect(
      screen.getByText("Missing dependency in workspace.")
    ).toBeInTheDocument();
    expect(screen.getByText("cargo check --workspace")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Copy fix" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("cargo check --workspace"));
    expect(screen.getByRole("button", { name: "Copied" })).toBeInTheDocument();
  });

  it("shows provider warning if Explain error is clicked without active provider/model", async () => {
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "request_confirmation") return "confirm-4";
      if (cmd === "terminal_run") {
        return {
          run_id: 4,
          output: "exit code 1",
          truncated: false,
          success: false,
        };
      }
      return null;
    });

    renderDrawer({ selectedProvider: null, selectedModel: null });
    fireEvent.change(screen.getByLabelText("Command"), {
      target: { value: "false" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Run" }));

    const explainBtn = await screen.findByRole("button", {
      name: "Explain error",
    });
    fireEvent.click(explainBtn);

    expect(
      screen.getByText(
        "Select a provider and model first (Settings), then explain."
      )
    ).toBeInTheDocument();
  });
});
