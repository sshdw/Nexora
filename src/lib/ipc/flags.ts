//! Feature-flag IPC area (AC-205): the read-only 2.0 rollout gates
//! (commands/flags.rs).

import { invoke } from "./core";

// ---- Feature flags (2.0 rollout) --------------------------------------
// Read-only rollout gates: workspace `.nexora/flags.json` wins over the
// app-global `flags.*` settings keys, which win over the hardcoded defaults
// (current behavior). There is no setter command: flags are edited where
// they live (the workspace file, or the `flags.*` keys via settings).

/** One flag's status in the read-only `flags_status` view. */
export interface FlagStatus {
  enabled: boolean;
  source: "workspace" | "global" | "default";
  /** Whether the run path enforces this flag today (phased rollout:
   * `injection`/`assembly` only; `snapshots`/`self_audit` resolve but gate
   * nothing yet). */
  enforced: boolean;
}

/** Read-only feature-flag status (`flags_status`): every registered flag
 * mapped to its effective value, fixed-vocabulary source, and enforcement
 * mark, plus the workspace-file fallback notice (`null` unless a present
 * workspace flags file failed to load). */
export interface FlagsStatus {
  flags: Record<string, FlagStatus>;
  notice: string | null;
}

export function flagsStatus(): Promise<FlagsStatus> {
  return invoke<FlagsStatus>("flags_status");
}
