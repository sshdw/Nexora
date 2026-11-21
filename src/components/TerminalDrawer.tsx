import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import {
  onTerminalOutput,
  terminalExplain,
  terminalKill,
  terminalRun,
  type ErrorExplanation,
  type TerminalOutputEvent,
} from "../lib/tauri";
import { useStrings } from "../lib/useLocale";
import Button from "./Button";
import IconButton from "./IconButton";
import {
  CheckIcon,
  CloseIcon,
  LoaderIcon,
  StopIcon,
  TerminalIcon,
  TrashIcon,
} from "./icons";

export interface TerminalDrawerProps {
  open: boolean;
  onClose: () => void;
  selectedProvider: string | null;
  selectedModel: string | null;
}

const MAX_TERMINAL_OUTPUT_CHARS = 262_144;

function appendTerminalChunk(
  prev: string,
  chunk: string
): { text: string; truncated: boolean } {
  if (!chunk) return { text: prev, truncated: false };
  const combined = prev + chunk;
  if (combined.length <= MAX_TERMINAL_OUTPUT_CHARS) {
    return { text: combined, truncated: false };
  }
  return {
    text: combined.slice(combined.length - MAX_TERMINAL_OUTPUT_CHARS),
    truncated: true,
  };
}

function splitCombinedOutput(output: string): {
  stdout: string;
  stderr: string;
} {
  const marker = "--- stderr ---\n";
  const idx = output.indexOf(marker);
  if (idx < 0) return { stdout: output, stderr: "" };
  return {
    stdout: output.slice(0, idx),
    stderr: output.slice(idx + marker.length),
  };
}

function readErrorMessage(cause: unknown, fallback: string): {
  message: string;
  kind: string | null;
} {
  if (typeof cause === "object" && cause !== null) {
    const rec = cause as { kind?: unknown; message?: unknown };
    const kind = typeof rec.kind === "string" ? rec.kind : null;
    if (typeof rec.message === "string" && rec.message.trim().length > 0) {
      return { message: rec.message, kind };
    }
    return { message: fallback, kind };
  }
  if (cause instanceof Error && cause.message.trim().length > 0) {
    return { message: cause.message, kind: null };
  }
  return { message: fallback, kind: null };
}

export default function TerminalDrawer({
  open,
  onClose,
  selectedProvider,
  selectedModel,
}: TerminalDrawerProps) {
  const { t } = useStrings();
  const [command, setCommand] = useState("");
  const [cwd, setCwd] = useState("");
  const [running, setRunning] = useState(false);
  const [stdout, setStdout] = useState("");
  const [stderr, setStderr] = useState("");
  const [hasRan, setHasRan] = useState(false);
  const [truncated, setTruncated] = useState(false);
  const [killed, setKilled] = useState(false);
  const [lastSuccess, setLastSuccess] = useState<boolean | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [explaining, setExplaining] = useState(false);
  const [explanation, setExplanation] = useState<ErrorExplanation | null>(null);
  const [copied, setCopied] = useState(false);

  const outputRef = useRef<HTMLDivElement | null>(null);
  const cmdInputRef = useRef<HTMLInputElement | null>(null);
  const runningRef = useRef(false);
  const activeRunIdRef = useRef<number | null>(null);
  const receivedChunksRef = useRef(false);
  const cancelledRef = useRef(false);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | null = null;

    void onTerminalOutput((ev: TerminalOutputEvent) => {
      if (!active) return;
      if (!runningRef.current && ev.run_id !== activeRunIdRef.current) {
        return;
      }
      if (activeRunIdRef.current === null) {
        activeRunIdRef.current = ev.run_id;
      } else if (ev.run_id !== activeRunIdRef.current) {
        return;
      }

      if (ev.stream === "stdout" && ev.chunk) {
        receivedChunksRef.current = true;
        setStdout((prev) => {
          const next = appendTerminalChunk(prev, ev.chunk);
          if (next.truncated) setTruncated(true);
          return next.text;
        });
      } else if (ev.stream === "stderr" && ev.chunk) {
        receivedChunksRef.current = true;
        setStderr((prev) => {
          const next = appendTerminalChunk(prev, ev.chunk);
          if (next.truncated) setTruncated(true);
          return next.text;
        });
      }

      if (ev.truncated) setTruncated(true);
      if (ev.killed) setKilled(true);
      if (ev.done) {
        if (ev.success !== null) setLastSuccess(ev.success);
        runningRef.current = false;
        setRunning(false);
      }
    }).then((fn) => {
      if (!active) {
        fn();
      } else {
        unlisten = fn;
      }
    });

    return () => {
      active = false;
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    if (!open) return;
    cmdInputRef.current?.focus();
  }, [open]);

  useEffect(() => {
    const el = outputRef.current;
    if (!el || !open) return;
    el.scrollTop = el.scrollHeight;
  }, [stdout, stderr, open]);

  const handleClear = useCallback(() => {
    if (runningRef.current) return;
    setStdout("");
    setStderr("");
    setHasRan(false);
    setTruncated(false);
    setKilled(false);
    setLastSuccess(null);
    setErrorMsg(null);
    setExplanation(null);
    setCopied(false);
    activeRunIdRef.current = null;
  }, []);

  const handleRun = useCallback(
    async (event?: FormEvent) => {
      event?.preventDefault();
      const trimmed = command.trim();
      if (!trimmed || runningRef.current) return;
      const trimmedCwd = cwd.trim();

      runningRef.current = true;
      activeRunIdRef.current = null;
      receivedChunksRef.current = false;
      cancelledRef.current = false;

      setRunning(true);
      setHasRan(true);
      setStdout("");
      setStderr("");
      setTruncated(false);
      setKilled(false);
      setLastSuccess(null);
      setErrorMsg(null);
      setExplanation(null);
      setCopied(false);

      try {
        const res = await terminalRun(
          trimmed,
          trimmedCwd.length > 0 ? trimmedCwd : null
        );
        activeRunIdRef.current = res.run_id;
        setTruncated((prev) => prev || res.truncated);
        setLastSuccess(res.success);
        if (!receivedChunksRef.current || res.truncated) {
          const parsed = splitCombinedOutput(res.output);
          setStdout(parsed.stdout);
          setStderr(parsed.stderr);
        }
      } catch (cause) {
        const parsed = readErrorMessage(cause, t("term.errorRun"));
        const isKilledErr =
          cancelledRef.current ||
          parsed.kind === "cancelled" ||
          parsed.kind === "timeout";
        if (isKilledErr) {
          setKilled(true);
          setLastSuccess(false);
        } else {
          setErrorMsg(parsed.message);
          setLastSuccess((prev) => (prev === null ? false : prev));
        }
      } finally {
        runningRef.current = false;
        setRunning(false);
      }
    },
    [command, cwd, t]
  );

  const handleKill = useCallback(async () => {
    cancelledRef.current = true;
    try {
      const stopped = await terminalKill();
      if (stopped) {
        setKilled(true);
      }
    } catch (cause) {
      const parsed = readErrorMessage(cause, t("term.errorRun"));
      setErrorMsg(parsed.message);
    }
  }, [t]);

  const combinedOutput = stderr
    ? `${stdout}${stdout && !stdout.endsWith("\n") ? "\n" : ""}--- stderr ---\n${stderr}`
    : stdout;

  const statusText = running
    ? t("term.statusRunning")
    : killed
      ? t("term.statusKilled")
      : lastSuccess === true
        ? t("term.statusSucceeded")
        : lastSuccess === false
          ? t("term.statusFailed")
          : t("term.statusIdle");

  const canExplain =
    !running && lastSuccess === false && combinedOutput.trim().length > 0;

  const handleExplain = useCallback(async () => {
    if (!canExplain || explaining) return;
    if (!selectedProvider || !selectedModel) {
      setErrorMsg(t("term.needProviderExplain"));
      return;
    }
    setExplaining(true);
    setErrorMsg(null);
    try {
      const res = await terminalExplain(
        combinedOutput,
        statusText,
        selectedProvider,
        selectedModel
      );
      setExplanation(res);
      setCopied(false);
    } catch (cause) {
      const parsed = readErrorMessage(cause, t("term.errorRun"));
      setErrorMsg(parsed.message);
    } finally {
      setExplaining(false);
    }
  }, [
    canExplain,
    combinedOutput,
    explaining,
    selectedModel,
    selectedProvider,
    statusText,
    t,
  ]);

  const handleCopyFix = useCallback(async () => {
    if (!explanation?.suggested_fix) return;
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(explanation.suggested_fix);
      }
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }, [explanation]);

  if (!open) return null;

  const statusClass =
    "nex-terminal-drawer-status" +
    (running
      ? " is-running"
      : killed || lastSuccess === false
        ? " is-failed"
        : "");

  return (
    <section
      className="nex-terminal-drawer"
      aria-label={t("term.title")}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          onClose();
        }
      }}
    >
      <header className="nex-terminal-drawer-header">
        <div className="nex-terminal-drawer-title">
          <TerminalIcon size={16} />
          <span>{t("term.title")}</span>
          <span className={statusClass} role="status">
            {running && <LoaderIcon size={16} />}
            {statusText}
          </span>
          {truncated && (
            <span className="nex-terminal-drawer-badge">
              {t("term.truncated")}
            </span>
          )}
        </div>
        <div className="nex-terminal-drawer-actions">
          <IconButton
            size="sm"
            label={t("term.clear")}
            disabled={running || (!stdout && !stderr && !explanation && !errorMsg)}
            onClick={handleClear}
          >
            <TrashIcon size={16} />
          </IconButton>
          <IconButton size="sm" label={t("term.close")} onClick={onClose}>
            <CloseIcon size={16} />
          </IconButton>
        </div>
      </header>

      <form className="nex-terminal-drawer-form" onSubmit={(e) => void handleRun(e)}>
        <input
          ref={cmdInputRef}
          type="text"
          className="nex-input nex-terminal-drawer-cmd"
          aria-label={t("term.cmdLabel")}
          placeholder={t("term.cmdPlaceholder")}
          value={command}
          disabled={running}
          onChange={(e) => setCommand(e.target.value)}
        />
        <input
          type="text"
          className="nex-input nex-terminal-drawer-cwd"
          aria-label={t("term.cwdLabel")}
          placeholder={t("term.cwdPlaceholder")}
          value={cwd}
          disabled={running}
          onChange={(e) => setCwd(e.target.value)}
        />
        {running ? (
          <Button
            type="button"
            variant="secondary"
            size="sm"
            onClick={() => void handleKill()}
          >
            <StopIcon size={16} />
            {t("term.stop")}
          </Button>
        ) : (
          <Button
            type="submit"
            variant="primary"
            size="sm"
            disabled={!command.trim()}
          >
            {t("term.run")}
          </Button>
        )}
        {canExplain && (
          <Button
            type="button"
            variant="quiet"
            size="sm"
            disabled={explaining}
            onClick={() => void handleExplain()}
          >
            {explaining ? t("term.explaining") : t("term.explain")}
          </Button>
        )}
      </form>

      {errorMsg && (
        <div className="nex-terminal-drawer-error" role="alert">
          {errorMsg}
        </div>
      )}

      <div
        ref={outputRef}
        className="nex-terminal-drawer-output"
        role="log"
        aria-label={t("term.outputLabel")}
      >
        {stdout && <pre className="nex-terminal-drawer-stdout">{stdout}</pre>}
        {stderr && (
          <>
            <div className="nex-agent-terminal-separator" aria-hidden="true">
              --- stderr ---
            </div>
            <pre className="nex-terminal-drawer-stderr">{stderr}</pre>
          </>
        )}
        {!stdout && !stderr && (
          <span className="nex-terminal-drawer-empty">
            {running
              ? t("term.running")
              : hasRan
                ? t("term.emptyOutput")
                : t("term.empty")}
          </span>
        )}
      </div>

      {explanation && (
        <div className="nex-terminal-drawer-explain">
          <div className="nex-terminal-drawer-explain-head">
            <span className="nex-terminal-drawer-explain-label">
              {t("term.diagnosis")}
            </span>
          </div>
          <p className="nex-terminal-drawer-explain-text">
            {explanation.explanation}
          </p>
          {explanation.suggested_fix && (
            <div className="nex-terminal-drawer-fix-box">
              <div className="nex-terminal-drawer-explain-head">
                <span className="nex-terminal-drawer-explain-label">
                  {t("term.suggestedFix")}
                </span>
                <Button
                  type="button"
                  variant="quiet"
                  size="sm"
                  onClick={() => void handleCopyFix()}
                >
                  {copied && <CheckIcon size={16} />}
                  {copied ? t("term.copied") : t("term.copyFix")}
                </Button>
              </div>
              <pre className="nex-terminal-drawer-fix">
                {explanation.suggested_fix}
              </pre>
            </div>
          )}
        </div>
      )}
    </section>
  );
}
