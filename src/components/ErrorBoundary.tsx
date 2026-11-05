//! React error boundaries (ADD-05, review spec 05-ADD-FOR-2.0.0.md §3.5a):
//! before this component the tree had zero boundaries (grep: 0 hits for
//! `ErrorBoundary`/`componentDidCatch`/`getDerivedStateFromError`) and the
//! bare root render at main.tsx:40 meant one render crash anywhere unmounted
//! the whole app to a blank window with no recovery path.
//!
//! One class boundary, three wiring levels:
//!   - root    — wraps `<App/>` in main.tsx; a shell-level crash degrades to
//!               a centred last-resort screen instead of a blank window.
//!   - dock    — wraps each DockPanel slot's children; one panel crashing
//!               leaves the panel shell (title, close, dismissal) alive.
//!   - thread  — wraps the conversation subtree in ConversationPane, the
//!               most dynamic surface.
//!
//! The fallback is deliberately MUTED, per the work-package pin: surface-1
//! (`--card`), hairline seam (`--border`, THEME §2), text-2 ink — a boundary
//! means "this slot broke", not "your data is wrong", so it does NOT ride
//! the M3E error-token strip (§3.5a names that strip only as the precedent
//! for where a fallback may render; the muted variant is the ask). Every
//! value is a token; class component because boundaries are class-idiomatic
//! (React 19 still supports getDerivedStateFromError/componentDidCatch).

import { Component } from "react";
import type { ErrorInfo, ReactNode } from "react";

import { useStrings } from "../lib/useLocale";
import M3Button from "./M3Button";

export interface ErrorBoundaryProps {
  children: ReactNode;
  /** Root wiring: the fallback gets the whole window (centred card on the
   *  canvas) instead of flowing inside a slot. */
  root?: boolean;
}

interface ErrorBoundaryState {
  /** The caught render error; null = subtree healthy. Kept (not logged to
   *  any store) so Retry can reset to a clean subtree. */
  error: Error | null;
}

/** The fallback UI is a function component so the copy comes from the same
 *  catalog as every other string (useStrings), which a class cannot call. */
function BoundaryFallback({
  onRetry,
  root,
}: {
  onRetry: () => void;
  root: boolean;
}) {
  const { t } = useStrings();
  const card = (
    <div className="nex-error-boundary" role="alert">
      <p className="nex-error-boundary-text">{t("err.boundaryBody")}</p>
      <M3Button variant="quiet" onClick={onRetry}>
        {t("common.retry")}
      </M3Button>
    </div>
  );
  if (!root) return card;
  return <div className="nex-error-boundary-root">{card}</div>;
}

export default class ErrorBoundary extends Component<
  ErrorBoundaryProps,
  ErrorBoundaryState
> {
  state: ErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    // Local diagnostics only — the webview console, nothing persisted, no
    // IPC, and no message content copied anywhere. Resetting is the only
    // recovery this component offers (the ask's Retry action).
    console.error("[nexora boundary] render error:", error, info.componentStack);
  }

  /** Retry: forget the error and re-render the same children. The crashing
   *  subtree remounts from its own state — for transient render faults this
   *  recovers in place, and a persistently crashing child just lands back
   *  here, which is the honest outcome. */
  private reset = (): void => {
    this.setState({ error: null });
  };

  render(): ReactNode {
    if (this.state.error !== null) {
      return <BoundaryFallback onRetry={this.reset} root={this.props.root ?? false} />;
    }
    return this.props.children;
  }
}
