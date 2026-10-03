//! Workspace terminal panel: user-authored commands through the existing
//! agent `execute_command` tool path.
//!
//! Presentational over the `terminal_run` / `terminal_kill` IPC wrappers
//! (which dispatch a real tool call — workspace-scoped cwd, hard timeout,
//! bounded capture, truncation with notice): runs are synchronous (output
//! arrives on completion, no streaming), one at a time (single session),
//! and session-only (no persistence across restart). Scrollback renders as
//! plain text inside the shared `.nex-agent-terminal` containment
//! primitives from `conversation.css` — no xterm, no new npm deps.
//!
//! Approval UX: commands are typed by the user directly, so the Run press
//! IS the approval (no agent park exists for user-authored commands); the
//! wrapper always passes the backend's per-call confirmation, like the git
//! writes. `cd <relative-path>` is intercepted client-side (each backend
//! run spawns a fresh shell, so `cd` could never persist there) and only
//! changes the in-memory working directory shown in the indicator.
//! Interactive-TUI programs (`vim`, `ssh`, …) are unsupported — stdin is
//! closed backend-side, so they run until the timeout; the panel warns but
//! never blocks. Limits are surfaced in the header subtitle: hard timeout,
//! output cap with notice, workspace scope.
//!
//! Keyboard: the input autofocuses on open; Enter runs, Shift+Enter inserts
//! a newline, ArrowUp/Down walk the in-memory history. No entrance
//! animation: the panel renders instantly (reduced-motion safe).

import { useCallback, useEffect, useRef, useState } from "react";

import {
  getWorkspaceRoot,
  terminalKill,
  terminalRun,
  type CommandError,
} from "../lib/tauri";
import M3Button from "./M3Button";

/** One scrollback entry: the command plus its outcome. `pending` marks the
 * in-flight run (Stop targets it); `stopped` marks a stop-killed run;
 * `error` carries a backend rejection (shown in the body, never lost). */
interface TerminalBlock {
  id: number;
  command: string;
  cwdLabel: string;
  output: string;
  success: boolean;
  truncated: boolean;
  pending: boolean;
  stopped: boolean;
  error: string | null;
}

/** `cd` target that stays inside the workspace: absolute paths, home
 * shortcuts, and dotdot escapes are refused with fixed vocabulary. */
function resolveCd(current: string, target: string): string {
  const raw = target.trim();
  if (raw === "" || raw === "~" || raw.startsWith("/") || /^[a-zA-Z]:/.test(raw)) {
    throw new Error("cd takes a workspace-relative path only");
  }
  const parts = [...current.split("/").filter(Boolean)];
  for (const segment of raw.split("/")) {
    if (segment === "" || segment === ".") continue;
    if (segment === "..") {
      if (parts.length === 0) throw new Error("cd cannot leave the workspace root");
      parts.pop();
      continue;
    }
    parts.push(segment);
  }
  return parts.join("/");
}

/** First-word match for programs that need a live terminal (stdin is closed
 * backend-side, so they hang until the timeout). Warning only — never a
 * block (flags like `vim --version` exit fine). */
const INTERACTIVE_RE = /^(vi|vim|nvim|nano|emacs|ssh|less|more|top|htop|watch|tmux|screen)\b/;

function toMessage(error: unknown): string {
  if (
    typeof error === "object" &&
    error !== null &&
    typeof (error as CommandError).message === "string"
  ) {
    return (error as CommandError).message;
  }
  if (error instanceof Error) return error.message;
  return String(error);
}

export interface TerminalPanelProps {
  onClose: () => void;
  /** Palette-raised action (clear / focus-input). The token identifies the
   * request so a re-render never replays it. */
  request?: { token: number; action: "clear" | "focus-input" } | null;
}

let nextBlockId = 1;

export default function TerminalPanel({ onClose, request = null }: TerminalPanelProps) {
  const [blocks, setBlocks] = useState<TerminalBlock[]>([]);
  const [input, setInput] = useState<string>("");
  const [history, setHistory] = useState<string[]>([]);
  const [historyIndex, setHistoryIndex] = useState<number | null>(null);
  const [cwdRel, setCwdRel] = useState<string>("");
  const [workspaceRoot, setWorkspaceRoot] = useState<string | null>(null);
  const [running, setRunning] = useState<boolean>(false);
  const [stopping, setStopping] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const [interactiveWarn, setInteractiveWarn] = useState<boolean>(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const stoppingRef = useRef(false);
  stoppingRef.current = stopping;

  useEffect(() => {
    let live = true;
    void getWorkspaceRoot()
      .then((root) => {
        if (live) setWorkspaceRoot(root);
      })
      .catch((e: unknown) => {
        if (live) setError(toMessage(e));
      });
    return () => {
      live = false;
    };
  }, []);

  // Scrollback follows new output instantly (no smooth scroll — instant
  // jumps are the reduced-motion-safe default for a log view).
  useEffect(() => {
    const node = scrollRef.current;
    if (node) node.scrollTop = node.scrollHeight;
  }, [blocks]);

  // Palette requests: clear wipes the session scrollback; focus-input moves
  // keyboard focus to the command line (same handlers as the panel's own
  // buttons — no duplicated logic).
  const seenRequestRef = useRef<number | null>(null);
  const clearRef = useRef<() => void>(() => {});
  const handleClear = useCallback(() => {
    setBlocks([]);
    setError(null);
  }, []);
  clearRef.current = handleClear;
  useEffect(() => {
    if (!request || seenRequestRef.current === request.token) return;
    seenRequestRef.current = request.token;
    if (request.action === "clear") {
      clearRef.current();
      return;
    }
    inputRef.current?.focus();
  }, [request]);

  const cwdLabel = workspaceRoot
    ? cwdRel === ""
      ? workspaceRoot
      : `${workspaceRoot}/${cwdRel.replace(/\//g, "/")}`
    : cwdRel === ""
      ? "(workspace)"
      : cwdRel;

  const handleRun = useCallback(
    async (raw: string) => {
      const command = raw.trim();
      if (command === "" || running) return;
      // `cd` never reaches the backend: each run spawns a fresh shell, so a
      // remote `cd` could not persist — it only retargets the in-memory cwd
      // the next run is scoped to.
      const cdMatch = command.match(/^cd(?:\s+(.*))?$/);
      if (cdMatch) {
        const target = (cdMatch[1] ?? "").trim().replace(/^["']|["']$/g, "");
        try {
          setCwdRel(resolveCd(cwdRel, target));
          setError(null);
        } catch (e) {
          setError(toMessage(e));
        }
        setHistory((prev) => [...prev.slice(-99), command]);
        setHistoryIndex(null);
        setInput("");
        inputRef.current?.focus();
        return;
      }
      const blockId = nextBlockId;
      nextBlockId += 1;
      const label = cwdLabel;
      setBlocks((prev) => [
        ...prev,
        {
          id: blockId,
          command,
          cwdLabel: label,
          output: "",
          success: true,
          truncated: false,
          pending: true,
          stopped: false,
          error: null,
        },
      ]);
      setHistory((prev) => [...prev.slice(-99), command]);
      setHistoryIndex(null);
      setInput("");
      setError(null);
      setRunning(true);
      try {
        const result = await terminalRun(command, cwdRel === "" ? null : cwdRel);
        setBlocks((prev) =>
          prev.map((block) =>
            block.id === blockId
              ? {
                  ...block,
                  output: result.output,
                  success: result.success,
                  truncated: result.truncated,
                  pending: false,
                }
              : block,
          ),
        );
      } catch (e) {
        const message = toMessage(e);
        if (stoppingRef.current) {
          // Our own Stop won the race: mark the block stopped, not failed.
          setBlocks((prev) =>
            prev.map((block) =>
              block.id === blockId ? { ...block, pending: false, stopped: true } : block,
            ),
          );
        } else {
          setBlocks((prev) =>
            prev.map((block) =>
              block.id === blockId
                ? { ...block, pending: false, success: false, error: message }
                : block,
            ),
          );
        }
      } finally {
        setRunning(false);
        setStopping(false);
        inputRef.current?.focus();
      }
    },
    [cwdLabel, cwdRel, running],
  );

  const handleStop = useCallback(async () => {
    if (!running || stopping) return;
    setStopping(true);
    try {
      await terminalKill();
      // The pending `terminalRun` rejects with the stop error, which the
      // run handler above turns into the block's stopped mark. If the run
      // already finished (kill returned false), the finally there still
      // clears `stopping`.
    } catch (e) {
      setError(toMessage(e));
      setStopping(false);
    }
  }, [running, stopping]);

  const handleInputKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLInputElement>) => {
      if (event.key === "Enter" && !event.shiftKey) {
        event.preventDefault();
        void handleRun(input);
        return;
      }
      if (event.key === "ArrowUp") {
        event.preventDefault();
        if (history.length === 0) return;
        const next = historyIndex === null ? history.length - 1 : Math.max(0, historyIndex - 1);
        setHistoryIndex(next);
        setInput(history[next]);
        return;
      }
      if (event.key === "ArrowDown") {
        event.preventDefault();
        if (historyIndex === null) return;
        const next = historyIndex + 1;
        if (next >= history.length) {
          setHistoryIndex(null);
          setInput("");
        } else {
          setHistoryIndex(next);
          setInput(history[next]);
        }
      }
    },
    [handleRun, history, historyIndex, input],
  );

  const handleInputChange = useCallback((event: React.ChangeEvent<HTMLInputElement>) => {
    const value = event.target.value;
    setInput(value);
    setHistoryIndex(null);
    setInteractiveWarn(INTERACTIVE_RE.test(value.trim()));
  }, []);

  return (
    <div className="nex-term" role="group" aria-label="Terminal">
      <header className="nex-vcs-header">
        <div className="nex-vcs-heading">
          <h2 className="nex-vcs-title">Terminal</h2>
          <p className="nex-vcs-subtitle">
            Workspace commands only — your Run press is the approval. Hard timeout, output
            capped with a notice, no streaming, one run at a time. Interactive programs
            (vim, ssh) are unsupported: stdin is closed.
          </p>
        </div>
        <div className="nex-vcs-header-actions">
          <M3Button variant="quiet" onClick={handleClear} disabled={running || blocks.length === 0}>
            Clear
          </M3Button>
          <M3Button variant="quiet" onClick={onClose}>
            Back to conversations
          </M3Button>
        </div>
      </header>

      <div className="nex-term-body">
        {error && (
          <div className="nex-composer-error nex-fade-in" role="alert">
            {error}
          </div>
        )}
        <div
          ref={scrollRef}
          className="nex-term-scroll"
          role="log"
          aria-label="Terminal scrollback"
          aria-live="off"
          tabIndex={0}
        >
          {blocks.length === 0 ? (
            <p className="nex-agent-empty">
              No commands yet — type a workspace command below and press Enter.
            </p>
          ) : (
            blocks.map((block) => (
              <div key={block.id} className="nex-agent-terminal" role="group" aria-label="Terminal output">
                <div className="nex-agent-terminal-header">
                  <span className="nex-agent-terminal-prompt" aria-hidden="true">
                    $
                  </span>
                  <span className="nex-agent-terminal-command nex-tag-mono">{block.command}</span>
                  <span className="nex-agent-terminal-cwd nex-tag-mono" title={block.cwdLabel}>
                    ({block.cwdLabel})
                  </span>
                  <span
                    className={
                      "nex-tag nex-tag-mono nex-term-exit" +
                      (block.pending
                        ? " nex-term-exit-running"
                        : block.stopped
                          ? " nex-term-exit-stopped"
                          : block.success
                            ? " nex-term-exit-ok"
                            : " nex-term-exit-fail")
                    }
                  >
                    {block.pending
                      ? "running"
                      : block.stopped
                        ? "stopped"
                        : block.success
                          ? "exit 0"
                          : "non-zero exit"}
                  </span>
                  {block.truncated && !block.pending && (
                    <span className="nex-tag nex-tag-mono nex-term-exit nex-term-exit-truncated">
                      truncated
                    </span>
                  )}
                </div>
                <div className="nex-agent-terminal-body">
                  {block.pending ? (
                    <p className="nex-agent-empty">Running…</p>
                  ) : block.error ? (
                    <pre className="nex-agent-terminal-stderr">{block.error}</pre>
                  ) : block.output === "" ? (
                    <p className="nex-agent-empty">(no output)</p>
                  ) : (
                    <pre className="nex-agent-terminal-stdout">{block.output}</pre>
                  )}
                </div>
              </div>
            ))
          )}
        </div>

        <div className="nex-term-input-row">
          <span className="nex-agent-terminal-prompt nex-term-prompt" aria-hidden="true">
            $
          </span>
          <span className="nex-tag nex-tag-mono nex-term-cwd" title={cwdLabel}>
            {cwdLabel}
          </span>
          <input
            ref={inputRef}
            className="nex-term-input"
            type="text"
            autoFocus
            placeholder="workspace command (cd changes directory)"
            aria-label="Terminal command input"
            value={input}
            disabled={running}
            onChange={handleInputChange}
            onKeyDown={(event) => handleInputKeyDown(event)}
          />
          {running ? (
            <M3Button
              variant="destructive"
              size="sm"
              disabled={stopping}
              onClick={() => void handleStop()}
              title="Stop the running command"
            >
              {stopping ? "Stopping…" : "Stop"}
            </M3Button>
          ) : (
            <M3Button
              variant="primary"
              size="sm"
              disabled={input.trim() === ""}
              onClick={() => void handleRun(input)}
              title="Run the command in the workspace"
            >
              Run
            </M3Button>
          )}
        </div>
        {interactiveWarn && !running && (
          <p className="nex-vcs-notice" role="note">
            That looks like an interactive program — stdin is closed, so it will run until
            the timeout. Prefer a non-interactive flag (e.g. `--version`, `--help`).
          </p>
        )}
      </div>
    </div>
  );
}
