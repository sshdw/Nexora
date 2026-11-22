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
import { invoke } from "@tauri-apps/api/core";

/** Snapshot of the pre-split `tauri.ts` runtime exports (`grep -oP
 * "^export (async )?function \K\w+" src/lib/tauri.ts | sort`, 114 names). */
const SNAPSHOT: readonly string[] = [
  "acquireTaskLock",
  "activityFeed",
  "addPermissionRule",
  "addProviderCredential",
  "agentSetMode",
  "answerRunQuestion",
  "archiveConversation",
  "attachFile",
  "cancelAgentRun",
  "cancelWorkQueueItem",
  "chatgptOAuthDisconnect",
  "chatgptOAuthRefresh",
  "chatgptOAuthStart",
  "chatgptOAuthStatus",
  "clarifyAgentRun",
  "clearApplicationData",
  "clearModelCapabilityOverride",
  "compactConversationContext",
  "compatStatus",
  "continueAgentRun",
  "conversationContextStats",
  "conversationHistory",
  "createConversation",
  "createDebtItem",
  "createIssueForFinding",
  "createPrompt",
  "createProvider",
  "createTask",
  "decideRunPlan",
  "deleteConversationPermanently",
  "deleteDebtItem",
  "deletePrompt",
  "deleteSetting",
  "deleteTask",
  "depInventory",
  "diagnosticsBundle",
  "discoverProjectInstructions",
  "docsManifest",
  "enqueueWorkItem",
  "exportConversationToFile",
  "exportSetup",
  "exportSetupToFile",
  "extendAgentRun",
  "flagsStatus",
  "getAccessProfile",
  "getAgentStepObservation",
  "getCompatConfig",
  "getModelCapabilities",
  "getResolvedSettings",
  "getSetting",
  "getSandboxStatus",
  "getWorkspaceRoot",
  "ghActionLog",
  "ghActions",
  "ghIssues",
  "ghPulls",
  "ghRunners",
  "gitAbortMerge",
  "gitBranchCreate",
  "gitBranchDelete",
  "gitBranchSwitch",
  "gitBranches",
  "gitCommit",
  "gitCommitDiff",
  "gitConflictDiff",
  "gitExplainCommit",
  "gitFetch",
  "gitFileDiff",
  "gitGenerateCommitMessage",
  "gitInfo",
  "gitPrepareReviewDiff",
  "gitPull",
  "gitPush",
  "gitResolveConflict",
  "gitStage",
  "gitStashList",
  "gitStashPop",
  "gitStashPush",
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
  "killSubagent",
  "listAgentRuns",
  "listAgentSteps",
  "listAttachments",
  "listConversations",
  "listPromptSkills",
  "previewPromptContext",
  "updatePromptSkill",
  "viewPromptSkill",
  "listConversationRunEvents",
  "listDebtItems",
  "listModelCapabilities",
  "listPermissionRules",
  "listPrompts",
  "listProviders",
  "listRunCheckpoints",
  "listRunPlans",
  "listRunQuestions",
  "listTaskSteps",
  "listTasks",
  "listWorkQueue",
  "listWorkspaceRecent",
  "mcpClearAuth",
  "mcpListServerTools",
  "mcpListServers",
  "mcpRefreshOAuth",
  "mcpRemoveServer",
  "mcpSetBearerToken",
  "mcpSetServerEnabled",
  "mcpStartOAuth",
  "mcpUpsertServer",
  "moveTask",
  "nexoraInit",
  // Post-split addition (02-SECURITY §4.2.3/§6.1): the external-open sink
  // wrapper (open_external_url, ipc/system.ts) joins the barrel surface.
  "openExternalUrl",
  "pauseAgentRun",
  "privacyExport",
  "privacyStatus",
  "privacyWipe",
  "providerHealth",
  "refactorApply",
  "onTerminalOutput",
  "onTerminalPtyExit",
  "onTerminalPtyOutput",
  "refreshProviderModelMetadata",
  "releaseStatus",
  "releaseTaskLock",
  "removeAttachment",
  "removePermissionRule",
  "removeProvider",
  "removeProviderCredential",
  "renameConversation",
  "reorderWorkQueue",
  "repoAudit",
  "requestConfirmation",
  "resolveAgentApproval",
  "restoreConversation",
  "resumeAgentRun",
  "revertFileCheckpoint",
  "revertTurnCheckpoints",
  "rootsAdd",
  "rootsList",
  "rootsRemove",
  "runNextWorkItem",
  "saveScopedSetting",
  "saveWorkspaceProfile",
  "search",
  "sendMessage",
  "setAccessProfile",
  "setCompatConfig",
  "setModelCapabilityOverride",
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
  "terminalPtyKill",
  "terminalPtyResize",
  "terminalPtySpawn",
  "terminalPtyWrite",
  "terminalRun",
  "testgenDrafts",
  "updateCheck",
  "updateDebtItemStatus",
  "updatePrompt",
  "updateProviderCredential",
  "updateTask",
  "updateWorkQueueItem",
];

describe("tauri barrel parity (AC-205)", () => {
  it("re-exports exactly the historical runtime surface", () => {
    const keys = Object.keys(barrel).sort();
    expect(keys).toEqual([...SNAPSHOT].sort());
  });

  it("routes durable history with camelCase cursor arguments", async () => {
    await barrel.listConversationRunEvents(7, 23, 2);
    expect(invoke).toHaveBeenLastCalledWith("list_conversation_run_events", {
      conversationId: 7,
      afterEventId: 23,
      limit: 2,
    });
  });

  it("routes killSubagent with camelCase runId and subagentId", async () => {
    await barrel.killSubagent(7, "sub-1");
    expect(invoke).toHaveBeenLastCalledWith("kill_subagent", {
      runId: 7,
      subagentId: "sub-1",
    });
  });

  it("re-exports nothing undefined (every entry is the wrapper function)", () => {
    const exports = barrel as unknown as Record<string, unknown>;
    for (const key of SNAPSHOT) {
      const value = exports[key];
      expect(value, `barrel export "${key}" is undefined`).toBeDefined();
      expect(typeof value, `barrel export "${key}" is not a function`).toBe(
        "function"
      );
    }
  });
});

it("reads authoritative sandbox status without arguments", async () => {
  await barrel.getSandboxStatus();
  expect(invoke).toHaveBeenLastCalledWith("get_sandbox_status");
});

const promptReferences = {
  skills: [{ source_id: "skill-source", revision: "skill-revision" }],
  contexts: [
    {
      selector: { kind: "file", path: "README.md" },
      revision: "file-revision",
    },
  ],
  review: true,
} satisfies barrel.PromptReferences;

it("routes Agent attachments and prompt references with the same Plan and effort selection", async () => {
  await barrel.startAgentRun(
    7,
    "read these files",
    "openai",
    "gpt-4o",
    "high",
    true,
    [12, 13],
    promptReferences
  );
  expect(invoke).toHaveBeenLastCalledWith("start_agent_run", {
    conversationId: 7,
    content: "read these files",
    provider: "openai",
    model: "gpt-4o",
    effort: "high",
    isPlan: true,
    attachmentIds: [12, 13],
    promptReferences,
  });
});

it("sends empty Agent attachments and null prompt references when callers omit them", async () => {
  await barrel.startAgentRun(7, "plain request", "openai", "gpt-4o");
  expect(invoke).toHaveBeenLastCalledWith("start_agent_run", {
    conversationId: 7,
    content: "plain request",
    provider: "openai",
    model: "gpt-4o",
    effort: null,
    isPlan: null,
    attachmentIds: [],
    promptReferences: null,
  });
});

it.each([
  {
    state: "provided",
    references: promptReferences,
    expected: promptReferences,
  },
  { state: "omitted", references: undefined, expected: null },
])("routes $state Chat prompt references", async ({ references, expected }) => {
  await barrel.sendMessage(
    7,
    "read these files",
    "openai",
    "gpt-4o",
    [12],
    "high",
    references
  );
  expect(invoke).toHaveBeenLastCalledWith("send_message", {
    conversationId: 7,
    content: "read these files",
    provider: "openai",
    model: "gpt-4o",
    attachmentIds: [12],
    effort: "high",
    promptReferences: expected,
  });
});

it("routes the ChatGPT OAuth IPC wrappers", async () => {
  await barrel.chatgptOAuthStatus();
  expect(invoke).toHaveBeenLastCalledWith("chatgpt_oauth_status");

  await barrel.chatgptOAuthStart();
  expect(invoke).toHaveBeenLastCalledWith("chatgpt_oauth_start");

  await barrel.chatgptOAuthRefresh();
  expect(invoke).toHaveBeenLastCalledWith("chatgpt_oauth_refresh");

  await barrel.chatgptOAuthDisconnect();
  expect(invoke).toHaveBeenLastCalledWith("chatgpt_oauth_disconnect");
});

it("routes the interactive PTY terminal IPC wrappers with camelCase args", async () => {
  await barrel.terminalPtySpawn(100, 30);
  expect(invoke).toHaveBeenLastCalledWith("terminal_pty_spawn", {
    cols: 100,
    rows: 30,
  });

  await barrel.terminalPtyWrite("sess-1", "ls\r");
  expect(invoke).toHaveBeenLastCalledWith("terminal_pty_write", {
    sessionId: "sess-1",
    data: "ls\r",
  });

  await barrel.terminalPtyResize("sess-1", 120, 40);
  expect(invoke).toHaveBeenLastCalledWith("terminal_pty_resize", {
    sessionId: "sess-1",
    cols: 120,
    rows: 40,
  });

  await barrel.terminalPtyKill("sess-1");
  expect(invoke).toHaveBeenLastCalledWith("terminal_pty_kill", {
    sessionId: "sess-1",
  });
});
