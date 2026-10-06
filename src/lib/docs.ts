//! In-app docs content model (Docs panel): the curated section inventory +
//! the feature list with honest backend anchors.
//!
//! Honesty rule: every feature entry names the palette command id that opens
//! the feature (verified against the `buildCommands` registry in
//! commands.ts — invoking the id IS the feature, so the list cannot name
//! anything that does not exist). Every migration version renders from the
//! backend `docs_manifest` spine; the per-version notes live in the string
//! catalog (`docs.migN`) with a bare `vN` fallback, so prose can lag but the
//! version list never can. Shortcuts render straight from the SHORTCUTS
//! registry (like ShortcutsDialog) — no hand-copied combos here.

import type { StringKey } from "./strings";

/** Docs sections in nav order. No FAQ: only sections with honest,
// * code-backed content ship (placeholders are out of scope). */
export type DocsSectionId = "start" | "features" | "migration" | "shortcuts";

export const DOC_SECTIONS: readonly DocsSectionId[] = [
  "start",
  "features",
  "migration",
  "shortcuts",
];

/** One documented feature: the palette command that opens it plus the
 * catalog keys for its title and blurb. `paletteId` must be an id produced
 * by `buildCommands` (commands.ts) — grep-verified at authoring time. */
export interface DocsFeature {
  /** Stable palette command id (e.g. "go.tasks"). */
  paletteId: string;
  titleKey: StringKey;
  bodyKey: StringKey;
}

// Feature inventory (12): one row per workspace surface. Blurbs describe
// only shipped behavior with its backend anchor in parentheses for the
// reviewer (anchors: the Tauri command each surface is presentational over).
export const DOC_FEATURES: readonly DocsFeature[] = [
  { paletteId: "chat.new", titleKey: "docs.featChat", bodyKey: "docs.featChatBody" },
  { paletteId: "go.library", titleKey: "docs.featLibrary", bodyKey: "docs.featLibraryBody" },
  { paletteId: "go.vcs", titleKey: "docs.featVcs", bodyKey: "docs.featVcsBody" },
  { paletteId: "go.terminal", titleKey: "docs.featTerminal", bodyKey: "docs.featTerminalBody" },
  { paletteId: "go.tasks", titleKey: "docs.featTasks", bodyKey: "docs.featTasksBody" },
  { paletteId: "go.audit", titleKey: "docs.featAudit", bodyKey: "docs.featAuditBody" },
  { paletteId: "go.debt", titleKey: "docs.featDebt", bodyKey: "docs.featDebtBody" },
  { paletteId: "go.gh", titleKey: "docs.featGh", bodyKey: "docs.featGhBody" },
  { paletteId: "go.activity", titleKey: "docs.featActivity", bodyKey: "docs.featActivityBody" },
  { paletteId: "go.health", titleKey: "docs.featHealth", bodyKey: "docs.featHealthBody" },
  { paletteId: "go.privacy", titleKey: "docs.featPrivacy", bodyKey: "docs.featPrivacyBody" },
  { paletteId: "go.diagnostics", titleKey: "docs.featDiag", bodyKey: "docs.featDiagBody" },
];

/** Catalog key for a migration version's note (`docs.mig1` … `docs.migN`).
 * Unknown versions fall back to a bare `vN` label in the panel, so a new
 * backend migration never blanks the guide before its note lands. */
export function migrationNoteKey(version: number): StringKey {
  return `docs.mig${version}` as StringKey;
}
