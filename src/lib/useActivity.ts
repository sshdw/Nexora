//! Activity & health data hook (AC-206).
//!
//! Owns the health panel's read-only machines over pre-existing query
//! surfaces: the activity feed (`activity_feed`, capped page + the server-max
//! page backing the spend filter + conversation titles), the provider health
//! probes (`supported_providers` × `provider_health`, per-probe failures
//! degrade to "unknown"), flags, git summary, the active conversation's
//! context stats, the spend-limit setting, the diagnostics bundle, the
//! button-only update check and pre-update snapshot, and the privacy center
//! (status + ledger export + confirmed wipe, which reloads the status).
//! Manual Refresh only — no auto-refresh or live polling (documented).
//!
//! The dock's ActivityPanel shares the family: with `{ taskQueueOnly: true }`
//! the store runs ONLY the panel's one-shot task-queue fetch (`list_tasks`,
//! fetched on open, deliberately not subscribed — G-23) and never fires the
//! health slices, so opening the dock costs exactly the IPC it cost before.
//! The queue is fetched once per mount; the old effect's accidental
//! locale-change refetch (a dependency of its error-fallback string) is not
//! reproduced — the contract is fetch-on-open.

import { useCallback, useEffect, useState } from "react";

import {
  activityFeed,
  conversationContextStats,
  diagnosticsBundle,
  flagsStatus,
  getSetting,
  gitInfo,
  listConversations,
  listTasks,
  privacyExport,
  privacyStatus,
  privacyWipe,
  providerHealth,
  snapshotDatabase,
  supportedProviders,
  updateCheck,
  type ActivityFeed,
  type CommandError,
  type Conversation,
  type ConversationContextStats,
  type DiagnosticsBundle,
  type FlagsStatus,
  type GitInfo,
  type LedgerExport,
  type PrivacyStatus,
  type ProviderHealth,
  type SnapshotInfo,
  type SupportedProvider,
  type UpdateCheck,
  type WipeResult,
} from "./tauri";
import { SPEND_LIMIT_KEY } from "./useSpendLimit";
import { getLocale, tr } from "./strings";

const FEED_LIMIT = 50;
// Server-max page for the spend filter: the backend clamps `limit` to
// 1..=100, and totals cover every persisted run, so the spend set derives
// from this wider page (the main list keeps FEED_LIMIT).
const FEED_MAX = 100;

export interface ActivityStore {
  feed: ActivityFeed | null;
  /** Wider (server-max) page backing the "spend" filter only; `null` =
   * unavailable → the filter falls back to the main `feed` page. */
  spendFeed: ActivityFeed | null;
  conversations: Conversation[];
  activityLoading: boolean;
  activityError: string | null;
  supported: SupportedProvider[];
  health: Record<string, ProviderHealth>;
  healthError: string | null;
  flags: FlagsStatus | null;
  flagsError: string | null;
  git: GitInfo | null;
  gitError: string | null;
  context: ConversationContextStats | null;
  contextError: string | null;
  spendLimit: number | null;
  healthLoading: boolean;
  bundle: DiagnosticsBundle | null;
  bundleError: string | null;
  update: UpdateCheck | null;
  updateError: string | null;
  updateBusy: boolean;
  /** Button-only update check (never on a timer). */
  checkUpdate: () => Promise<void>;
  snapshot: SnapshotInfo | null;
  snapshotError: string | null;
  snapshotBusy: boolean;
  /** User-initiated pre-update snapshot. */
  makeSnapshot: () => Promise<void>;
  privacy: PrivacyStatus | null;
  privacyError: string | null;
  loadPrivacy: () => Promise<void>;
  /** Ledger aggregates for the caller's copy/download. */
  exportLedger: () => Promise<LedgerExport>;
  /** Confirmed wipe (the native OS dialog is the gate); reloads the
   * privacy status on success. */
  wipe: () => Promise<WipeResult>;
  /** Manual refresh: reloads the feed and the health aggregate. */
  reload: () => void;
  /** Dock task queue (taskQueueOnly stores only): `null` until answered. */
  queue: import("./tauri").AgentTask[] | null;
  queueError: string | null;
}

function toMessage(error: unknown): string {
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

function parseSpendLimit(value: string | null): number | null {
  if (value === null) return null;
  const parsed = Number(value.trim());
  if (!Number.isInteger(parsed) || parsed <= 0) return null;
  return parsed;
}

export function useActivity(
  options?: { taskQueueOnly?: boolean; activeConversationId?: number | null },
): ActivityStore {
  const taskQueueOnly = options?.taskQueueOnly === true;
  const activeConversationId = options?.activeConversationId ?? null;
  const [feed, setFeed] = useState<ActivityFeed | null>(null);
  const [spendFeed, setSpendFeed] = useState<ActivityFeed | null>(null);
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [activityLoading, setActivityLoading] = useState<boolean>(true);
  const [activityError, setActivityError] = useState<string | null>(null);

  const [supported, setSupported] = useState<SupportedProvider[]>([]);
  const [health, setHealth] = useState<Record<string, ProviderHealth>>({});
  const [healthError, setHealthError] = useState<string | null>(null);
  const [flags, setFlags] = useState<FlagsStatus | null>(null);
  const [flagsError, setFlagsError] = useState<string | null>(null);
  const [git, setGit] = useState<GitInfo | null>(null);
  const [gitError, setGitError] = useState<string | null>(null);
  const [context, setContext] = useState<ConversationContextStats | null>(null);
  const [contextError, setContextError] = useState<string | null>(null);
  const [spendLimit, setSpendLimit] = useState<number | null>(null);
  const [healthLoading, setHealthLoading] = useState<boolean>(true);

  // System & diagnostics (bundle auto-loads with health — local only;
  // the update check runs on the button press alone, never on a timer).
  const [bundle, setBundle] = useState<DiagnosticsBundle | null>(null);
  const [bundleError, setBundleError] = useState<string | null>(null);
  const [update, setUpdate] = useState<UpdateCheck | null>(null);
  const [updateError, setUpdateError] = useState<string | null>(null);
  const [updateBusy, setUpdateBusy] = useState(false);
  const [snapshot, setSnapshot] = useState<SnapshotInfo | null>(null);
  const [snapshotError, setSnapshotError] = useState<string | null>(null);
  const [snapshotBusy, setSnapshotBusy] = useState(false);

  // Privacy center: egress inventory + local ledger (read-only here; the
  // export/wipe actions live in the caller's privacy section).
  const [privacy, setPrivacy] = useState<PrivacyStatus | null>(null);
  const [privacyError, setPrivacyError] = useState<string | null>(null);

  // Dock task queue (one-shot, not subscribed — G-23).
  const [queue, setQueue] = useState<import("./tauri").AgentTask[] | null>(null);
  const [queueError, setQueueError] = useState<string | null>(null);

  const loadPrivacy = useCallback(async () => {
    try {
      setPrivacy(await privacyStatus());
      setPrivacyError(null);
    } catch (e) {
      setPrivacyError(toMessage(e));
      setPrivacy(null);
    }
  }, []);

  const loadActivity = useCallback(async () => {
    setActivityLoading(true);
    setActivityError(null);
    try {
      const [feedData, convs, spendData] = await Promise.all([
        activityFeed(FEED_LIMIT),
        listConversations(),
        activityFeed(FEED_MAX).catch(() => null),
      ]);
      setFeed(feedData);
      setConversations(convs);
      setSpendFeed(spendData);
    } catch (e) {
      setActivityError(toMessage(e));
      setFeed(null);
      setSpendFeed(null);
    } finally {
      setActivityLoading(false);
    }
  }, []);

  const loadHealth = useCallback(async () => {
    setHealthLoading(true);
    setHealthError(null);
    setFlagsError(null);
    setGitError(null);
    setContextError(null);
    setBundleError(null);
    try {
      const defs = await supportedProviders();
      setSupported(defs);
      const probes = await Promise.all(
        defs.map(async (def) => {
          try {
            return await providerHealth(def.name);
          } catch {
            return null;
          }
        }),
      );
      const next: Record<string, ProviderHealth> = {};
      defs.forEach((def, index) => {
        const probe = probes[index];
        if (probe) next[def.name] = probe;
      });
      setHealth(next);
    } catch (e) {
      setHealthError(toMessage(e));
      setSupported([]);
    }
    try {
      setFlags(await flagsStatus());
    } catch (e) {
      setFlagsError(toMessage(e));
      setFlags(null);
    }
    try {
      setGit(await gitInfo(5));
    } catch (e) {
      setGitError(toMessage(e));
      setGit(null);
    }
    try {
      const stored = await getSetting(SPEND_LIMIT_KEY);
      setSpendLimit(parseSpendLimit(stored));
    } catch {
      setSpendLimit(null);
    }
    try {
      setBundle(await diagnosticsBundle());
    } catch (e) {
      setBundleError(toMessage(e));
      setBundle(null);
    }
    await loadPrivacy();
    if (activeConversationId !== null) {
      try {
        setContext(await conversationContextStats(activeConversationId));
      } catch (e) {
        setContextError(toMessage(e));
        setContext(null);
      }
    } else {
      setContext(null);
    }
    setHealthLoading(false);
  }, [activeConversationId, loadPrivacy]);

  // Manual refresh only: the store loads on mount / refresh, never on a
  // timer (no auto-refresh, no live polling — documented). A taskQueueOnly
  // store never fires these slices.
  useEffect(() => {
    if (taskQueueOnly) return;
    void loadActivity();
    void loadHealth();
  }, [taskQueueOnly, loadActivity, loadHealth]);

  useEffect(() => {
    if (!taskQueueOnly) return;
    let cancelled = false;
    listTasks()
      .then((rows) => {
        if (!cancelled) setQueue(rows);
      })
      .catch((e: unknown) => {
        if (!cancelled) {
          setQueue([]);
          setQueueError(
            e instanceof Error ? e.message : tr(getLocale(), "task.unavailable"),
          );
        }
      });
    return () => {
      cancelled = true;
    };
  }, [taskQueueOnly]);

  const checkUpdate = useCallback(async () => {
    setUpdateBusy(true);
    setUpdateError(null);
    try {
      setUpdate(await updateCheck());
    } catch (e) {
      setUpdateError(toMessage(e));
      setUpdate(null);
    } finally {
      setUpdateBusy(false);
    }
  }, []);

  const makeSnapshot = useCallback(async () => {
    setSnapshotBusy(true);
    setSnapshotError(null);
    try {
      setSnapshot(await snapshotDatabase());
    } catch (e) {
      setSnapshotError(toMessage(e));
      setSnapshot(null);
    } finally {
      setSnapshotBusy(false);
    }
  }, []);

  const exportLedger = useCallback(async (): Promise<LedgerExport> => {
    return privacyExport();
  }, []);

  const wipe = useCallback(async (): Promise<WipeResult> => {
    const result = await privacyWipe();
    await loadPrivacy();
    return result;
  }, [loadPrivacy]);

  const reload = useCallback(() => {
    void loadActivity();
    void loadHealth();
  }, [loadActivity, loadHealth]);

  return {
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
    checkUpdate,
    snapshot,
    snapshotError,
    snapshotBusy,
    makeSnapshot,
    privacy,
    privacyError,
    loadPrivacy,
    exportLedger,
    wipe,
    reload,
    queue,
    queueError,
  };
}
