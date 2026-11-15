import { useEffect, useState } from "react";
import { getSandboxStatus, type SandboxStatus } from "../lib/tauri";
import { tr, type Locale } from "../lib/strings";

export default function SandboxStatusBadge({ workspaceRoot, locale }: { workspaceRoot: string | null; locale: Locale }) {
  const [result, setResult] = useState<{ workspace: string | null; status: SandboxStatus | null; failed: boolean } | null>(null);
  useEffect(() => {
    let active = true;
    const refresh = async () => {
      try {
        const status = await getSandboxStatus();
        if (active) setResult({ workspace: workspaceRoot, status, failed: false });
      } catch {
        if (active) setResult({ workspace: workspaceRoot, status: null, failed: true });
      }
    };
    void refresh();
    const timer = window.setInterval(() => void refresh(), 5000);
    return () => { active = false; window.clearInterval(timer); };
  }, [workspaceRoot]);
  const current = result?.workspace === workspaceRoot ? result : null;
  const verified = current?.status?.workspace_root === workspaceRoot && current?.status?.state === "verified" && current.status.posture === "Verified";
  const label = !current ? "sandbox.checking" : verified ? "sandbox.verified" : "sandbox.consent";
  return (
    <div className="nex-settings-hint" role="status" aria-label={tr(locale, "sandbox.label")} aria-atomic="true">
      <strong>{tr(locale, "sandbox.label")}: {tr(locale, label)}</strong>
      <p>{tr(locale, verified ? "sandbox.hint" : current?.failed ? "sandbox.unavailable" : "sandbox.failed")}</p>
      {current?.status?.reason && <p>{current.status.reason}</p>}
      {verified && current?.status?.mechanism && <span>{current.status.mechanism}</span>}
    </div>
  );
}
