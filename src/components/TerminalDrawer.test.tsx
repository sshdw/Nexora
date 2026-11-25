import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  TerminalPtyExitEvent,
  TerminalPtyOutputEvent,
} from "../lib/tauri";
import { LOCALE_KEY, LocaleProvider } from "../lib/useLocale";
import TerminalDrawer, { type TerminalDrawerProps } from "./TerminalDrawer";

type TauriEvent<T> = { payload: T };

const {
  mockInvoke,
  mockListen,
  emitPtyOutput,
  emitPtyExit,
  xtermInstances,
  fitAddonInstances,
  resizeCallbacks,
} = vi.hoisted(() => {
  const outputListeners: Array<(ev: TauriEvent<TerminalPtyOutputEvent>) => void> =
    [];
  const exitListeners: Array<(ev: TauriEvent<TerminalPtyExitEvent>) => void> =
    [];

  const mockInvoke = vi.fn();
  const mockListen = vi.fn(
    async (
      eventName: string,
      handler: (event: TauriEvent<unknown>) => void
    ): Promise<() => void> => {
      if (eventName === "terminal-pty-output") {
        const typed = handler as (ev: TauriEvent<TerminalPtyOutputEvent>) => void;
        outputListeners.push(typed);
        return () => {
          const idx = outputListeners.indexOf(typed);
          if (idx >= 0) outputListeners.splice(idx, 1);
        };
      }
      if (eventName === "terminal-pty-exit") {
        const typed = handler as (ev: TauriEvent<TerminalPtyExitEvent>) => void;
        exitListeners.push(typed);
        return () => {
          const idx = exitListeners.indexOf(typed);
          if (idx >= 0) exitListeners.splice(idx, 1);
        };
      }
      return () => {};
    }
  );

  const emitPtyOutput = (payload: TerminalPtyOutputEvent): void => {
    for (const handler of [...outputListeners]) handler({ payload });
  };

  const emitPtyExit = (payload: TerminalPtyExitEvent): void => {
    for (const handler of [...exitListeners]) handler({ payload });
  };

  interface MockTerminalInstance {
    cols: number;
    rows: number;
    options: Record<string, unknown>;
    dataHandlers: Array<(data: string) => void>;
    loadAddon: ReturnType<typeof vi.fn>;
    open: ReturnType<typeof vi.fn>;
    write: ReturnType<typeof vi.fn>;
    clear: ReturnType<typeof vi.fn>;
    reset: ReturnType<typeof vi.fn>;
    focus: ReturnType<typeof vi.fn>;
    dispose: ReturnType<typeof vi.fn>;
    onData: ReturnType<typeof vi.fn>;
    triggerData: (data: string) => void;
  }

  interface MockFitAddonInstance {
    fit: ReturnType<typeof vi.fn>;
  }

  const xtermInstances: MockTerminalInstance[] = [];
  const fitAddonInstances: MockFitAddonInstance[] = [];
  const resizeCallbacks: Array<() => void> = [];

  return {
    mockInvoke,
    mockListen,
    emitPtyOutput,
    emitPtyExit,
    xtermInstances,
    fitAddonInstances,
    resizeCallbacks,
  };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mockListen }));

vi.mock("@xterm/xterm", () => {
  class MockTerminal {
    cols = 80;
    rows = 24;
    options: Record<string, unknown>;
    dataHandlers: Array<(data: string) => void> = [];
    loadAddon = vi.fn();
    open = vi.fn();
    write = vi.fn();
    clear = vi.fn();
    reset = vi.fn();
    focus = vi.fn();
    dispose = vi.fn();
    onData = vi.fn((cb: (data: string) => void) => {
      this.dataHandlers.push(cb);
      return {
        dispose: () => {
          const idx = this.dataHandlers.indexOf(cb);
          if (idx >= 0) this.dataHandlers.splice(idx, 1);
        },
      };
    });

    constructor(opts: Record<string, unknown> = {}) {
      this.options = { ...opts };
      xtermInstances.push(this);
    }

    triggerData(data: string) {
      for (const cb of [...this.dataHandlers]) cb(data);
    }
  }
  return { Terminal: MockTerminal };
});

vi.mock("@xterm/addon-fit", () => {
  class MockFitAddon {
    fit = vi.fn();
    constructor() {
      fitAddonInstances.push(this);
    }
  }
  return { FitAddon: MockFitAddon };
});

class MockResizeObserver {
  private cb: () => void;
  constructor(cb: () => void) {
    this.cb = cb;
    resizeCallbacks.push(cb);
  }
  observe() {}
  unobserve() {}
  disconnect() {
    const idx = resizeCallbacks.indexOf(this.cb);
    if (idx >= 0) resizeCallbacks.splice(idx, 1);
  }
}

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

describe("TerminalDrawer (interactive PTY)", () => {
  const originalResizeObserver = globalThis.ResizeObserver;

  beforeEach(() => {
    localStorage.setItem(LOCALE_KEY, "en");
    mockInvoke.mockReset();
    mockListen.mockClear();
    xtermInstances.length = 0;
    fitAddonInstances.length = 0;
    resizeCallbacks.length = 0;
    globalThis.ResizeObserver =
      MockResizeObserver as unknown as typeof ResizeObserver;
  });

  afterEach(() => {
    cleanup();
    globalThis.ResizeObserver = originalResizeObserver;
  });

  it("renders nothing and does not spawn a PTY when closed", () => {
    const { container } = renderDrawer({ open: false });
    expect(container.firstChild).toBeNull();
    expect(mockInvoke).not.toHaveBeenCalled();
    expect(xtermInstances).toHaveLength(0);
  });

  it("auto-spawns PTY on open, streams output, forwards keystrokes, and focuses terminal", async () => {
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "terminal_pty_spawn") {
        return {
          session_id: "pty-1",
          shell_name: "pwsh",
          cwd: "D:\\projects\\nx",
        };
      }
      return null;
    });

    renderDrawer();

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("terminal_pty_spawn", {
        cols: 80,
        rows: 24,
      })
    );

    expect(await screen.findByText("pwsh · D:\\projects\\nx")).toBeInTheDocument();
    const term = xtermInstances[0];
    expect(term).toBeDefined();
    expect(term.focus).toHaveBeenCalled();
    expect(term.options.fontFamily).toBe(
      '"Cascadia Mono", "Cascadia Code", "JetBrains Mono", "Segoe UI Mono", Consolas, monospace'
    );
    expect(term.options.fontSize).toBe(13);
    expect(term.options.cursorStyle).toBe("block");
    expect(term.options.cursorInactiveStyle).toBe("outline");
    expect((term.options.theme as { cursor?: string } | undefined)?.cursor).toBeTruthy();

    // Streams matching session output and ignores foreign sessions.
    act(() => {
      emitPtyOutput({ session_id: "other-session", data: "ignored" });
      emitPtyOutput({ session_id: "pty-1", data: "PS D:\\projects\\nx> " });
    });
    expect(term.write).toHaveBeenCalledTimes(1);
    expect(term.write).toHaveBeenCalledWith("PS D:\\projects\\nx> ");

    // Forwards user keystrokes to terminal_pty_write.
    act(() => {
      term.triggerData("git status\r");
    });
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("terminal_pty_write", {
        sessionId: "pty-1",
        data: "git status\r",
      })
    );
  });

  it("resizes the PTY when ResizeObserver fires and terminal grid changes", async () => {
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "terminal_pty_spawn") {
        return {
          session_id: "pty-resize",
          shell_name: "bash",
          cwd: "/workspace",
        };
      }
      return null;
    });

    renderDrawer();
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("terminal_pty_spawn", {
        cols: 80,
        rows: 24,
      })
    );

    const term = xtermInstances[0];
    const fitAddon = fitAddonInstances[0];
    fitAddon.fit.mockImplementation(() => {
      term.cols = 112;
      term.rows = 30;
    });

    act(() => {
      for (const cb of [...resizeCallbacks]) cb();
    });

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("terminal_pty_resize", {
        sessionId: "pty-resize",
        cols: 112,
        rows: 30,
      })
    );

    // Collapsed / tiny transition frames (< 10 cols or < 4 rows) must not resize ConPTY.
    mockInvoke.mockClear();
    fitAddon.fit.mockImplementation(() => {
      term.cols = 2;
      term.rows = 1;
    });
    act(() => {
      for (const cb of [...resizeCallbacks]) cb();
    });
    await new Promise((r) => setTimeout(r, 30));
    expect(mockInvoke).not.toHaveBeenCalledWith(
      "terminal_pty_resize",
      expect.anything()
    );
  });

  it("supports Clear, Restart, exit status display, and Close cleanup", async () => {
    let spawnCount = 0;
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "terminal_pty_spawn") {
        spawnCount += 1;
        return {
          session_id: `pty-${spawnCount}`,
          shell_name: "pwsh",
          cwd: "D:\\projects\\nx",
        };
      }
      if (cmd === "terminal_pty_kill") return true;
      return null;
    });

    const { props, unmount } = renderDrawer();
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("terminal_pty_spawn", {
        cols: 80,
        rows: 24,
      })
    );

    const term = xtermInstances[0];

    // Clear clears scrollback and refocuses terminal.
    fireEvent.click(screen.getByRole("button", { name: "Clear" }));
    expect(term.clear).toHaveBeenCalledTimes(1);

    // Process exit event shows Exited status badge.
    act(() => {
      emitPtyExit({ session_id: "pty-1", exit_code: 0 });
    });
    expect(screen.getByRole("status")).toHaveTextContent("Exited (0)");

    // Restart resets xterm and spawns a fresh PTY session.
    fireEvent.click(screen.getByRole("button", { name: "Restart" }));
    expect(term.reset).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(spawnCount).toBe(2));
    expect(screen.queryByRole("status")).not.toBeInTheDocument();

    // Restart while active kills previous session before spawning next.
    fireEvent.click(screen.getByRole("button", { name: "Restart" }));
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("terminal_pty_kill", {
        sessionId: "pty-2",
      })
    );
    await waitFor(() => expect(spawnCount).toBe(3));

    // Close button invokes onClose.
    fireEvent.click(screen.getByRole("button", { name: "Close terminal" }));
    expect(props.onClose).toHaveBeenCalledTimes(1);

    // Unmount kills the currently live PTY session cleanly.
    unmount();
    expect(mockInvoke).toHaveBeenCalledWith("terminal_pty_kill", {
      sessionId: "pty-3",
    });
    expect(term.dispose).toHaveBeenCalledTimes(1);
  });

  it("shows an error alert when terminal_pty_spawn fails", async () => {
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "terminal_pty_spawn") {
        throw {
          kind: "io",
          message: "the interactive terminal session could not be started",
        };
      }
      return null;
    });

    renderDrawer();
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "the interactive terminal session could not be started"
    );
  });
});
