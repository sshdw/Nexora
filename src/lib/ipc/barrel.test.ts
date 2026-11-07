//! AC-205 barrel-parity pin.
//!
//! `src/lib/tauri.ts` was split into per-area modules under `src/lib/ipc/`
//! and re-exported through the same barrel, so no consumer import changed.
//! The refactor is behavior-preserving only if the barrel's RUNTIME surface
//! is byte-for-byte the old one: every wrapper the monolith exported, under
//! the same name, and nothing else (the cross-module `OP_*` confirmation
//! literals and `core.invoke` stay internal). This test pins that parity
//! against a snapshot list of all 114 exported wrapper functions; a dropped,
//! renamed, or newly leaked export fails here instead of at a consumer.
//!
//! Type-only exports (`interface` / `type`, 96 of them) are erased at
//! runtime and are checked by `npx tsc` across the 53 importing files.

import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
}));

import * as barrel from "../tauri";

/** Snapshot of the pre-split `tauri.ts` runtime exports (`grep -oP
 * "^export (async )?function \K\w+" src/lib/tauri.ts | sort`, 114 names). */
const SNAPSHOT: readonly string[] = [
  "acquireTaskLock",
  "activityFeed",
  "addPermissionRule",
  "addProviderCredential",
  "agentSetMode",
  "archiveConversation",
  "attachFile",
  "cancelAgentRun",
  "clearApplicationData",
  "compatStatus",
  "conversationContextStats",
  "conversationHistory",
  "createConversation",
  "createDebtItem",
  "createIssueForFinding",
  "createPrompt",
  "createProvider",
  "createTask",
  "deleteConversationPermanently",
  "deleteDebtItem",
  "deletePrompt",
  "deleteSetting",
  "deleteTask",
  "depInventory",
  "diagnosticsBundle",
  "docsManifest",
  "exportConversationToFile",
  "exportSetup",
  "exportSetupToFile",
  "extendAgentRun",
  "flagsStatus",
  "getAgentStepObservation",
  "getCompatConfig",
  "getSetting",
  "getWorkspaceRoot",
  "ghActionLog",
  "ghActions",
  "ghIssues",
  "ghPulls",
  "ghRunners",
  "gitCommit",
  "gitCommitDiff",
  "gitExplainCommit",
  "gitFileDiff",
  "gitGenerateCommitMessage",
  "gitInfo",
  "gitPush",
  "gitStage",
  "gitUnstage",
  "hasProviderCredential",
  "heartbeatTaskLock",
  "importConversation",
  "importDebtFromAudit",
  "importMcpServers",
  "importSetup",
  "importVscodeSettings",
  "inspectRun",
  "inspectRunLive",
  "isProviderAvailable",
  "listAgentRuns",
  "listAgentSteps",
  "listAttachments",
  "listConversations",
  "listDebtItems",
  "listPermissionRules",
  "listPrompts",
  "listProviders",
  "listTaskSteps",
  "listTasks",
  "listWorkspaceRecent",
  "moveTask",
  "nexoraInit",
  "pauseAgentRun",
  "privacyExport",
  "privacyStatus",
  "privacyWipe",
  "providerHealth",
  "refactorApply",
  "releaseStatus",
  "releaseTaskLock",
  "removeAttachment",
  "removePermissionRule",
  "removeProvider",
  "removeProviderCredential",
  "renameConversation",
  "repoAudit",
  "requestConfirmation",
  "resolveAgentApproval",
  "restoreConversation",
  "resumeAgentRun",
  "rootsAdd",
  "rootsList",
  "rootsRemove",
  "saveWorkspaceProfile",
  "search",
  "sendMessage",
  "setCompatConfig",
  "setSetting",
  "setWorkspaceRoot",
  "snapshotDatabase",
  "spendDashboard",
  "startAgentRun",
  "startTaskRun",
  "stopTaskRun",
  "supportedProviders",
  "terminalExplain",
  "terminalKill",
  "terminalRun",
  "testgenDrafts",
  "updateCheck",
  "updateDebtItemStatus",
  "updatePrompt",
  "updateProviderCredential",
  "updateTask",
];

describe("tauri barrel parity (AC-205)", () => {
  it("re-exports exactly the historical runtime surface", () => {
    const keys = Object.keys(barrel).sort();
    expect(keys).toEqual([...SNAPSHOT].sort());
  });

  it("re-exports nothing undefined (every entry is the wrapper function)", () => {
    const exports = barrel as unknown as Record<string, unknown>;
    for (const key of SNAPSHOT) {
      const value = exports[key];
      expect(value, `barrel export "${key}" is undefined`).toBeDefined();
      expect(typeof value, `barrel export "${key}" is not a function`).toBe("function");
    }
  });
});
