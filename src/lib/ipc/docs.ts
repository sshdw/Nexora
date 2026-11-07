//! In-app docs IPC area (AC-205): the read-only migration-guide manifest
//! (commands/docs.rs).

import { invoke } from "./core";

// ---- In-app docs (migration guide backbone) ---------------------------
// Read-only manifest behind the Docs panel: the ordered migration spine
// computed backend-side from the registered migrations (commands/docs.rs),
// so the guide lists exactly the migrations that exist. Payloads stay
// snake_case like every other backend struct. The per-version notes render
// frontend-side from the string catalog (`docs.migN`).

/** One schema milestone in the migration guide: the migration version only. */
export interface DocsMigration {
  version: number;
}

/** Docs manifest (`docs_manifest`): highest known schema version plus the
 * full ordered migration spine. */
export interface DocsManifest {
  schema_version: number;
  migrations: DocsMigration[];
}

export function docsManifest(): Promise<DocsManifest> {
  return invoke<DocsManifest>("docs_manifest");
}
