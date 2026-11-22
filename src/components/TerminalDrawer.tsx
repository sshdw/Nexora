import { useCallback, useEffect, useRef, useState } from "react";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal, type ITheme } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import {
  onTerminalPtyExit,
  onTerminalPtyOutput,
  terminalPtyKill,
  terminalPtyResize,
  terminalPtySpawn,
  terminalPtyWrite,
  type PtySessionInfo,
  type TerminalPtyExitEvent,
  type TerminalPtyOutputEvent,
} from "../lib/tauri";
import { useStrings } from "../lib/useLocale";
import Button from "./Button";
import IconButton from "./IconButton";
import { CloseIcon, TerminalIcon } from "./icons";

export interface TerminalDrawerProps {
  open: boolean;
  onClose: () => void;
  selectedProvider?: string | null;
  selectedModel?: string | null;
}

const DEFAULT_COLS = 80;
const DEFAULT_ROWS = 24;
const TERMINAL_FONT_FAMILY = '"Cascadia Code", Consolas, monospace';
const TERMINAL_FONT_SIZE = 13;

function readCssVar(
  style: CSSStyleDeclaration,
  name: string
): string | undefined {
  const value = style.getPropertyValue(name).trim();
  return value.length > 0 ? value : undefined;
}

function readTerminalTheme(): ITheme {
  if (typeof window === "undefined" || typeof document === "undefined") {
    return {};
  }
  const style = window.getComputedStyle(document.documentElement);
  const bg = readCssVar(style, "--color-surface-container-lowest");
  const fg = readCssVar(style, "--color-on-surface");
  const muted = readCssVar(style, "--color-on-surface-variant");
  const tertiary = readCssVar(style, "--color-on-surface-tertiary");
  const faint = readCssVar(style, "--color-on-surface-faint");
  const selection = readCssVar(style, "--state-selected");
  const scrollThumb = readCssVar(style, "--scrollbar-thumb");
  const scrollThumbHover = readCssVar(style, "--scrollbar-thumb-hover");
  const err = readCssVar(style, "--color-error");
  const ok = readCssVar(style, "--color-success");

  const theme: ITheme = {};
  if (bg) {
    theme.background = bg;
    theme.cursorAccent = bg;
    theme.black = bg;
  }
  if (fg) {
    theme.foreground = fg;
    theme.cursor = fg;
    theme.white = fg;
    theme.brightWhite = fg;
  }
  if (faint) theme.brightBlack = faint;
  if (err) {
    theme.red = err;
    theme.brightRed = err;
  }
  if (ok) {
    theme.green = ok;
    theme.brightGreen = ok;
  }
  if (muted) {
    theme.yellow = muted;
    theme.brightYellow = muted;
    theme.blue = muted;
    theme.brightBlue = muted;
    theme.cyan = muted;
    theme.brightCyan = muted;
  }
  if (tertiary) {
    theme.magenta = tertiary;
    theme.brightMagenta = tertiary;
  }
  if (selection) theme.selectionBackground = selection;
  if (scrollThumb) theme.scrollbarSliderBackground = scrollThumb;
  if (scrollThumbHover) {
    theme.scrollbarSliderHoverBackground = scrollThumbHover;
    theme.scrollbarSliderActiveBackground = scrollThumbHover;
  }
  return theme;
}

function prefersReducedMotion(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return false;
  }
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

function readErrorMessage(cause: unknown, fallback: string): string {
  if (typeof cause === "object" && cause !== null) {
    const rec = cause as { message?: unknown };
    if (typeof rec.message === "string" && rec.message.trim().length > 0) {
      return rec.message;
    }
  }
  if (cause instanceof Error && cause.message.trim().length > 0) {
    return cause.message;
  }
  return fallback;
}

export default function TerminalDrawer({ open, onClose }: TerminalDrawerProps) {
  const { t } = useStrings();
  const [sessionInfo, setSessionInfo] = useState<PtySessionInfo | null>(null);
  const [starting, setStarting] = useState(false);
  const [exitedCode, setExitedCode] = useState<number | null | undefined>(
    undefined
  );
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const viewportRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const sessionIdRef = useRef<string | null>(null);
  const lastSizeRef = useRef<{ cols: number; rows: number }>({
    cols: DEFAULT_COLS,
    rows: DEFAULT_ROWS,
  });
  const spawnSeqRef = useRef(0);
  const disposedRef = useRef(true);
  const pendingOutputRef = useRef<TerminalPtyOutputEvent[]>([]);
  const pendingExitRef = useRef<TerminalPtyExitEvent[]>([]);
  const errorSpawnRef = useRef(t("term.errorSpawn"));
  errorSpawnRef.current = t("term.errorSpawn");

  const fitTerminal = useCallback((): { cols: number; rows: number } => {
    const term = termRef.current;
    const fitAddon = fitAddonRef.current;
    if (fitAddon) {
      try {
        fitAddon.fit();
      } catch {
        // Non-visual layout (e.g. jsdom or zero-width transition frame).
      }
    }
    const cols =
      term && Number.isFinite(term.cols) && term.cols > 0
        ? term.cols
        : DEFAULT_COLS;
    const rows =
      term && Number.isFinite(term.rows) && term.rows > 0
        ? term.rows
        : DEFAULT_ROWS;
    return { cols, rows };
  }, []);

  const syncPtySize = useCallback(
    (force = false) => {
      const { cols, rows } = fitTerminal();
      const prev = lastSizeRef.current;
      const changed = prev.cols !== cols || prev.rows !== rows;
      lastSizeRef.current = { cols, rows };
      const sid = sessionIdRef.current;
      if (sid && (force || changed)) {
        void terminalPtyResize(sid, cols, rows).catch(() => {});
      }
    },
    [fitTerminal]
  );

  const startSession = useCallback(async () => {
    const mySeq = ++spawnSeqRef.current;
    const prevSid = sessionIdRef.current;
    sessionIdRef.current = null;
    pendingOutputRef.current = [];
    pendingExitRef.current = [];
    setErrorMsg(null);
    setExitedCode(undefined);
    setStarting(true);

    if (prevSid) {
      await terminalPtyKill(prevSid).catch(() => {});
    }
    if (disposedRef.current || spawnSeqRef.current !== mySeq) return;

    const { cols, rows } = fitTerminal();
    lastSizeRef.current = { cols, rows };

    try {
      const info = await terminalPtySpawn(cols, rows);
      if (disposedRef.current || spawnSeqRef.current !== mySeq) {
        void terminalPtyKill(info.session_id).catch(() => {});
        return;
      }

      sessionIdRef.current = info.session_id;
      setSessionInfo(info);
      setStarting(false);

      const term = termRef.current;
      const queuedOutput = pendingOutputRef.current;
      pendingOutputRef.current = [];
      for (const ev of queuedOutput) {
        if (ev.session_id === info.session_id) {
          term?.write(ev.data);
        }
      }

      const queuedExits = pendingExitRef.current;
      pendingExitRef.current = [];
      const matchingExit = queuedExits.find(
        (ev) => ev.session_id === info.session_id
      );
      if (matchingExit) {
        sessionIdRef.current = null;
        setExitedCode(matchingExit.exit_code);
      } else {
        syncPtySize(false);
      }

      term?.focus();
    } catch (cause) {
      if (disposedRef.current || spawnSeqRef.current !== mySeq) return;
      setStarting(false);
      setSessionInfo(null);
      setErrorMsg(readErrorMessage(cause, errorSpawnRef.current));
    }
  }, [fitTerminal, syncPtySize]);

  useEffect(() => {
    if (!open) return;
    const container = viewportRef.current;
    if (!container) return;

    disposedRef.current = false;

    const term = new Terminal({
      fontFamily: TERMINAL_FONT_FAMILY,
      fontSize: TERMINAL_FONT_SIZE,
      cursorBlink: !prefersReducedMotion(),
      convertEol: false,
      scrollback: 5000,
      theme: readTerminalTheme(),
    });
    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(container);
    termRef.current = term;
    fitAddonRef.current = fitAddon;

    const dataDisposable = term.onData((data) => {
      const sid = sessionIdRef.current;
      if (sid) {
        void terminalPtyWrite(sid, data).catch(() => {});
      }
    });

    let resizeObserver: ResizeObserver | null = null;
    if (typeof ResizeObserver !== "undefined") {
      resizeObserver = new ResizeObserver(() => {
        if (!disposedRef.current) {
          syncPtySize(false);
        }
      });
      resizeObserver.observe(container);
    }

    let themeObserver: MutationObserver | null = null;
    if (typeof MutationObserver !== "undefined") {
      themeObserver = new MutationObserver(() => {
        if (!disposedRef.current && termRef.current) {
          termRef.current.options.theme = readTerminalTheme();
        }
      });
      themeObserver.observe(document.documentElement, {
        attributes: true,
        attributeFilter: ["data-theme"],
      });
    }

    let unlistenOutput: (() => void) | null = null;
    let unlistenExit: (() => void) | null = null;

    void Promise.all([
      onTerminalPtyOutput((ev) => {
        if (disposedRef.current) return;
        const activeSid = sessionIdRef.current;
        if (activeSid === null) {
          pendingOutputRef.current.push(ev);
          return;
        }
        if (ev.session_id === activeSid) {
          term.write(ev.data);
        }
      }),
      onTerminalPtyExit((ev) => {
        if (disposedRef.current) return;
        const activeSid = sessionIdRef.current;
        if (activeSid === null) {
          pendingExitRef.current.push(ev);
          return;
        }
        if (ev.session_id === activeSid) {
          sessionIdRef.current = null;
          setExitedCode(ev.exit_code);
        }
      }),
    ]).then(([offOut, offExit]) => {
      if (disposedRef.current) {
        offOut();
        offExit();
        return;
      }
      unlistenOutput = offOut;
      unlistenExit = offExit;
      void startSession();
    });

    return () => {
      disposedRef.current = true;
      spawnSeqRef.current += 1;
      const activeSid = sessionIdRef.current;
      sessionIdRef.current = null;
      pendingOutputRef.current = [];
      pendingExitRef.current = [];
      if (activeSid) {
        void terminalPtyKill(activeSid).catch(() => {});
      }
      if (unlistenOutput) unlistenOutput();
      if (unlistenExit) unlistenExit();
      resizeObserver?.disconnect();
      themeObserver?.disconnect();
      dataDisposable.dispose();
      term.dispose();
      termRef.current = null;
      fitAddonRef.current = null;
      setSessionInfo(null);
      setStarting(false);
      setExitedCode(undefined);
      setErrorMsg(null);
    };
  }, [open, startSession, syncPtySize]);

  const handleClear = useCallback(() => {
    termRef.current?.clear();
    termRef.current?.focus();
  }, []);

  const handleRestart = useCallback(() => {
    termRef.current?.reset();
    void startSession();
  }, [startSession]);

  if (!open) return null;

  const metaLabel = sessionInfo
    ? `${sessionInfo.shell_name} · ${sessionInfo.cwd}`
    : starting
      ? t("term.starting")
      : null;

  const exitedLabel =
    exitedCode !== undefined
      ? exitedCode === null
        ? t("term.exited")
        : t("term.exitedCode").replace("{code}", String(exitedCode))
      : null;

  return (
    <section className="nex-terminal-drawer" aria-label={t("term.title")}>
      <header className="nex-terminal-drawer-header">
        <div className="nex-terminal-drawer-title">
          <TerminalIcon size={16} />
          <span>{t("term.title")}</span>
          {metaLabel && (
            <span
              className="nex-terminal-drawer-meta"
              title={sessionInfo?.cwd ?? undefined}
            >
              {metaLabel}
            </span>
          )}
          {exitedLabel && (
            <span className="nex-terminal-drawer-status is-exited" role="status">
              {exitedLabel}
            </span>
          )}
        </div>
        <div className="nex-terminal-drawer-actions">
          <Button
            type="button"
            variant="quiet"
            size="sm"
            onClick={handleClear}
          >
            {t("term.clear")}
          </Button>
          <Button
            type="button"
            variant="quiet"
            size="sm"
            disabled={starting}
            onClick={handleRestart}
          >
            {t("term.restart")}
          </Button>
          <IconButton size="sm" label={t("term.close")} onClick={onClose}>
            <CloseIcon size={16} />
          </IconButton>
        </div>
      </header>

      {errorMsg && (
        <div className="nex-terminal-drawer-error" role="alert">
          {errorMsg}
        </div>
      )}

      <div
        ref={viewportRef}
        className="nex-terminal-drawer-viewport"
        role="region"
        aria-label={t("term.viewportLabel")}
        onClick={() => termRef.current?.focus()}
      />
    </section>
  );
}
