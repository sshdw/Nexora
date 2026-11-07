//! Activity & health panel: read-only surfaces over data the backends
//! already record — no new collection, no writes, no polling.
//!
//! ONE overlay area with two tabs (documented choice): a single rail entry
//! ("Activity & Health") opens this panel; the Activity/Health switch is an
//! M3 segmented tab group inside the panel. Two rail entries were rejected —
//! the rail foot already holds Settings/Library/VCS/Import, and a second
//! entry would split one read-only story across two destinations.
//!
//! Presentational over the `useActivity` data hook (AC-206), which owns the
//! query machines behind every section:
//!   activity feed .... `activity_feed` (commands/agent.rs: batch over
//!                        persisted `agent_runs` + cross-run spend totals)
//!   conversations .... `list_conversations` (titles for feed labels)
//!   provider health .. `supported_providers` × `provider_health`
//!   budgets .......... feed `totals` + `agent.spend_limit_micro_usd` setting
//!   context pressure . `conversation_context_stats` (active conversation)
//!   git .............. `git_info` (branch + dirty-file summary)
//!   flags ............ `flags_status`
//!   system ........... `diagnostics_bundle` / `update_check` /
//!                        `snapshot_database`
//!   privacy .......... `privacy_status` / `privacy_export` / `privacy_wipe`
//!
//! Display rules (secret-free): feed rows render ids, fixed-vocabulary
//! labels, counters, and relative times only — never `final_content` /
//! `error` text (the backend omits those columns by construction), never
//! credentials, SQL, or message content. Times render relative
//! (`formatRelativeTime`); spend renders as compact dollars from micro-USD.
//! Manual Refresh only — no auto-refresh or live polling (documented).
//! The panel never animates on entry (instant render under reduced motion);
//! tab switches ride the segmented group's own spring, gated by motion.css.
//!
//! AC-208 (review §3.1): the tab bodies and the shared formatters live in
//! `activityHealth/` (`format.ts`, `tabs.ts`, `ActivityTab.tsx`,
//! `HealthTab.tsx`, `SystemSection.tsx`, `PrivacySection.tsx`); this file
//! keeps the shell, the tab/filter state (owned here so a tab round trip
//! preserves both, exactly as before the split), and the public exports.

import { useEffect, useState } from "react";

import { useActivity } from "../lib/useActivity";
import { useStrings } from "../lib/useLocale";
import M3Button from "./M3Button";
import M3LoadingIndicator from "./M3LoadingIndicator";
import M3SegmentedGroup from "./M3SegmentedGroup";
import ActivityTab from "./activityHealth/ActivityTab";
import HealthTab from "./activityHealth/HealthTab";
import SystemSection from "./activityHealth/SystemSection";
import PrivacySection from "./activityHealth/PrivacySection";
import type { ActivityHealthTab, ActivityKind } from "./activityHealth/tabs";

export type { ActivityHealthTab, ActivityKind };

export interface ActivityHealthPanelProps {
  onClose: () => void;
  /** Tab to show on open (rail entry and palette deep links). */
  initialTab?: ActivityHealthTab;
  /** Palette-raised tab switch while open (token dedupes re-renders). */
  tabRequest?: { token: number; tab: ActivityHealthTab } | null;
  /** Active conversation for the context-pressure section (`null` = none). */
  activeConversationId?: number | null;
}

export default function ActivityHealthPanel({
  onClose,
  initialTab = "activity",
  tabRequest = null,
  activeConversationId = null,
}: ActivityHealthPanelProps) {
  const { t } = useStrings();
  const activity = useActivity({ activeConversationId });
  const {
    feed,
    spendFeed,
    conversations,
    activityLoading,
    activityError,
    supported,
    health,
    healthError,
    flags,
    flagsError,
    git,
    gitError,
    context,
    contextError,
    spendLimit,
    healthLoading,
    bundle,
    bundleError,
    update,
    updateError,
    updateBusy,
    snapshot,
    snapshotError,
    snapshotBusy,
    privacy,
    privacyError,
  } = activity;
  const [tab, setTab] = useState<ActivityHealthTab>(initialTab);
  const [kind, setKind] = useState<ActivityKind>("all");
  // Palette deep links retarget the tab while open (mirrors the VCS
  // request pattern): the token identifies the request so re-renders
  // never replay it.
  const [seenToken, setSeenToken] = useState<number | null>(null);
  useEffect(() => {
    if (tabRequest !== null && tabRequest.token !== seenToken) {
      setSeenToken(tabRequest.token);
      setTab(tabRequest.tab);
    }
  }, [tabRequest, seenToken]);
  useEffect(() => {
    setTab(initialTab);
  }, [initialTab]);

  // System section interaction states (clipboard copy of the diagnostics
  // bundle — clipboard only, no IPC). Owned here, not by the section: the
  // section unmounts on every tab switch and the "Copied" confirmation
  // must survive the round trip exactly as before the split.
  const [copied, setCopied] = useState(false);
  const [copyFailed, setCopyFailed] = useState(false);

  return (
    <div className="nex-activity" role="group" aria-label={t("activity.group")}>
      <header className="nex-activity-header">
        <div className="nex-activity-heading">
          <h2 className="nex-activity-title">{t("activity.title")}</h2>
          <p className="nex-activity-subtitle">
            {t("activity.subtitle")}
          </p>
        </div>
        <div className="nex-activity-header-actions">
          <M3Button
            variant="quiet"
            size="sm"
            onClick={activity.reload}
            title={t("activity.refreshTitle")}
          >
            {t("vcs.refresh")}
          </M3Button>
          <M3Button variant="quiet" size="sm" onClick={onClose}>
            {t("common.close")}
          </M3Button>
        </div>
      </header>
      <div className="nex-activity-tabs">
        <M3SegmentedGroup<ActivityHealthTab>
          label={t("activity.tabsLabel")}
          semantics="tabs"
          options={[
            { value: "activity", label: t("activity.tabActivity") },
            { value: "health", label: t("activity.tabHealth") },
          ]}
          value={tab}
          onChange={setTab}
        />
      </div>
      <div className="nex-activity-body">
        {tab === "activity" ? (
          <ActivityTab
            feed={feed}
            spendFeed={spendFeed}
            conversations={conversations}
            loading={activityLoading}
            error={activityError}
            kind={kind}
            onKindChange={setKind}
          />
        ) : healthLoading ? (
          <M3LoadingIndicator label={t("activity.loadingHealth")} />
        ) : (
          <>
            <HealthTab
              supported={supported}
              health={health}
              healthError={healthError}
              feed={feed}
              spendLimit={spendLimit}
              activeConversationId={activeConversationId}
              context={context}
              contextError={contextError}
              git={git}
              gitError={gitError}
              flags={flags}
              flagsError={flagsError}
            />
            <SystemSection
              bundle={bundle}
              bundleError={bundleError}
              copied={copied}
              copyFailed={copyFailed}
              onCopy={async () => {
                if (bundle === null) return;
                try {
                  await navigator.clipboard.writeText(
                    JSON.stringify(bundle, null, 2),
                  );
                  setCopied(true);
                  setCopyFailed(false);
                } catch {
                  setCopied(false);
                  setCopyFailed(true);
                }
              }}
              update={update}
              updateError={updateError}
              updateBusy={updateBusy}
              onCheckUpdate={() => void activity.checkUpdate()}
              snapshot={snapshot}
              snapshotError={snapshotError}
              snapshotBusy={snapshotBusy}
              onSnapshot={() => void activity.makeSnapshot()}
            />
            <PrivacySection
              status={privacy}
              error={privacyError}
              onReload={() => void activity.loadPrivacy()}
              onExport={() => activity.exportLedger()}
              onWipe={() => activity.wipe()}
            />
          </>
        )}
      </div>
    </div>
  );
}
