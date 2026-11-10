//! Pure label/format helpers for the Activity & Health panel (AC-208 split,
//! per the review §3.1 plan — moved verbatim from `ActivityHealthPanel.tsx`).
//! No React: fixed-vocabulary labels, compact dollar/byte formats, and the
//! shared error classifier.

import type {
  CommandError,
  PrivacySurface,
  ProviderHealth,
} from "../../lib/tauri";
import { getLocale, tr, type Locale } from "../../lib/strings";

/** Fixed-vocabulary run-status label; unknown values fall back to a
 * capitalized echo of the backend token (defensive — the backend only
 * sends its `agent_runs.status` vocabulary). */
export function runStatusLabel(
  status: string,
  locale: Locale = getLocale()
): string {
  switch (status) {
    case "running":
      return tr(locale, "activity.runRunning");
    case "completed":
      return tr(locale, "activity.runCompleted");
    case "cancelled":
      return tr(locale, "activity.runCancelled");
    case "interrupted":
      return tr(locale, "agent.statusInterrupted");
    case "budget_exhausted":
      return tr(locale, "activity.runBudget");
    case "spend_limit_exceeded":
      return tr(locale, "activity.runSpendHit");
    case "error":
      return tr(locale, "activity.runFailed");
    default:
      return status.charAt(0).toUpperCase() + status.slice(1);
  }
}

/** Fixed-vocabulary provider-health label. */
export function healthLabel(
  status: ProviderHealth["status"],
  locale: Locale = getLocale()
): string {
  switch (status) {
    case "healthy":
      return tr(locale, "activity.healthHealthy");
    case "degraded":
      return tr(locale, "activity.healthDegraded");
    case "unreachable":
      return tr(locale, "activity.healthUnreachable");
    case "unknown":
    default:
      return tr(locale, "activity.healthUnknown");
  }
}

/** Compact dollars from micro-USD (`null` = not recorded → "n/a"). */
export function formatMicroUsd(
  micros: number | null,
  locale: Locale = getLocale()
): string {
  if (micros === null) return tr(locale, "common.na");
  const dollars = micros / 1_000_000;
  return `$${dollars >= 1 ? dollars.toFixed(2) : dollars.toFixed(4)}`;
}

/** Human byte size (`null` = unknown → the catalog "unknown"). */
export function formatBytes(
  bytes: number | null,
  locale: Locale = getLocale()
): string {
  if (bytes === null) return tr(locale, "diag.sizeUnknown");
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KB`;
  return `${(kb / 1024).toFixed(1)} MB`;
}

/** Fixed-vocabulary surface-state label. */
export function surfaceStateLabel(
  state: PrivacySurface["state"],
  locale: Locale = getLocale()
): string {
  switch (state) {
    case "on":
      return tr(locale, "privacy.stateOn");
    case "off":
      return tr(locale, "privacy.stateOff");
    case "manual":
      return tr(locale, "privacy.stateManual");
    case "local":
    default:
      return tr(locale, "privacy.stateLocal");
  }
}

/** Fixed-vocabulary ledger-kind label; unknown kinds echo defensively. */
export function ledgerKindLabel(
  kind: string,
  locale: Locale = getLocale()
): string {
  switch (kind) {
    case "agent_run":
      return tr(locale, "privacy.kind_agent_run");
    case "message":
      return tr(locale, "privacy.kind_message");
    case "github_read":
      return tr(locale, "privacy.kind_github_read");
    case "update_check":
      return tr(locale, "privacy.kind_update_check");
    default:
      return kind;
  }
}

export function toMessage(error: unknown): string {
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
