//! Simplified first-run onboarding (3-step overlay).
//!
//! Flow: 1. provider key connect → 2. workspace folder pick → 3. done
//! (pointers to Ctrl+K palette + shortcuts dialog). Each step reuses the
//! existing flows, never reimplements them: step 1 drives
//! `ProvidersStore.connect` / `selectProvider` / `selectModel` (the same
//! keyring-backed path as SettingsView credentials, SettingsView.tsx);
//! step 2 drives `WorkspaceStore.pickFolder` / `selectRecent` (the same
//! backend-persisted path as the sidebar picker, useWorkspace.ts).
//!
//! M3E mapping: ModalShell owns Esc + focus trap/return (Modal.tsx
//! precedent — shortcut:dialog.close, shortcut:dialog.trap); enter motion
//! reuses .nex-pop-enter (components.css); one primary per step (Continue
//! / Get started); all visuals are --nex-sys-* / --motion-* tokens
//! (onboarding.css) — zero raw values; reduced motion resolves via the
//! global motion.css gate.
//!
//! Never blocks: every step is skippable (Continue without acting), the
//! whole flow dismisses via Esc/backdrop/"Not now", and any close marks
//! completion (useOnboarding) so the app stays usable with zero keys —
//! the composer unconfigured notice (ConversationView.tsx) is untouched.

import { useEffect, useState } from "react";

import { isCustomModelId, type ProvidersStore } from "../lib/useProviders";
import type { WorkspaceStore } from "../lib/useWorkspace";
import M3Button from "./M3Button";
import ModalShell from "./Modal";

export interface OnboardingFlowProps {
  /** Shared provider store (App-lifted single source). */
  providers: ProvidersStore;
  /** Shared workspace store (App-lifted single source). */
  workspace: WorkspaceStore;
  /** Dismiss the flow (marks completion — Esc/backdrop/Not now/Finish). */
  onClose: () => void;
  /** Dismiss the flow and open Settings (deep-link, no dialog stacking). */
  onOpenSettings: () => void;
}

const STEP_TITLES = [
  "Connect a provider",
  "Pick a workspace folder",
  "You're set",
] as const;

export default function OnboardingFlow({
  providers,
  workspace,
  onClose,
  onOpenSettings,
}: OnboardingFlowProps) {
  const [step, setStep] = useState(0);
  const [providerName, setProviderName] = useState(
    providers.selectedProvider ??
      providers.providers[0]?.supported.name ??
      "",
  );
  const [apiKey, setApiKey] = useState("");
  const [connecting, setConnecting] = useState(false);
  const [connectDone, setConnectDone] = useState(false);
  // Until the user picks a provider manually, follow the persisted
  // selection / first available provider so the dropdown fills in once
  // the provider list loads after mount (replay-after-load).
  const [manualPick, setManualPick] = useState(false);
  useEffect(() => {
    if (manualPick) return;
    const next =
      providers.selectedProvider ??
      providers.providers[0]?.supported.name ??
      "";
    setProviderName((current) => (current === next ? current : next));
  }, [providers.selectedProvider, providers.providers, manualPick]);

  const activeProvider = providers.providers.find(
    (p) => p.supported.name === providerName,
  );
  const connectedName = providers.providers.find((p) => p.available)
    ?.supported.display_name;

  const handleConnect = async () => {
    const credential = apiKey.trim();
    if (!credential || !activeProvider || connecting) return;
    setConnecting(true);
    try {
      const ok = await providers.connect(
        activeProvider.supported.name,
        activeProvider.supported.display_name,
        credential,
      );
      if (!ok) return; // Error surfaces via providers.error; keep input.
      setApiKey("");
      setConnectDone(true);
      // Make the connected provider the active selection (same defaulting
      // as SettingsView provider change: keep a valid custom model,
      // otherwise persist the provider default).
      const keepCustom = providers.selectedModel
        ? isCustomModelId(providers.selectedModel)
        : false;
      await providers.selectProvider(activeProvider.supported.name);
      if (!keepCustom && activeProvider.supported.models.length > 0) {
        await providers.selectModel(activeProvider.supported.models[0]);
      }
    } finally {
      setConnecting(false);
    }
  };

  const next = () => setStep((s) => Math.min(s + 1, STEP_TITLES.length - 1));
  const back = () => setStep((s) => Math.max(s - 1, 0));

  return (
    <ModalShell
      title="Welcome to Nexora"
      busy={connecting || workspace.saving}
      onClose={onClose}
    >
      <div className="nex-onboarding nex-pop-enter">
        <ol className="nex-onboarding-steps" aria-label="Onboarding progress">
          {STEP_TITLES.map((title, index) => (
            <li
              key={title}
              className={
                "nex-onboarding-step" +
                (index === step ? " is-current" : "") +
                (index < step ? " is-done" : "")
              }
              aria-current={index === step ? "step" : undefined}
            >
              <span className="nex-onboarding-dot" aria-hidden="true" />
              <span className="nex-sr-only">
                {`Step ${index + 1} of ${STEP_TITLES.length}: ${title}${
                  index === step ? " (current)" : index < step ? " (done)" : ""
                }`}
              </span>
            </li>
          ))}
        </ol>
        <p className="nex-onboarding-counter" aria-hidden="true">
          Step {step + 1} of {STEP_TITLES.length}
        </p>

        <div
          role="region"
          aria-label={`Step ${step + 1} of ${STEP_TITLES.length}: ${STEP_TITLES[step]}`}
        >
          {step === 0 && (
            <section
              className="nex-onboarding-pane"
              aria-labelledby="nex-onboarding-provider-heading"
            >
              <h4
                id="nex-onboarding-provider-heading"
                className="nex-onboarding-heading"
              >
                Connect a provider
              </h4>
              <p className="nex-settings-hint">
                Add one API key to start chatting. Keys stay in your
                system&rsquo;s secure keyring, never in the database. You
                can skip this — Nexora stays usable without any key.
              </p>
              {providers.providers.length === 0 ? (
                <p className="nex-settings-hint" role="status">
                  No providers are available in this build yet.
                </p>
              ) : (
                <form
                  className="nex-onboarding-field"
                  onSubmit={(event) => {
                    event.preventDefault();
                    void handleConnect();
                  }}
                >
                  <label
                    className="nex-settings-label"
                    htmlFor="nex-onboarding-provider"
                  >
                    Provider
                  </label>
                  <select
                    id="nex-onboarding-provider"
                    className="nex-select"
                    value={providerName}
                    disabled={connecting}
                    onChange={(event) => {
                      setManualPick(true);
                      setProviderName(event.target.value);
                      setConnectDone(false);
                    }}
                  >
                    {providers.providers.map(({ supported, available }) => (
                      <option key={supported.name} value={supported.name}>
                        {supported.display_name}
                        {available ? " · Connected" : ""}
                      </option>
                    ))}
                  </select>
                  <label
                    className="nex-settings-label"
                    htmlFor="nex-onboarding-key"
                  >
                    API key
                  </label>
                  <input
                    id="nex-onboarding-key"
                    className="nex-input"
                    type="password"
                    autoComplete="new-password"
                    placeholder="Paste your API key"
                    aria-label={`${activeProvider?.supported.display_name ?? "Provider"} API key`}
                    value={apiKey}
                    disabled={connecting}
                    onChange={(event) => setApiKey(event.target.value)}
                  />
                  <div className="nex-onboarding-inline">
                    <M3Button
                      variant="secondary"
                      size="sm"
                      type="submit"
                      loading={connecting}
                      disabled={!apiKey.trim() || connecting}
                    >
                      {activeProvider?.credentialed ? "Update key" : "Connect"}
                    </M3Button>
                    {(connectDone || activeProvider?.available) && (
                      <span
                        className="nex-onboarding-status"
                        role="status"
                      >
                        Connected{connectedName ? ` · ${connectedName}` : ""}.
                      </span>
                    )}
                  </div>
                  {providers.error && (
                    <p
                      className="nex-settings-error nex-fade-in"
                      role="alert"
                    >
                      {providers.error.message}
                    </p>
                  )}
                </form>
              )}
            </section>
          )}

          {step === 1 && (
            <section
              className="nex-onboarding-pane"
              aria-labelledby="nex-onboarding-workspace-heading"
            >
              <h4
                id="nex-onboarding-workspace-heading"
                className="nex-onboarding-heading"
              >
                Pick a workspace folder
              </h4>
              <p className="nex-settings-hint">
                The folder the agent&rsquo;s tools are scoped to. You can
                change it any time from the sidebar.
              </p>
              <p className="nex-onboarding-current" role="status">
                {workspace.loading
                  ? "Loading…"
                  : (workspace.root ?? "No folder chosen yet.")}
              </p>
              <div className="nex-onboarding-inline">
                <M3Button
                  variant="secondary"
                  size="sm"
                  loading={workspace.saving}
                  disabled={workspace.saving}
                  onClick={() => void workspace.pickFolder()}
                >
                  Choose folder…
                </M3Button>
              </div>
              {workspace.error && (
                <p className="nex-settings-error nex-fade-in" role="alert">
                  {workspace.error.message}
                </p>
              )}
              {workspace.recent.length > 0 && (
                <div className="nex-onboarding-field">
                  <span
                    className="nex-settings-label"
                    id="nex-onboarding-recent-label"
                  >
                    Recent folders
                  </span>
                  <ul
                    className="nex-onboarding-recent"
                    aria-labelledby="nex-onboarding-recent-label"
                  >
                    {workspace.recent.map((path) => (
                      <li key={path}>
                        <button
                          type="button"
                          className="nex-onboarding-recent-item"
                          title={path}
                          disabled={
                            workspace.saving || path === workspace.root
                          }
                          onClick={() => void workspace.selectRecent(path)}
                        >
                          {path === workspace.root ? `● ${path}` : path}
                        </button>
                      </li>
                    ))}
                  </ul>
                </div>
              )}
            </section>
          )}

          {step === 2 && (
            <section
              className="nex-onboarding-pane"
              aria-labelledby="nex-onboarding-done-heading"
            >
              <h4
                id="nex-onboarding-done-heading"
                className="nex-onboarding-heading"
              >
                You&rsquo;re set
              </h4>
              <p className="nex-settings-hint">
                {connectedName ?? workspace.root
                  ? [
                      connectedName
                        ? `Provider: ${connectedName}`
                        : "No provider connected yet",
                      workspace.root
                        ? `Workspace: ${workspace.root}`
                        : "No workspace folder yet",
                    ].join(" · ")
                  : "Skipped setup — you can do it any time from Settings."}
              </p>
              <ul className="nex-onboarding-tips">
                <li>
                  Press <kbd>Ctrl</kbd> + <kbd>K</kbd> to open the command
                  palette — every action lives there.
                </li>
                <li>
                  Press <kbd>Ctrl</kbd> + <kbd>/</kbd> (or <kbd>F1</kbd>) to
                  see all keyboard shortcuts.
                </li>
              </ul>
            </section>
          )}
        </div>

        <div className="nex-onboarding-actions">
          {step > 0 ? (
            <M3Button variant="quiet" size="sm" onClick={back}>
              Back
            </M3Button>
          ) : (
            <M3Button variant="quiet" size="sm" onClick={onClose}>
              Not now
            </M3Button>
          )}
          <span className="nex-onboarding-spacer" aria-hidden="true" />
          {step < STEP_TITLES.length - 1 ? (
            <>
              <M3Button variant="quiet" size="sm" onClick={next}>
                Skip this step
              </M3Button>
              <M3Button variant="primary" size="sm" onClick={next}>
                Continue
              </M3Button>
            </>
          ) : (
            <>
              <M3Button variant="quiet" size="sm" onClick={onOpenSettings}>
                Open Settings
              </M3Button>
              <M3Button variant="primary" size="sm" onClick={onClose}>
                Get started
              </M3Button>
            </>
          )}
        </div>
      </div>
    </ModalShell>
  );
}
