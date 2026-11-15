import { invoke } from "./core";

export interface SandboxStatus {
  workspace_root: string;
  state: "verified" | "unavailable" | "failed" | "stale";
  posture: "Verified" | "PerCommandConsent";
  mechanism: string | null;
  reason: string | null;
}

/** Read-only server status for the active workspace; no client posture inputs. */
export function getSandboxStatus(): Promise<SandboxStatus> {
  return invoke("get_sandbox_status");
}
