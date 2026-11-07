//! Debt-backlog IPC area (AC-205): manual entries plus idempotent audit
//! imports behind the Debt panel (commands/debt.rs).

import { invoke } from "./core";

// ---- Debt backlog (manual entries + idempotent audit imports) -----------
// Five commands behind the Debt panel: `create_debt_item` adds one manual
// entry; `list_debt_items` returns every row (most recently touched first);
// `update_debt_item_status` moves one row through the triage lifecycle
// (`open` / `accepted` / `fixed` / `wontfix`); `delete_debt_item` removes
// one row; `import_debt_from_audit` runs the read-only audit over the
// workspace and inserts fresh findings as rows (re-runs insert nothing new).
// NOTE: command ARGS are camelCase (Tauri v2); payloads stay snake_case —
// never align one to the other.

/** One `debt_items` row as persisted (v10 migration). */
export interface DebtItem {
  id: number;
  title: string;
  source: "audit" | "manual";
  severity: "info" | "warning";
  status: "open" | "accepted" | "fixed" | "wontfix";
  location: string | null;
  note: string | null;
  audit_key: string | null;
  created_at: number; // seconds since unix epoch
  updated_at: number; // seconds since unix epoch
}

/** Add one manually tracked debt entry. Returns the schema-assigned id. */
export function createDebtItem(
  title: string,
  severity: string,
  location: string | null,
  note: string | null,
): Promise<number> {
  return invoke<number>("create_debt_item", {
    title,
    severity,
    location: location ?? null,
    note: note ?? null,
  });
}

/** List every debt item, most recently touched first. */
export function listDebtItems(): Promise<DebtItem[]> {
  return invoke<DebtItem[]>("list_debt_items");
}

/** Move one debt item to a new triage status. */
export function updateDebtItemStatus(debtId: number, status: string): Promise<void> {
  return invoke<void>("update_debt_item_status", { debtId, status });
}

/** Delete one debt item by id (a no-op when the id is unknown). */
export function deleteDebtItem(debtId: number): Promise<void> {
  return invoke<void>("delete_debt_item", { debtId });
}

/** Outcome of one audit import: newly inserted rows plus rows skipped as
 * unimportable (`inserted == 0 && skipped == 0` means already tracked). */
export interface DebtImportResult {
  inserted: number;
  skipped: number;
}

/** Import current audit findings as debt rows. Returns inserted + skipped
 * counts (0 + 0 when everything was already tracked). */
export function importDebtFromAudit(): Promise<DebtImportResult> {
  return invoke<DebtImportResult>("import_debt_from_audit");
}
