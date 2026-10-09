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
//! wrapper mints a single-use server-side confirmation id per run
//! (NEX-SEC-004), like the data-management wrappers. `cd <relative-path>` is intercepted client-side (each backend
//! run spawns a fresh shell, so `cd` could never persist there) and only
//! changes the in-memory working directory shown in the indicator.
//! Interactive-TUI programs (`vim`, `ssh`, …) are unsupported — stdin is
//! closed backend-side, so they run until the timeout; the panel warns but
//! never blocks. Limits are surfaced in the header subtitle: hard timeout,
//! output cap with notice, workspace scope.
//!
//! Error intelligence: failed runs (non-zero exit badge) offer an Explain
//! action that sends the capped failed output through the existing AI
//! execution path (like commit-message generation) and renders the
//! diagnosis plus a copy-only fix suggestion — never auto-applied. The
//! disclosure notice (external transmission) and the truncation notice
//! mirror the version-control panel's wording pattern.
//!
//! Keyboard: the input autofocuses on open; Enter runs, Shift+Enter inserts
//! a newline, ArrowUp/Down walk the in-memory history. No entrance
//! animation: the panel renders instantly (reduced-motion safe).

import { useCallback, useEffect, useRef, useState } from "react";

import {
  getSetting,
  getWorkspaceRoot,
  terminalExplain,
  terminalKill,
  terminalRun,
  type CommandError,
  type ErrorExplanation,
} from "../lib/tauri";
import M3Button from "./M3Button";
import { getLocale, tr } from "../lib/strings";
import { useStrings } from "../lib/useLocale";

/** One scrollback entry: the command plus its outcome. `pending` marks the
 * in-flight run (Stop targets it); `stopped` marks a stop-killed run;
 * `cancelled` marks a run refused at the native confirmation prompt (the
 * command never executed — distinct from stopped and from failed);
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
  cancelled: boolean;
  error: string | null;
}

/** `cd` target that stays inside the workspace: absolute paths, home
 * shortcuts, and dotdot escapes are refused with fixed vocabulary. */
function resolveCd(current: string, target: string, locale = getLocale()): string {
  const raw = target.trim();
  if (raw === "" || raw === "~" || raw.startsWith("/") || /^[a-zA-Z]:/.test(raw)) {
    throw new Error(tr(locale, "term.cdRelative"));
  }
  const parts = [...current.split("/").filter(Boolean)];
  for (const segment of raw.split("/")) {
    if (segment === "" || segment === ".") continue;
    if (segment === "..") {
      if (parts.length === 0) throw new Error(tr(locale, "term.cdRoot"));
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

/** Explain state for one failed block: the AI diagnosis plus the copy-only
 * fix suggestion. `copied` names the field whose Copy press last succeeded
 * (`"diagnosis"` / `"fix"`), so the button can confirm without a timer. */
interface BlockExplain {
  loading: boolean;
  explanation: string | null;
  suggestedFix: string | null;
  truncatedInput: boolean;
  error: string | null;
  copied: string | null;
}

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

/** True when the backend refused because the user cancelled at the native OS
 * confirmation prompt (`request_confirmation` mints nothing, so the command
 * never ran). Distinct from a genuine run failure: nothing executed. */
function isConfirmationCancelled(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    (error as CommandError).kind === "confirmationRequired"
  );
}

export interface TerminalPanelProps {
  onClose: () => void;
  /** Palette-raised action (clear / focus-input). The token identifies the
   * request so a re-render never replays it. */
  request?: { token: number; action: "clear" | "focus-input" } | null;
}

let nextBlockId = 1;

export default function TerminalPanel({ onClose, request = null }: TerminalPanelProps) {
  const { t } = useStrings();
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
  const [explains, setExplains] = useState<Record<number, BlockExplain>>({});
  const inputRef = useRef<HTMLInputElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const stoppingRef = useRef(false);
  stoppingRef.current = stopping;
  // Synchronous in-flight run guard (see handleRun): flips in the same
  // tick as the Run press, unlike `running` state.
  const runningRef = useRef(false);
  // Synchronous per-block explain guard: `explains[blockId].loading`
  // lands after re-render, so two rapid Explain presses could both pass
  // the state guard and stack two paid AI requests. The ref flips in
  // the same tick.
  const explainingRef = useRef<Set<number>>(new Set());

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
    setExplains({});
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
      ? t("term.wsFallback")
      : cwdRel;

  const handleRun = useCallback(
    async (raw: string) => {
      const command = raw.trim();
      // Synchronous in-flight guard: `running` state lands after re-render,
      // so two rapid Enters could both pass the state guard and stack a
      // cosmetic AlreadyRunning error block. The ref flips in the same tick.
      if (command === "" || runningRef.current) return;
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
          cancelled: false,
          error: null,
        },
      ]);
      setHistory((prev) => [...prev.slice(-99), command]);
      setHistoryIndex(null);
      setInput("");
      setError(null);
      runningRef.current = true;
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
        } else if (isConfirmationCancelled(e)) {
          // Cancelled at the native confirmation prompt (NEX-SEC-004): no id
          // was minted and the command never ran. Reported honestly as a
          // cancelled run — never as a failed command, and never with the
          // exit-ok badge: `cancelled` renders its own badge below while the
          // body keeps the explanatory text.
          setBlocks((prev) =>
            prev.map((block) =>
              block.id === blockId
                ? {
                    ...block,
                    pending: false,
                    success: false,
                    cancelled: true,
                    error: tr(getLocale(), "term.runCancelled"),
                  }
                : block,
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
        runningRef.current = false;
        setRunning(false);
        setStopping(false);
        inputRef.current?.focus();
      }
    },
    [cwdLabel, cwdRel],
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

  // Error intelligence: explain one failed block through the existing AI
  // execution path (same persisted provider/model settings as commit-message
  // generation). The suggestion is copy-only — this handler never feeds it
  // back into `terminalRun`, so nothing auto-executes.
  const handleExplain = useCallback(async (blockId: number, output: string, runTruncated: boolean) => {
    if (explainingRef.current.has(blockId)) return;
    explainingRef.current.add(blockId);
    setExplains((prev) => ({
      ...prev,
      [blockId]: {
        loading: true,
        explanation: prev[blockId]?.explanation ?? null,
        suggestedFix: prev[blockId]?.suggestedFix ?? null,
        truncatedInput: prev[blockId]?.truncatedInput ?? false,
        error: null,
        copied: null,
      },
    }));
    try {
      const [provider, model] = await Promise.all([
        getSetting("provider.selected"),
        getSetting("provider.model"),
      ]);
      if (!provider || !model) {
        setExplains((prev) => ({
          ...prev,
          [blockId]: {
            loading: false,
            explanation: prev[blockId]?.explanation ?? null,
            suggestedFix: prev[blockId]?.suggestedFix ?? null,
            truncatedInput: prev[blockId]?.truncatedInput ?? false,
            error: tr(getLocale(), "term.needProviderExplain"),
            copied: null,
          },
        }));
        return;
      }
      const exitContext = runTruncated
        ? "non-zero exit (run output already truncated by the tool path)"
        : "non-zero exit";
      const result: ErrorExplanation = await terminalExplain(output, exitContext, provider, model);
      setExplains((prev) => ({
        ...prev,
        [blockId]: {
          loading: false,
          explanation: result.explanation,
          suggestedFix: result.suggested_fix,
          truncatedInput: result.truncated_input,
          error: null,
          copied: null,
        },
      }));
    } catch (e) {
      setExplains((prev) => ({
        ...prev,
        [blockId]: {
          loading: false,
          explanation: prev[blockId]?.explanation ?? null,
          suggestedFix: prev[blockId]?.suggestedFix ?? null,
          truncatedInput: prev[blockId]?.truncatedInput ?? false,
          error: toMessage(e),
          copied: null,
        },
      }));
    } finally {
      explainingRef.current.delete(blockId);
    }
  }, []);

  const handleCopyExplain = useCallback(async (blockId: number, field: "diagnosis" | "fix", text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setExplains((prev) =>
        prev[blockId]
          ? { ...prev, [blockId]: { ...prev[blockId], copied: field } }
          : prev,
      );
    } catch {
      setExplains((prev) =>
        prev[blockId]
          ? { ...prev, [blockId]: { ...prev[blockId], copied: null, error: tr(getLocale(), "common.copyFailed") } }
          : prev,
      );
    }
  }, []);

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
    <div className="nex-term" role="group" aria-label={t("term.group")}>
      <header className="nex-vcs-header">
        <div className="nex-vcs-heading">
          <h2 className="nex-vcs-title">{t("term.title")}</h2>
          <p className="nex-vcs-subtitle">
            {t("term.subtitle")}
          </p>
        </div>
        <div className="nex-vcs-header-actions">
          <M3Button variant="quiet" onClick={handleClear} disabled={running || blocks.length === 0}>
            {t("term.clear")}
          </M3Button>
          <M3Button variant="quiet" onClick={onClose}>
            {t("common.backToConversations")}
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
          aria-label={t("term.scrollback")}
          aria-live="off"
          tabIndex={0}
        >
          {blocks.length === 0 ? (
            <p className="nex-agent-empty">
              {t("term.empty")}
            </p>
          ) : (
            blocks.map((block) => {
              const explain = explains[block.id];
              const explainLoading = explain?.loading === true;
              return (
              <div key={block.id} className="nex-agent-terminal" role="group" aria-label={t("term.blockAria")}>
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
                        : block.cancelled
                          ? " nex-term-exit-stopped"
                          : block.stopped
                            ? " nex-term-exit-stopped"
                            : block.success
                              ? " nex-term-exit-ok"
                              : " nex-term-exit-fail")
                    }
                  >
                    {block.pending
                      ? t("term.exitRunning")
                      : block.cancelled
                        ? t("term.exitCancelled")
                        : block.stopped
                          ? t("term.exitStopped")
                          : block.success
                            ? t("term.exitOk")
                            : t("term.exitFail")}
                  </span>
                  {block.truncated && !block.pending && (
                    <span className="nex-tag nex-tag-mono nex-term-exit nex-term-exit-truncated">
                      {t("term.truncatedTag")}
                    </span>
                  )}
                </div>
                <div className="nex-agent-terminal-body">
                  {block.pending ? (
                    <p className="nex-agent-empty">{t("term.pendingText")}</p>
                  ) : block.error ? (
                    <pre className="nex-agent-terminal-stderr">{block.error}</pre>
                  ) : block.output === "" ? (
                    <p className="nex-agent-empty">{t("term.noOutput")}</p>
                  ) : (
                    <pre className="nex-agent-terminal-stdout">{block.output}</pre>
                  )}
                </div>
                {!block.pending && !block.success && !block.stopped && !block.error && (
                  <div className="nex-agent-terminal-body">
                    <div className="nex-vcs-header-actions">
                      <M3Button
                        variant="quiet"
                        size="sm"
                        disabled={explainLoading}
                        onClick={() => void handleExplain(block.id, block.output, block.truncated)}
                        title={t("term.explainTitle")}
                      >
                        {explainLoading
                          ? t("vcs.explaining")
                          : explain?.explanation
                            ? t("vcs.explainAgain")
                            : t("vcs.explain")}
                      </M3Button>
                      {explain?.explanation && (
                        <M3Button
                          variant="quiet"
                          size="sm"
                          onClick={() => void handleCopyExplain(block.id, "diagnosis", explain.explanation ?? "")}
                        >
                          {explain.copied === "diagnosis" ? t("vcs.copied") : t("term.copyDiagnosis")}
                        </M3Button>
                      )}
                      {explain?.suggestedFix && (
                        <M3Button
                          variant="quiet"
                          size="sm"
                          onClick={() => void handleCopyExplain(block.id, "fix", explain.suggestedFix ?? "")}
                        >
                          {explain.copied === "fix" ? t("vcs.copied") : t("term.copyFix")}
                        </M3Button>
                      )}
                    </div>
                    <p className="nex-vcs-notice" role="note">
                      {t("term.explainNotice")}
                    </p>
                    {explain?.truncatedInput === true && (
                      <p className="nex-vcs-notice" role="note">
                        {t("term.explainTruncated")}
                      </p>
                    )}
                    {explain?.error && (
                      <div className="nex-composer-error nex-fade-in" role="alert">
                        {explain.error}
                      </div>
                    )}
                    {explain?.explanation && (
                      <pre className="nex-agent-terminal-stdout">{explain.explanation}</pre>
                    )}
                    {explain?.suggestedFix && (
                      <>
                        <p className="nex-vcs-notice" role="note">
                          {t("term.fixHint")}
                        </p>
                        <pre className="nex-agent-terminal-stdout">{explain.suggestedFix}</pre>
                      </>
                    )}
                  </div>
                )}
              </div>
              );
            })
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
            placeholder={t("term.inputPh")}
            aria-label={t("term.inputAria")}
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
              title={t("term.stopTitle")}
            >
              {stopping ? t("term.stopping") : t("term.stop")}
            </M3Button>
          ) : (
            <M3Button
              variant="primary"
              size="sm"
              disabled={input.trim() === ""}
              onClick={() => void handleRun(input)}
              title={t("term.runTitle")}
            >
              {t("term.run")}
            </M3Button>
          )}
        </div>
        {interactiveWarn && !running && (
          <p className="nex-vcs-notice" role="note">
            {t("term.interactiveWarn")}
          </p>
        )}
      </div>
    </div>
  );
}
