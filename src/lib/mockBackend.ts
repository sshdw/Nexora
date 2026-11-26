//! DEV-ONLY visual-QA mock backend — never active in production.
//! Updated for Task 5.1 with agent run parity.

import type {
  ApprovalMetadata,
  PlanDecisionKind,
  PlanDecisionResult,
  PlanDecisionStatus,
  RunPlan,
} from "./ipc/agent";

interface MockMessage {
  id: number;
  conversation_id: number;
  role: "user" | "assistant";
  content: string;
  provider_id: number | null;
  model_name: string | null;
  created_at: number;
}

const now = () => Math.floor(Date.now() / 1000);
const hoursAgo = (h: number) => now() - h * 3600;

let nextConversationId = 4;
let nextMessageId = 4;
let nextPromptId = 3;
let nextProviderId = 2;
let nextAttachmentId = 1;

// Single source for the mock catalog (DEV-ONLY): the live backend's
// `supported_providers()` is the source of truth; this hardcoded list only
// feeds the visual-QA mock. Provider/model IDs are written once here and all
// mock defaults below derive from it (no duplication).
const supported = [
  {
    name: "openai",
    display_name: "OpenAI",
    models: ["gpt-5", "gpt-4o", "o3", "o4-mini", "gpt-4.1"],
  },
  {
    name: "anthropic",
    display_name: "Anthropic",
    models: [
      "claude-sonnet-4-20250514",
      "claude-haiku-4-5-20251001",
      "claude-opus-4-20250514",
    ],
  },
  {
    name: "gemini",
    display_name: "Gemini",
    models: [
      "gemini-2.5-flash",
      "gemini-2.5-pro",
      "gemini-2.5-flash-lite",
      "gemini-flash-lite-latest",
      "gemini-pro-latest",
    ],
  },
  {
    name: "openrouter",
    display_name: "OpenRouter",
    models: [
      "inclusionai/ling-3.0-flash-fin:free",
      "nvidia/nemotron-3.5-lightning:free",
      "nvidia/nemotron-3-super-120b-a12b:free",
      "cohere/north-mini-code:free",
      "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free",
      "inclusionai/ling-3.0-flash-sante:free",
      "inclusionai/ling-3.0-flash-vl:free",
      "liquid/lfm-2.5-2.6b:free",
    ],
  },
  {
    name: "nvidia",
    display_name: "NVIDIA NIM",
    models: [
      "nvidia/nemotron-3-super-120b-a12b",
      "nvidia/nemotron-3-ultra-550b-a55b",
      "meta/llama-3.2-11b-vision-instruct",
      "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning",
      "openai/gpt-oss-20b",
    ],
  },
  {
    name: "opencode_zen",
    display_name: "OpenCode Zen",
    models: [
      "ling-3.0-flash-fin-free",
      "nemotron-3-ultra-free",
      "nemotron-3.5-lightning-free",
      "big-pickle",
      "mimo-v2.5-free",
    ],
  },
  {
    name: "deepseek",
    display_name: "DeepSeek",
    models: ["deepseek-chat", "deepseek-reasoner"],
  },
  {
    name: "qwen",
    display_name: "Alibaba / Qwen",
    models: ["qwen-max", "qwen-plus", "qwq-plus", "qwen-vl-max"],
  },
  {
    name: "zai",
    display_name: "ZAI",
    models: ["glm-4.5", "glm-4.5-air", "glm-4-plus"],
  },
  {
    name: "minimax",
    display_name: "MiniMax",
    models: ["MiniMax-M1", "MiniMax-Text-01"],
  },
  {
    name: "moonshot",
    display_name: "Moonshot / Kimi",
    models: [
      "kimi-k2-0711-preview",
      "kimi-thinking-preview",
      "moonshot-v1-128k",
      "moonshot-v1-128k-vision-preview",
    ],
  },
  {
    name: "groq",
    display_name: "Groq",
    models: [
      "llama-3.3-70b-versatile",
      "openai/gpt-oss-120b",
      "openai/gpt-oss-20b",
      "qwen/qwen3-32b",
      "llama-3.1-8b-instant",
    ],
  },
  {
    name: "xai",
    display_name: "xAI",
    models: ["grok-3", "grok-3-mini", "grok-4.7"],
  },
  {
    name: "mistral",
    display_name: "Mistral",
    models: [
      "mistral-large-latest",
      "magistral-medium-latest",
      "codestral-latest",
      "pixtral-large-latest",
    ],
  },
  {
    name: "openai_compat",
    display_name: "OpenAI Compatible",
    models: [],
  },
  {
    name: "chatgpt_subscription",
    display_name: "ChatGPT subscription",
    models: [],
  },
];
const MOCK_DEFAULT_PROVIDER = supported[2].name;
const MOCK_DEFAULT_MODEL = supported[2].models[0];

const conversations = [
  {
    id: 1,
    title: "Designing the composer",
    status: "active",
    created_at: hoursAgo(30),
    updated_at: hoursAgo(1),
    workspace_root: null as string | null,
  },
  {
    id: 2,
    title: "Rust migration notes",
    status: "active",
    created_at: hoursAgo(50),
    updated_at: hoursAgo(5),
    workspace_root: null as string | null,
  },
  {
    id: 3,
    title: "Old research thread",
    status: "archived",
    created_at: hoursAgo(200),
    updated_at: hoursAgo(96),
    workspace_root: null as string | null,
  },
];

const messages: MockMessage[] = [
  {
    id: 1,
    conversation_id: 1,
    role: "user",
    content: "Hello",
    provider_id: null,
    model_name: null,
    created_at: hoursAgo(1),
  },
  {
    id: 2,
    conversation_id: 1,
    role: "assistant",
    content: "Hello! How can I help you today?",
    provider_id: 1,
    model_name: MOCK_DEFAULT_MODEL,
    created_at: hoursAgo(1) + 20,
  },
  {
    id: 3,
    conversation_id: 1,
    role: "user",
    content:
      "Summarize the Material 3 Expressive motion system in two sentences.",
    provider_id: null,
    model_name: null,
    created_at: hoursAgo(1) + 60,
  },
];

const prompts = [
  {
    id: 1,
    title: "Weekly review",
    content:
      "Summarize this week's progress, list blockers, and propose the top three priorities for next week.",
    created_at: hoursAgo(20),
    updated_at: hoursAgo(2),
  },
  {
    id: 2,
    title: "Code review checklist",
    content:
      "Review the attached diff for correctness, edge cases, naming, and test coverage. Flag anything that changes public behavior.",
    created_at: hoursAgo(40),
    updated_at: hoursAgo(10),
  },
];

const providers = [
  {
    id: 1,
    name: MOCK_DEFAULT_PROVIDER,
    display_name: supported[2].display_name,
  },
];
const credentialed = new Set<string>([MOCK_DEFAULT_PROVIDER]);
const settings = new Map<string, string>([
  ["provider.selected", MOCK_DEFAULT_PROVIDER],
  ["provider.model", MOCK_DEFAULT_MODEL],
  ["appearance.theme", "dark"],
  ["agent.autonomy", "semi_autonomous"],
  ["agent.access_profile", "standard"],
  ["agent.spend_limit_micro_usd", "1000000"],
]);
const projectSettings = new Map<string, string>();
const MOCK_LAYERED_DEFAULTS: Record<string, string | null> = {
  "appearance.theme": "dark",
  "provider.selected": null,
  "provider.model": null,
  "provider.effort": null,
  "provider.capability_overrides": null,
  "agent.access_profile": "standard",
  "agent.autonomy": "semi_autonomous",
  "agent.spend_limit_micro_usd": null,
  "agent.approval_timeout_secs": "600",
  "agent.preset": "coding",
  "flags.snapshots": "true",
  "flags.self_audit": "true",
  "flags.injection": "true",
  "flags.assembly": "true",
  "routing.profile.chat": null,
  "routing.profile.agent": null,
  "mcp.servers": null,
};

function mockAccessRank(v: string | undefined): number {
  if (v === "read_only" || v === "supervised") return 2;
  if (v === "full_access") return 0;
  return 1;
}

function mockAutonomyRank(v: string | undefined): number {
  if (v === "supervised") return 2;
  if (v === "full_autonomous") return 0;
  return 1;
}

function buildMockResolvedSettings(
  sessionOverrides?: Record<string, string> | null
) {
  const entries: Record<
    string,
    {
      key: string;
      value: string | null;
      source: "session" | "project" | "personal" | "default";
      blocked_project_value: boolean;
      blocked_reason: string | null;
    }
  > = {};
  const blocked: { key: string; reason: string; detail: string }[] = [];
  const personalAccess = settings.get("agent.access_profile") ?? "standard";
  const personalAutonomy = settings.get("agent.autonomy") ?? "semi_autonomous";

  for (const [key, defVal] of Object.entries(MOCK_LAYERED_DEFAULTS)) {
    const sessVal = sessionOverrides?.[key];
    let projVal = projectSettings.get(key);
    const persVal = settings.get(key);
    let blockedProject = false;
    let blockedReason: string | null = null;

    if (
      key === "agent.access_profile" &&
      projVal !== undefined &&
      (projVal === "full_access" ||
        mockAccessRank(projVal) < mockAccessRank(personalAccess))
    ) {
      projVal = undefined;
      blockedProject = true;
      blockedReason = "cannot_elevate_access_profile";
      blocked.push({
        key,
        reason: blockedReason,
        detail:
          "project config cannot elevate access profile above personal or session ceiling",
      });
    } else if (
      key === "agent.autonomy" &&
      projVal !== undefined &&
      mockAutonomyRank(projVal) < mockAutonomyRank(personalAutonomy)
    ) {
      projVal = undefined;
      blockedProject = true;
      blockedReason = "cannot_widen_autonomy";
      blocked.push({
        key,
        reason: blockedReason,
        detail:
          "project config cannot widen autonomy above personal or session ceiling",
      });
    } else if (key === "flags.injection" && projVal === "false") {
      projVal = undefined;
      blockedProject = true;
      blockedReason = "cannot_weaken_sandbox";
      blocked.push({
        key,
        reason: blockedReason,
        detail:
          "project config cannot weaken sandbox rules or deny-read/sensitive-write guards",
      });
    }

    let value: string | null = defVal;
    let source: "session" | "project" | "personal" | "default" = "default";
    if (sessVal !== undefined && sessVal !== "") {
      value = sessVal;
      source = "session";
    } else if (projVal !== undefined) {
      value = projVal;
      source = "project";
    } else if (persVal !== undefined) {
      value = persVal;
      source = "personal";
    }

    entries[key] = {
      key,
      value,
      source,
      blocked_project_value: blockedProject,
      blocked_reason: blockedReason,
    };
  }

  return {
    workspace_root: mockWorkspaceRoot || null,
    has_project_config: projectSettings.size > 0,
    entries,
    blocked,
    config_notice: null,
  };
}

// ---- Agent workspace folder mock state (1.3.0) ----
let mockWorkspaceRoot = "C:\\mock\\workspace";
let mockWorkspaceRecent: string[] = ["C:\\mock\\workspace"];

// ---- Agent task queue (DEV-ONLY mirror of `agent_tasks`) ----
// One row per state the Activity panel groups by, plus the backing steps
// TaskPanel lists. Ids and shapes follow src/lib/tauri.ts exactly.
const mockTasks = [
  {
    id: 1,
    title: "Migrate the settings store to the mono theme",
    description: "Replace the legacy M3E tokens in settings.css.",
    status: "running" as const,
    conversation_id: 1,
    provider: MOCK_DEFAULT_PROVIDER,
    model: MOCK_DEFAULT_MODEL,
    max_steps: 6,
    current_step: 3,
    total_steps: 5,
    report: null,
    run_id: null,
    created_at: hoursAgo(2),
    updated_at: hoursAgo(0),
  },
  {
    id: 2,
    title: "Audit the diff viewer for horizontal overflow",
    description: null,
    status: "pending" as const,
    conversation_id: 2,
    provider: MOCK_DEFAULT_PROVIDER,
    model: MOCK_DEFAULT_MODEL,
    max_steps: 4,
    current_step: 0,
    total_steps: 4,
    report: null,
    run_id: null,
    created_at: hoursAgo(6),
    updated_at: hoursAgo(5),
  },
  {
    id: 3,
    title: "Document the approval recovery contract",
    description: null,
    status: "completed" as const,
    conversation_id: 1,
    provider: MOCK_DEFAULT_PROVIDER,
    model: MOCK_DEFAULT_MODEL,
    max_steps: 3,
    current_step: 3,
    total_steps: 3,
    report: "Written to docs/.",
    run_id: null,
    created_at: hoursAgo(30),
    updated_at: hoursAgo(28),
  },
];

const mockTaskSteps = [
  {
    id: 1,
    task_id: 1,
    seq: 1,
    title: "Read settings.css",
    status: "completed" as const,
    result: null,
    run_id: null,
    started_at: hoursAgo(2),
    finished_at: hoursAgo(2) + 30,
  },
  {
    id: 2,
    task_id: 1,
    seq: 2,
    title: "Map legacy tokens",
    status: "completed" as const,
    result: null,
    run_id: null,
    started_at: hoursAgo(2) + 60,
    finished_at: hoursAgo(2) + 90,
  },
  {
    id: 3,
    task_id: 1,
    seq: 3,
    title: "Rewrite the token block",
    status: "running" as const,
    result: null,
    run_id: null,
    started_at: hoursAgo(1),
    finished_at: null,
  },
  {
    id: 4,
    task_id: 1,
    seq: 4,
    title: "Verify contrast pairs",
    status: "pending" as const,
    result: null,
    run_id: null,
    started_at: hoursAgo(1),
    finished_at: null,
  },
  {
    id: 5,
    task_id: 1,
    seq: 5,
    title: "Run the theme test",
    status: "pending" as const,
    result: null,
    run_id: null,
    started_at: hoursAgo(1),
    finished_at: null,
  },
];

const attachments: Array<{
  id: number;
  conversation_id: number;
  message_id: number | null;
  file_name: string;
  file_path: string;
  file_size_bytes: number | null;
  mime_type: string | null;
}> = [];

// ---- Agent run mock state (Task 5.1) ----
let nextAgentRunId = 1;
interface MockAgentRun {
  interrupted: boolean;
  id: number;
  conversation_id: number | null;
  model: string;
  mode: string;
  status: string;
  started_at: number;
  finished_at: number | null;
  total_steps: number;
  final_content: string | null;
  error: string | null;
  spent_micro_usd: number | null;
  limit_micro_usd: number | null;
  is_plan?: boolean;
  plan_content?: string | null;
  plan_decision_status?: PlanDecisionStatus | null;
  plan_decision_reply?: string | null;
  plan_implemented_run_id?: number | null;
  continued_from_run_id?: number | null;
}
interface MockAgentStep {
  id: number;
  run_id: number;
  seq: number;
  kind: string;
  tool_name: string | null;
  arguments: string | null;
  observation: string | null;
  observation_len: number | null;
  observation_truncated: boolean;
  status: string | null;
  started_at: number;
  duration_ms: number | null;
}
const agentRuns: MockAgentRun[] = [];
const agentSteps = new Map<number, MockAgentStep[]>();
let mockNextStepId = 1;

/** DEV-ONLY byte-length mirror of the backend preview contract
 * (NEX-PERF-003): mock observations are short, so every mock step lists
 * untruncated with its exact byte total. */
function mockObsLen(observation: string | null): number | null {
  return observation === null
    ? null
    : new TextEncoder().encode(observation).length;
}

type ActiveMockRun = {
  status: string;
  timers: number[];
  pendingApproval: {
    call_id: string;
    name: string;
    arguments: string;
    requested_at: number;
    metadata: ApprovalMetadata;
  } | null;
  pendingQuestion: {
    question_id: number;
    run_id: number;
    conversation_id: number;
    call_id: string;
    question: string;
    options: string[];
    multi_select: boolean;
    allow_free_text: boolean;
    created_at: number;
  } | null;
  budgetExhausted: boolean;
  paused: boolean;
};
const activeRuns = new Map<number, ActiveMockRun>();

type MockRunQuestion = {
  id: number;
  run_id: number;
  conversation_id: number;
  call_id: string;
  question: string;
  options: string[];
  multi_select: boolean;
  allow_free_text: boolean;
  status: "pending" | "answered" | "cancelled";
  selected_options: string[];
  free_text: string | null;
  created_at: number;
  answered_at: number | null;
};
const mockRunQuestions: MockRunQuestion[] = [];
const mockRunPlans: RunPlan[] = [];
let mockNextPlanId = 1;

function spawnSimpleMockContinuationOrImpl(params: {
  convId: number;
  model: string;
  summaryText: string;
  continuedFromRunId: number | null;
}): number {
  const runId = nextAgentRunId++;
  const run: MockAgentRun = {
    interrupted: false,
    id: runId,
    conversation_id: params.convId,
    model: params.model,
    mode: "semi_autonomous",
    status: "running",
    started_at: now(),
    finished_at: null,
    total_steps: 0,
    final_content: null,
    error: null,
    spent_micro_usd: null,
    limit_micro_usd: null,
    is_plan: false,
    plan_content: null,
    plan_decision_status: null,
    plan_decision_reply: null,
    plan_implemented_run_id: null,
    continued_from_run_id: params.continuedFromRunId,
  };
  agentRuns.push(run);
  agentSteps.set(runId, []);
  const active: ActiveMockRun = {
    status: "running",
    timers: [],
    pendingApproval: null,
    pendingQuestion: null,
    budgetExhausted: false,
    paused: false,
  };
  activeRuns.set(runId, active);
  active.timers.push(
    window.setTimeout(() => {
      if (!activeRuns.has(runId)) return;
      const steps = agentSteps.get(runId)!;
      const stepId = mockNextStepId++;
      const step: MockAgentStep = {
        id: stepId,
        run_id: runId,
        seq: steps.length + 1,
        kind: "model_turn",
        tool_name: null,
        arguments: null,
        observation: params.summaryText,
        observation_len: mockObsLen(params.summaryText),
        observation_truncated: false,
        status: null,
        started_at: now(),
        duration_ms: 80,
      };
      steps.push(step);
      run.total_steps = steps.length;
      emitAgentEvent({
        type: "step",
        conversation_id: params.convId,
        run_id: runId,
        event: {
          seq: step.seq,
          kind: step.kind,
          tool_name: null,
          arguments: null,
          observation: step.observation,
          observation_len: step.observation_len,
          observation_truncated: false,
          status: null,
          duration_ms: 80,
        },
      });
      run.status = "completed";
      run.finished_at = now();
      run.final_content = params.summaryText;
      activeRuns.delete(runId);
      messages.push({
        id: nextMessageId++,
        conversation_id: params.convId,
        role: "assistant",
        content: params.summaryText,
        provider_id: 1,
        model_name: params.model,
        created_at: now(),
      });
      emitAgentEvent({
        type: "finished",
        run_id: runId,
        event: {
          conversation_id: params.convId,
          status: "completed",
          final_content: params.summaryText,
          error: null,
        },
      });
    }, 300) as unknown as number
  );
  return runId;
}

type MockWorkQueueItem = {
  id: number;
  conversation_id: number;
  content: string;
  provider: string;
  model: string;
  effort: string | null;
  position: number;
  status: "queued" | "running" | "completed" | "cancelled";
  created_at: number;
  updated_at: number;
};
const mockWorkQueue: MockWorkQueueItem[] = [];
let nextWorkQueueId = 1;

function emitWorkQueueUpdated(conversationId: number) {
  const listeners = eventListeners.get("work-queue-updated");
  if (!listeners) return;
  for (const [, cb] of listeners) {
    try {
      (cb as (v: unknown) => void)({
        event: "work-queue-updated",
        id: 0,
        payload: { conversation_id: conversationId },
      });
    } catch {
      // ignore handler errors
    }
  }
}

// Event channel mock
let nextEventId = 1;
const eventListeners = new Map<string, Map<number, (ev: unknown) => void>>();

function emitAgentEvent(frame: unknown) {
  const listeners = eventListeners.get("agent-run-event");
  if (!listeners) return;
  for (const [, cb] of listeners) {
    try {
      (cb as (v: unknown) => void)({
        event: "agent-run-event",
        id: 0,
        payload: frame,
      });
    } catch {
      // ignore handler errors
    }
  }
}

let nextMockTerminalRunId = 1;
let activeMockTerminalKill: (() => void) | null = null;

function emitTerminalOutput(frame: {
  run_id: number;
  stream: string;
  chunk: string;
  done: boolean;
  truncated: boolean;
  killed: boolean;
  success: boolean | null;
}) {
  const listeners = eventListeners.get("terminal-output");
  if (!listeners) return;
  for (const [, cb] of listeners) {
    try {
      (cb as (v: unknown) => void)({
        event: "terminal-output",
        id: 0,
        payload: frame,
      });
    } catch {
      // ignore handler errors
    }
  }
}

interface MockPtySession {
  sessionId: string;
  shellName: string;
  cwd: string;
  cols: number;
  rows: number;
  lineBuffer: string;
}

let nextMockPtyId = 1;
const mockPtySessions = new Map<string, MockPtySession>();

function emitTerminalPtyOutput(sessionId: string, data: string) {
  const listeners = eventListeners.get("terminal-pty-output");
  if (!listeners) return;
  for (const [, cb] of listeners) {
    try {
      (cb as (v: unknown) => void)({
        event: "terminal-pty-output",
        id: 0,
        payload: { session_id: sessionId, data },
      });
    } catch {
      // ignore handler errors
    }
  }
}

function emitTerminalPtyExit(sessionId: string, exitCode: number | null) {
  const listeners = eventListeners.get("terminal-pty-exit");
  if (!listeners) return;
  for (const [, cb] of listeners) {
    try {
      (cb as (v: unknown) => void)({
        event: "terminal-pty-exit",
        id: 0,
        payload: { session_id: sessionId, exit_code: exitCode },
      });
    } catch {
      // ignore handler errors
    }
  }
}

/** Strict camelCase-only argument reader for the agent commands. Mirrors the
 * Tauri v2 IPC contract (arguments are deserialized by camelCase name) so the
 * mock enforces the same contract instead of forgiving snake_case fallbacks. */
function requireArg(
  args: Record<string, unknown>,
  command: string,
  key: string
): unknown {
  if (!(key in args) || args[key] === undefined || args[key] === null) {
    throw {
      kind: "invalidInput",
      message: `command ${command} missing required key ${key}`,
    };
  }
  return args[key];
}

function fail(message: string): never {
  throw { kind: "unknown", message };
}

async function invoke(
  command: string,
  args: Record<string, unknown> = {}
): Promise<unknown> {
  // Small delay so loading/skeleton states are observable.
  await new Promise((resolve) => setTimeout(resolve, 120));
  switch (command) {
    case "list_conversations":
      return [...conversations].sort((a, b) => b.updated_at - a.updated_at);
    case "create_conversation": {
      const id = nextConversationId++;
      conversations.push({
        id,
        title: typeof args.title === "string" ? args.title : "New Conversation",
        status: "active",
        created_at: now(),
        updated_at: now(),
        workspace_root: mockWorkspaceRoot,
      });
      return id;
    }
    case "conversation_history": {
      const id = Number(args.conversationId ?? args.conversation_id);
      return messages
        .filter((m) => m.conversation_id === id)
        .sort((a, b) => a.created_at - b.created_at);
    }
    case "rename_conversation": {
      const row = conversations.find((c) => c.id === Number(args.id));
      if (!row) fail("Conversation not found.");
      row.title = String(args.title);
      row.updated_at = now();
      return null;
    }
    case "archive_conversation": {
      const row = conversations.find((c) => c.id === Number(args.id));
      if (!row) fail("Conversation not found.");
      row.status = "archived";
      return null;
    }
    case "restore_conversation": {
      const row = conversations.find((c) => c.id === Number(args.id));
      if (!row) fail("Conversation not found.");
      row.status = "active";
      return null;
    }
    case "delete_conversation_permanently": {
      if (
        typeof args.confirmationId !== "string" ||
        args.confirmationId.length === 0
      ) {
        fail("Mock backend: a confirmation id is required.");
      }
      const id = Number(args.id);
      const index = conversations.findIndex((c) => c.id === id);
      if (index >= 0) conversations.splice(index, 1);
      for (let i = messages.length - 1; i >= 0; i--) {
        if (messages[i].conversation_id === id) messages.splice(i, 1);
      }
      // cascade agent runs
      for (let i = agentRuns.length - 1; i >= 0; i--)
        if (agentRuns[i].conversation_id === id) agentRuns.splice(i, 1);
      return null;
    }
    case "list_providers":
      return providers;
    case "supported_providers":
      return supported;
    case "create_provider": {
      const id = nextProviderId++;
      providers.push({
        id,
        name: String(args.name),
        display_name: String(args.displayName ?? args.display_name),
      });
      return id;
    }
    case "remove_provider": {
      const id = Number(args.id);
      const index = providers.findIndex((p) => p.id === id);
      if (index >= 0) providers.splice(index, 1);
      return null;
    }
    case "is_provider_available":
      return (
        providers.some((p) => p.name === (args.name as string)) &&
        credentialed.has(String(args.name))
      );
    case "has_provider_credential":
      return credentialed.has(String(args.provider));
    case "add_provider_credential":
    case "update_provider_credential":
      credentialed.add(String(args.provider));
      return null;
    case "remove_provider_credential":
      credentialed.delete(String(args.provider));
      return null;
    case "chatgpt_oauth_status":
      return {
        connected: settings.get("__mock_chatgpt_oauth_connected") === "true",
        has_refresh_token:
          settings.get("__mock_chatgpt_oauth_connected") === "true",
        expires_at:
          settings.get("__mock_chatgpt_oauth_connected") === "true"
            ? now() + 3600
            : null,
        connected_at:
          settings.get("__mock_chatgpt_oauth_connected") === "true"
            ? now() - 60
            : null,
        problem: null,
        models:
          settings.get("__mock_chatgpt_oauth_connected") === "true"
            ? [{ slug: "gpt-6.2-sol", display_name: "GPT-6.2 Sol" }]
            : [],
        auth_mode:
          settings.get("__mock_chatgpt_oauth_connected") === "true"
            ? "oauth"
            : "none",
      };
    case "chatgpt_oauth_start": {
      settings.set("__mock_chatgpt_oauth_connected", "true");
      if (!providers.some((p) => p.name === "chatgpt_subscription")) {
        providers.push({
          id: nextProviderId++,
          name: "chatgpt_subscription",
          display_name: "ChatGPT subscription",
        });
      }
      return {
        connected: true,
        has_refresh_token: true,
        expires_at: now() + 3600,
        connected_at: now(),
        problem: null,
        models: [{ slug: "gpt-6.2-sol", display_name: "GPT-6.2 Sol" }],
        auth_mode: "oauth",
      };
    }
    case "chatgpt_oauth_refresh": {
      if (settings.get("__mock_chatgpt_oauth_connected") !== "true") {
        throw {
          kind: "request",
          message: "ChatGPT OAuth is not connected",
        };
      }
      return {
        connected: true,
        has_refresh_token: true,
        expires_at: now() + 3600,
        connected_at: now() - 60,
        problem: null,
        models: [{ slug: "gpt-6.2-sol", display_name: "GPT-6.2 Sol" }],
        auth_mode: "oauth",
      };
    }
    case "chatgpt_oauth_disconnect":
      settings.delete("__mock_chatgpt_oauth_connected");
      return null;
    case "get_compat_config":
      return {
        base_url: settings.get("openai_compat.base_url") ?? "",
        model: settings.get("openai_compat.model") ?? "",
        organization: settings.get("openai_compat.organization") ?? null,
        headers: JSON.parse(settings.get("openai_compat.headers_json") ?? "[]"),
        supports_tools:
          (settings.get("openai_compat.supports_tools") ?? "true") !== "false",
      };
    case "set_compat_config": {
      const cfg = (args.config ?? {}) as {
        base_url?: string;
        model?: string;
        organization?: string | null;
        headers?: [string, string][];
        supports_tools?: boolean;
      };
      const rawUrl = (cfg.base_url ?? "").trim();
      const rawModel = (cfg.model ?? "").trim();
      if (!rawUrl)
        throw { kind: "invalidInput", message: "no base URL is configured" };
      let parsed: URL;
      try {
        parsed = new URL(rawUrl);
      } catch {
        throw {
          kind: "invalidInput",
          message:
            "the base URL must use https (http is allowed only for localhost)",
        };
      }
      if (parsed.username || parsed.password) {
        throw {
          kind: "invalidInput",
          message:
            "the base URL must not embed credentials; the API key belongs in the OS keyring",
        };
      }
      const isLocal = ["localhost", "127.0.0.1", "[::1]", "::1"].includes(
        parsed.hostname.toLowerCase()
      );
      if (
        parsed.protocol !== "https:" &&
        !(parsed.protocol === "http:" && isLocal)
      ) {
        throw {
          kind: "invalidInput",
          message:
            "the base URL must use https (http is allowed only for localhost)",
        };
      }
      if (!rawModel)
        throw { kind: "invalidInput", message: "no model is configured" };
      settings.set("openai_compat.base_url", rawUrl);
      settings.set("openai_compat.model", rawModel);
      if (cfg.organization?.trim())
        settings.set("openai_compat.organization", cfg.organization.trim());
      else settings.delete("openai_compat.organization");
      settings.set(
        "openai_compat.headers_json",
        JSON.stringify(cfg.headers ?? [])
      );
      settings.set(
        "openai_compat.supports_tools",
        cfg.supports_tools === false ? "false" : "true"
      );
      return null;
    }
    case "compat_status": {
      const baseUrl = (settings.get("openai_compat.base_url") ?? "").trim();
      const model = (settings.get("openai_compat.model") ?? "").trim();
      let baseUrlValid = false;
      try {
        const parsed = new URL(baseUrl);
        const isLocal = ["localhost", "127.0.0.1", "[::1]", "::1"].includes(
          parsed.hostname.toLowerCase()
        );
        baseUrlValid =
          !parsed.username &&
          !parsed.password &&
          (parsed.protocol === "https:" ||
            (parsed.protocol === "http:" && isLocal));
      } catch {
        baseUrlValid = false;
      }
      const hasCredential = credentialed.has("openai_compat");
      return {
        has_base_url: baseUrl.length > 0,
        base_url_valid: baseUrlValid,
        has_model: model.length > 0,
        has_organization: Boolean(
          (settings.get("openai_compat.organization") ?? "").trim()
        ),
        header_count: JSON.parse(
          settings.get("openai_compat.headers_json") ?? "[]"
        ).length,
        supports_tools:
          (settings.get("openai_compat.supports_tools") ?? "true") !== "false",
        has_credential: hasCredential,
        ready: baseUrlValid && model.length > 0 && hasCredential,
      };
    }
    case "get_model_capabilities":
    case "list_model_capabilities":
    case "set_model_capability_override":
    case "clear_model_capability_override":
    case "refresh_provider_model_metadata": {
      const rawPersonal = settings.get("provider.capability_overrides");
      const rawProject = projectSettings.get("provider.capability_overrides");
      const personalMap: Record<string, Record<string, unknown>> = rawPersonal
        ? JSON.parse(rawPersonal)
        : {};
      const projectMap: Record<string, Record<string, unknown>> = rawProject
        ? JSON.parse(rawProject)
        : {};
      const overrideMap: Record<
        string,
        {
          context_window?: number | null;
          max_output_tokens?: number | null;
          supported_efforts?: string[] | null;
          supports_thinking?: boolean | null;
          supports_attachments?: boolean | null;
          supports_tools?: boolean | null;
        }
      > = { ...personalMap, ...projectMap };
      const resolveMockCaps = (connId: string, modelId: string) => {
        const listed = supported.some(
          (p) => p.name === connId && p.models.includes(modelId)
        );
        const base = !listed
          ? {
              connection_id: connId,
              model_id: modelId,
              context_window: null,
              max_output_tokens: null,
              supported_efforts: [] as string[],
              supports_thinking: false,
              supports_attachments: false,
              supports_tools: false,
              provenance: "unknown" as const,
              has_manual_override: false,
            }
          : modelId.includes("flash-lite")
            ? {
                connection_id: connId,
                model_id: modelId,
                context_window: 1000000,
                max_output_tokens: 65536,
                supported_efforts: [] as string[],
                supports_thinking: false,
                supports_attachments: true,
                supports_tools: true,
                provenance: "built_in_catalog" as const,
                has_manual_override: false,
              }
            : {
                connection_id: connId,
                model_id: modelId,
                context_window: connId === "gemini" ? 1000000 : 200000,
                max_output_tokens: 64000,
                supported_efforts:
                  connId === "gemini" || modelId.includes("haiku")
                    ? ["low", "medium", "high"]
                    : ["low", "medium", "high", "xhigh"],
                supports_thinking: true,
                supports_attachments: true,
                supports_tools: true,
                provenance: "built_in_catalog" as const,
                has_manual_override: false,
              };
        const ov = overrideMap[`${connId}::${modelId}`];
        if (!ov) return base;
        return {
          ...base,
          context_window:
            ov.context_window !== undefined
              ? ov.context_window
              : base.context_window,
          max_output_tokens:
            ov.max_output_tokens !== undefined
              ? ov.max_output_tokens
              : base.max_output_tokens,
          supported_efforts:
            ov.supported_efforts !== undefined && ov.supported_efforts !== null
              ? ov.supported_efforts
              : base.supported_efforts,
          supports_thinking: ov.supports_thinking ?? base.supports_thinking,
          supports_attachments:
            ov.supports_attachments ?? base.supports_attachments,
          supports_tools: ov.supports_tools ?? base.supports_tools,
          provenance: "manual_override" as const,
          has_manual_override: true,
        };
      };
      if (command === "get_model_capabilities") {
        return resolveMockCaps(
          String(args.connectionId ?? args.connection_id ?? ""),
          String(args.modelId ?? args.model_id ?? "")
        );
      }
      if (
        command === "set_model_capability_override" ||
        command === "clear_model_capability_override"
      ) {
        const connId = String(args.connectionId ?? args.connection_id ?? "");
        const modelId = String(args.modelId ?? args.model_id ?? "");
        const scope = String(args.scope ?? "personal");
        const spec =
          command === "clear_model_capability_override"
            ? null
            : (args.overrideSpec ?? args.override_spec ?? null);
        if (scope === "project" && !mockWorkspaceRoot) {
          throw {
            kind: "invalidInput",
            message:
              "an active project workspace is required to save project settings",
          };
        }
        const targetStore = scope === "project" ? projectSettings : settings;
        const targetMap = scope === "project" ? projectMap : personalMap;
        if (spec) {
          targetMap[`${connId}::${modelId}`] = spec as Record<string, unknown>;
          overrideMap[`${connId}::${modelId}`] = spec as never;
        } else {
          delete targetMap[`${connId}::${modelId}`];
          if (scope === "project") {
            const fallback = personalMap[`${connId}::${modelId}`];
            if (fallback)
              overrideMap[`${connId}::${modelId}`] = fallback as never;
            else delete overrideMap[`${connId}::${modelId}`];
          } else if (!projectMap[`${connId}::${modelId}`]) {
            delete overrideMap[`${connId}::${modelId}`];
          }
        }
        if (Object.keys(targetMap).length === 0) {
          targetStore.delete("provider.capability_overrides");
        } else {
          targetStore.set(
            "provider.capability_overrides",
            JSON.stringify(targetMap)
          );
        }
        return resolveMockCaps(connId, modelId);
      }
      if (command === "refresh_provider_model_metadata") {
        const connId = String(args.connectionId ?? args.connection_id ?? "");
        const def = supported.find((p) => p.name === connId);
        if (connId === "openai_compat") {
          const model = (settings.get("openai_compat.model") ?? "").trim();
          return model ? [resolveMockCaps(connId, model)] : [];
        }
        return (def?.models ?? []).map((m) => resolveMockCaps(connId, m));
      }
      return supported.flatMap((p) =>
        p.models.map((m) => resolveMockCaps(p.name, m))
      );
    }
    case "get_resolved_settings":
      return buildMockResolvedSettings(
        (args.sessionOverrides ?? args.session_overrides) as Record<
          string,
          string
        > | null
      );
    case "save_scoped_setting": {
      const scope = String(args.scope ?? "personal");
      const key = String(args.key ?? "").trim();
      const val = args.value as string | null | undefined;
      const sessionOverrides = (args.sessionOverrides ??
        args.session_overrides) as Record<string, string> | null;
      if (scope !== "personal" && scope !== "project") {
        throw {
          kind: "invalidInput",
          message: "setting scope must be 'personal' or 'project'",
        };
      }
      if (scope === "project" && !mockWorkspaceRoot) {
        throw {
          kind: "invalidInput",
          message:
            "an active project workspace is required to save project settings",
        };
      }
      if (
        val !== null &&
        val !== undefined &&
        /sk-[A-Za-z0-9]{16,}/.test(String(val))
      ) {
        throw {
          kind: "permissionDenied",
          message: "settings cannot store raw credentials or API keys",
        };
      }
      if (scope === "project") {
        const strVal = val === null || val === undefined ? null : String(val);
        const personalAccess =
          settings.get("agent.access_profile") ?? "standard";
        const personalAutonomy =
          settings.get("agent.autonomy") ?? "semi_autonomous";
        if (
          key === "agent.access_profile" &&
          strVal !== null &&
          (strVal === "full_access" ||
            mockAccessRank(strVal) < mockAccessRank(personalAccess))
        ) {
          throw {
            kind: "permissionDenied",
            message:
              "project config cannot elevate access profile above personal or session ceiling",
          };
        }
        if (
          key === "agent.autonomy" &&
          strVal !== null &&
          mockAutonomyRank(strVal) < mockAutonomyRank(personalAutonomy)
        ) {
          throw {
            kind: "permissionDenied",
            message:
              "project config cannot widen autonomy above personal or session ceiling",
          };
        }
        if (key === "flags.injection" && strVal === "false") {
          throw {
            kind: "permissionDenied",
            message:
              "project config cannot disable prompt-injection protection",
          };
        }
        if (strVal === null) projectSettings.delete(key);
        else projectSettings.set(key, strVal);
      } else {
        if (val === null || val === undefined) settings.delete(key);
        else settings.set(key, String(val));
      }
      return buildMockResolvedSettings(sessionOverrides);
    }
    case "discover_project_instructions":
      return {
        files: [],
        combined: "",
        skipped_escapes: [],
        truncated: false,
      };
    case "get_setting":
      return settings.get(String(args.key)) ?? null;
    case "set_setting": {
      const key = String(args.key);
      const val = args.value as string | null | undefined;
      if (val === null || val === undefined) {
        settings.delete(key);
      } else {
        const strVal = String(val);
        if (key === "agent.autonomy") {
          if (
            !["supervised", "semi_autonomous", "full_autonomous"].includes(
              strVal
            )
          ) {
            throw {
              kind: "invalidInput",
              message: `value '${strVal}' is not a valid 'agent.autonomy' setting`,
            };
          }
        }
        if (key === "agent.spend_limit_micro_usd") {
          if (!/^\d+$/.test(strVal.trim()) || Number(strVal.trim()) <= 0) {
            throw {
              kind: "invalidInput",
              message: `value '${strVal}' is not a valid 'agent.spend_limit_micro_usd' setting`,
            };
          }
        }
        // Mirror the Rust custom model rule (commands/settings.rs): a listed
        // shortlist ID or 1..=200 chars of A-Za-z0-9._/:-+ without `..`.
        if (key === "provider.model") {
          const listed = supported.some((p) => p.models.includes(strVal));
          const custom =
            strVal.length >= 1 &&
            strVal.length <= 200 &&
            !strVal.includes("..") &&
            /^[A-Za-z0-9._/:+-]+$/.test(strVal);
          if (!listed && !custom) {
            throw {
              kind: "invalidInput",
              message: `value '${strVal}' is not a valid 'provider.model' setting`,
            };
          }
        }
        settings.set(key, strVal);
      }
      return null;
    }
    case "delete_setting":
      settings.delete(String(args.key));
      return null;
    case "request_confirmation": {
      // DEV-ONLY mock: every scope mints the same canned id, and the
      // destructive cases below accept any non-empty id (production mints
      // unguessable single-use ids backend-side — see NEX-SEC-004). The
      // allowlist mirrors the Rust `scope_of` set, now including the SF-03
      // scopes (version_control / refactor / privacy / release / export).
      const mockScopes = [
        "terminal",
        "data_management",
        "version_control",
        "refactor",
        "privacy",
        "release",
        "export",
      ];
      if (!mockScopes.includes(String(args.scope))) {
        fail("Mock backend: unknown confirmation scope.");
      }
      return "mock-confirmation-id";
    }
    case "clear_application_data": {
      if (
        typeof args.confirmationId !== "string" ||
        args.confirmationId.length === 0
      ) {
        fail("Mock backend: a confirmation id is required.");
      }
      conversations.length = 0;
      messages.length = 0;
      prompts.length = 0;
      providers.length = 0;
      credentialed.clear();
      settings.clear();
      attachments.length = 0;
      agentRuns.length = 0;
      agentSteps.clear();
      activeRuns.clear();
      return null;
    }
    case "search": {
      const q = String(args.query).toLowerCase();
      return {
        conversations: conversations.filter((c) =>
          c.title.toLowerCase().includes(q)
        ),
        message_matches: messages.filter((m) =>
          m.content.toLowerCase().includes(q)
        ),
        prompts: prompts.filter(
          (p) =>
            p.title.toLowerCase().includes(q) ||
            p.content.toLowerCase().includes(q)
        ),
      };
    }
    case "list_prompts":
      return prompts;
    case "create_prompt": {
      const id = nextPromptId++;
      prompts.push({
        id,
        title: String(args.title),
        content: String(args.content),
        created_at: now(),
        updated_at: now(),
      });
      return id;
    }
    case "update_prompt": {
      const row = prompts.find((p) => p.id === Number(args.id));
      if (!row) fail("Prompt not found.");
      row.title = String(args.title);
      row.content = String(args.content);
      row.updated_at = now();
      return null;
    }
    case "delete_prompt_permanently": {
      if (
        typeof args.confirmationId !== "string" ||
        args.confirmationId.length === 0
      ) {
        fail("Mock backend: a confirmation id is required.");
      }
      const index = prompts.findIndex((p) => p.id === Number(args.id));
      if (index >= 0) prompts.splice(index, 1);
      return null;
    }
    case "export_conversation_to_file":
      return null;
    case "import_conversation": {
      // SEC-01 wire shape: the command carries the dialog-picked `path` (the
      // backend reads the document itself); the mock cannot read files, so it
      // validates the path shape and imports the canned conversation.
      const pickedPath = String(args.path ?? "");
      if (!pickedPath.endsWith(".json")) {
        fail("Unsupported file.");
      }
      const id = nextConversationId++;
      conversations.push({
        id,
        title: "Imported conversation",
        status: "active",
        created_at: now(),
        updated_at: now(),
        workspace_root: mockWorkspaceRoot,
      });
      return id;
    }
    case "attach_file": {
      // SEC-01 wire shape: no size argument exists — production derives the
      // size backend-side from the picked file's own metadata. The mock
      // stands in with a fixed canned size so visual QA still renders one.
      const id = nextAttachmentId++;
      attachments.push({
        id,
        conversation_id: Number(args.conversationId ?? args.conversation_id),
        message_id: null,
        file_name: String(args.fileName ?? args.file_name),
        file_path: String(args.filePath ?? args.file_path),
        file_size_bytes: 1024,
        mime_type:
          typeof (args.mimeType ?? args.mime_type) === "string"
            ? ((args.mimeType ?? args.mime_type) as string)
            : null,
      });
      return attachments[attachments.length - 1];
    }
    case "list_attachments":
      return attachments.filter(
        (a) =>
          a.conversation_id ===
            Number(args.conversationId ?? args.conversation_id) &&
          a.message_id === null
      );
    case "remove_attachment": {
      const index = attachments.findIndex((a) => a.id === Number(args.id));
      if (index >= 0) attachments.splice(index, 1);
      return null;
    }
    case "send_message": {
      const conversationId = Number(
        args.conversationId ?? args.conversation_id
      );
      const content = String(args.content);
      const userId = nextMessageId++;
      messages.push({
        id: userId,
        conversation_id: conversationId,
        role: "user",
        content,
        provider_id: null,
        model_name: null,
        created_at: now(),
      });
      await new Promise((resolve) => setTimeout(resolve, 900));
      messages.push({
        id: nextMessageId++,
        conversation_id: conversationId,
        role: "assistant",
        content: `Mock reply to “${content}”.`,
        provider_id: 1,
        model_name: String(args.model),
        created_at: now(),
      });
      const conversation = conversations.find((c) => c.id === conversationId);
      if (conversation) conversation.updated_at = now();
      for (const id of ((args.attachmentIds ??
        args.attachment_ids) as number[]) ?? []) {
        const draft = attachments.find((a) => a.id === id);
        if (draft) draft.message_id = userId;
      }
      return { content: "ok", model: String(args.model) };
    }
    // ---- Agent workspace folder (1.3.0) ----
    case "get_sandbox_status":
      return {
        workspace_root: mockWorkspaceRoot,
        state: "unavailable",
        posture: "PerCommandConsent",
        mechanism: null,
        reason: "Native OS verification is unavailable in the visual mock.",
      };
    case "get_workspace_root":
      return mockWorkspaceRoot;
    case "set_workspace_root": {
      const raw = String(args.path ?? "");
      const trimmed = raw.trim();
      if (!trimmed)
        throw {
          kind: "invalidInput",
          message: "invalid workspace root: path must not be empty",
        };
      if (trimmed.length > 1024)
        throw {
          kind: "invalidInput",
          message: "invalid workspace root: path exceeds 1024 characters",
        };
      const lowered = trimmed.toLowerCase().replace(/\//g, "\\");
      const noTrail = lowered.replace(/\\+$/, "");
      if (noTrail === "c:\\windows" || noTrail.startsWith("c:\\windows\\")) {
        throw {
          kind: "invalidInput",
          message:
            "invalid workspace root: the Windows system directory is not allowed",
        };
      }
      if (/^[a-z]:$/.test(noTrail) || noTrail === "" || noTrail === "\\") {
        throw {
          kind: "invalidInput",
          message: "invalid workspace root: a drive root is not allowed",
        };
      }
      mockWorkspaceRoot = trimmed;
      mockWorkspaceRecent = [
        trimmed,
        ...mockWorkspaceRecent.filter((p) => p !== trimmed),
      ].slice(0, 5);
      settings.set("agent.workspace_root", trimmed);
      settings.set(
        "agent.workspace_recent",
        JSON.stringify(mockWorkspaceRecent)
      );
      return mockWorkspaceRoot;
    }
    case "list_workspace_recent":
      return [...mockWorkspaceRecent];
    // Registry view (roots_list / roots_add / roots_remove). The mock kept
    // only the 1.3.0 root + MRU ring, so a DEV run failed the whole workspace
    // load with "unhandled command roots_list" — which made the Workspace
    // panel show its error state instead of its content. Mirror the backend
    // contract (active root first) so visual QA exercises the real shape.
    case "roots_list":
      return {
        active: mockWorkspaceRoot,
        roots: [
          mockWorkspaceRoot,
          ...mockWorkspaceRecent.filter((p) => p !== mockWorkspaceRoot),
        ],
      };
    case "roots_add": {
      const path = String(args.path ?? "").trim();
      if (!path)
        throw {
          kind: "invalidInput",
          message: "invalid workspace root: path must not be empty",
        };
      mockWorkspaceRoot = path;
      mockWorkspaceRecent = [
        path,
        ...mockWorkspaceRecent.filter((p) => p !== path),
      ].slice(0, 5);
      return {
        active: mockWorkspaceRoot,
        roots: [
          mockWorkspaceRoot,
          ...mockWorkspaceRecent.filter((p) => p !== mockWorkspaceRoot),
        ],
      };
    }
    case "roots_remove": {
      const path = String(args.path ?? "");
      mockWorkspaceRecent = mockWorkspaceRecent.filter((p) => p !== path);
      if (mockWorkspaceRoot === path)
        mockWorkspaceRoot = mockWorkspaceRecent[0] ?? mockWorkspaceRoot;
      return {
        active: mockWorkspaceRoot,
        roots: [
          mockWorkspaceRoot,
          ...mockWorkspaceRecent.filter((p) => p !== mockWorkspaceRoot),
        ],
      };
    }
    // ---- Agent task queue (G-03) ----
    // The queue is the Activity panel's only honest feed, so the mock needs
    // rows for it (and for TaskPanel, which had none either). Three states,
    // one per section the panel groups by.
    case "list_tasks":
      return [...mockTasks].sort((a, b) => b.updated_at - a.updated_at);
    case "list_task_steps":
      return mockTaskSteps.filter(
        (step) => step.task_id === Number(args.taskId)
      );
    // ---- Agent runs (Task 5.1/5.2) ----
    case "start_agent_run": {
      const conversationId = Number(
        requireArg(args, "start_agent_run", "conversationId")
      );
      const content = String(requireArg(args, "start_agent_run", "content"));
      const provider = String(requireArg(args, "start_agent_run", "provider"));
      const model = String(requireArg(args, "start_agent_run", "model"));
      const isPlan = Boolean(args.isPlan);
      const convId = conversationId;
      if (!conversations.some((c) => c.id === convId)) {
        throw {
          kind: "notFound",
          message: `conversation ${convId} does not exist`,
        };
      }
      for (const [rid] of activeRuns) {
        const ar = agentRuns.find((x) => x.id === rid);
        if (ar && ar.conversation_id === convId)
          throw {
            kind: "invalidInput",
            message: `an agent run is already active for conversation ${convId}`,
          };
      }
      if (content.trim() === "")
        throw {
          kind: "invalidInput",
          message: "the user message must not be empty",
        };
      // Persist user message immediately
      const userId = nextMessageId++;
      messages.push({
        id: userId,
        conversation_id: convId,
        role: "user",
        content,
        provider_id: null,
        model_name: null,
        created_at: now(),
      });
      const conv = conversations.find((c) => c.id === convId);
      if (conv) conv.updated_at = now();

      const runId = nextAgentRunId++;
      const resolvedMode = (() => {
        const stored = settings.get("agent.autonomy");
        if (
          stored === "supervised" ||
          stored === "semi_autonomous" ||
          stored === "full_autonomous"
        )
          return stored;
        return "semi_autonomous";
      })();
      const run: MockAgentRun = {
        interrupted: false,
        id: runId,
        conversation_id: convId,
        model,
        mode: isPlan ? "read_only" : resolvedMode,
        status: "running",
        started_at: now(),
        finished_at: null,
        total_steps: 0,
        final_content: null,
        error: null,
        spent_micro_usd: null,
        limit_micro_usd: null,
        is_plan: isPlan,
        plan_content: null,
        plan_decision_status: isPlan ? "pending" : null,
        plan_decision_reply: null,
        plan_implemented_run_id: null,
        continued_from_run_id: null,
      };
      agentRuns.push(run);
      agentSteps.set(runId, []);
      const active: ActiveMockRun = {
        status: "running",
        timers: [],
        pendingApproval: null,
        pendingQuestion: null,
        budgetExhausted: false,
        paused: false,
      };
      activeRuns.set(runId, active);

      // Helper to append persisted step and emit live frame
      const appendStep = (
        kind: string,
        tool_name: string | null,
        argsStr: string | null,
        observation: string | null,
        status: string | null,
        duration_ms: number | null
      ) => {
        const steps = agentSteps.get(runId)!;
        const seq = steps.length + 1;
        const stepId = mockNextStepId++;
        // DEV-ONLY mirror of the backend preview contract (NEX-PERF-003):
        // the frame and the listing carry the preview; the full text is
        // served by the `get_agent_step_observation` case below.
        const observation_len =
          observation === null
            ? null
            : new TextEncoder().encode(observation).length;
        const step: MockAgentStep = {
          id: stepId,
          run_id: runId,
          seq,
          kind,
          tool_name,
          arguments: argsStr,
          observation,
          observation_len,
          observation_truncated: false,
          status,
          started_at: now(),
          duration_ms,
        };
        steps.push(step);
        run.total_steps = steps.length;
        emitAgentEvent({
          type: "step",
          conversation_id: agentRuns.find((r) => r.id === runId)!
            .conversation_id!,
          run_id: runId,
          event: {
            step_id: stepId,
            seq,
            kind,
            tool_name,
            arguments: argsStr,
            observation,
            observation_len,
            observation_truncated: false,
            status,
            duration_ms,
          },
        });
      };

      const finishRun = (
        status: string,
        finalContent: string | null,
        error: string | null
      ) => {
        // Clear timers
        for (const t of active.timers) clearTimeout(t);
        active.timers = [];
        run.status = status;
        run.finished_at = now();
        run.final_content = finalContent;
        run.error = error;
        activeRuns.delete(runId);
        // If completed cleanly and NOT a Plan run, persist assistant message
        // (Plan runs live in the Plan card + decision prompt rather than duplicating a bubble).
        if (isPlan && status === "completed" && finalContent) {
          const ts = now();
          const planRow: RunPlan = {
            id: mockNextPlanId++,
            run_id: runId,
            conversation_id: convId,
            plan_content: finalContent,
            decision_status: "pending",
            decision_reply: null,
            implemented_run_id: null,
            decided_at: null,
            created_at: ts,
          };
          mockRunPlans.push(planRow);
          run.plan_content = finalContent;
          run.plan_decision_status = "pending";
          emitAgentEvent({
            type: "governance",
            conversation_id: convId,
            run_id: runId,
            event: { type: "plan_ready", plan: planRow },
          });
        } else if (status === "completed" && finalContent) {
          messages.push({
            id: nextMessageId++,
            conversation_id: convId,
            role: "assistant",
            content: finalContent,
            provider_id: 1,
            model_name: model,
            created_at: now(),
          });
          const c = conversations.find((x) => x.id === convId);
          if (c) c.updated_at = now();
        }
        emitAgentEvent({
          type: "finished",
          run_id: runId,
          event: {
            conversation_id: convId,
            status,
            final_content: finalContent,
            error,
          },
        });
      };

      if (isPlan) {
        active.timers.push(
          window.setTimeout(() => {
            if (!activeRuns.has(runId)) return;
            appendStep(
              "tool_call",
              "read_file",
              JSON.stringify({ path: "README.md" }),
              "# Read-only inspection complete",
              "succeeded",
              40
            );
          }, 250) as unknown as number
        );
        active.timers.push(
          window.setTimeout(() => {
            if (!activeRuns.has(runId)) return;
            const planText = `# Implementation Plan\n\n1. Inspect affected modules in read-only mode.\n2. Apply changes for: ${content}\n3. Verify with targeted tests.`;
            appendStep("model_turn", null, null, planText, null, 90);
            finishRun("completed", planText, null);
          }, 600) as unknown as number
        );
        void provider;
        return { run_id: runId };
      }

      // Synthetic stream: terminal (execute_command) + diff (write_file) + approval park
      // Step 1: model_turn thinking (400ms)
      active.timers.push(
        window.setTimeout(() => {
          if (!activeRuns.has(runId)) return;
          appendStep(
            "model_turn",
            null,
            null,
            "Thinking about your request…",
            null,
            120
          );
          emitAgentEvent({
            type: "governance",
            conversation_id: agentRuns.find((r) => r.id === runId)!
              .conversation_id!,
            run_id: runId,
            event: { type: "completed", steps: 1 },
          });
        }, 400) as unknown as number
      );

      // Step 2: execute_command terminal (900ms) — demonstrates terminal view
      active.timers.push(
        window.setTimeout(() => {
          if (!activeRuns.has(runId)) return;
          const cmd = "cargo test";
          const observation =
            "running 3 tests\ntest ok\n--- stderr ---\nwarning: unused variable";
          appendStep(
            "tool_call",
            "execute_command",
            JSON.stringify({ command: cmd }),
            observation,
            "succeeded",
            210
          );
        }, 900) as unknown as number
      );

      // Step 3: approval park for write_file (1400ms) -> emit ApprovalRequested, park until resolved
      active.timers.push(
        window.setTimeout(() => {
          if (!activeRuns.has(runId)) return;
          const callId = `call-${runId}-approval`;
          const writeArgs = JSON.stringify({
            path: "output.txt",
            content: "hello world\nsecond line",
          });
          const rawProfile = settings.get("agent.access_profile");
          const profile: ApprovalMetadata["access_profile"] =
            rawProfile === undefined || rawProfile === "standard"
              ? "standard"
              : rawProfile === "full_access"
                ? "full_access"
                : "supervised";
          const metadata: ApprovalMetadata = {
            access_profile: profile,
            sandbox_posture: "PerCommandConsent",
            checkpoints_available: false,
            allow_for_run: profile === "standard",
            sensitive_write: false,
            shell: null,
          };
          active.pendingApproval = {
            requested_at: now(),
            metadata,
            call_id: callId,
            name: "write_file",
            arguments: writeArgs,
          };
          emitAgentEvent({
            type: "governance",
            conversation_id: agentRuns.find((r) => r.id === runId)!
              .conversation_id!,
            run_id: runId,
            event: {
              type: "approval_requested",
              metadata,
              call_id: callId,
              name: "write_file",
              arguments: writeArgs,
              timeout_secs: 600,
            },
          });
          // Auto-resolve after ~2s if not manually resolved
          const auto = window.setTimeout(() => {
            if (!activeRuns.has(runId) || !active.pendingApproval) return;
            const approved = true;
            emitAgentEvent({
              type: "governance",
              conversation_id: agentRuns.find((r) => r.id === runId)!
                .conversation_id!,
              run_id: runId,
              event: { type: "approval_resolved", call_id: callId, approved },
            });
            appendStep(
              "approval",
              "write_file",
              writeArgs,
              approved ? "approved" : "denied",
              approved ? "succeeded" : "denied",
              null
            );
            active.pendingApproval = null;
            // After approval, continue with write_file diff and final model_turn
            window.setTimeout(() => {
              if (!activeRuns.has(runId)) return;
              const diff =
                "--- a/output.txt\n+++ b/output.txt\n@@ -0,0 +1,2 @@\n+hello world\n+second line\n";
              appendStep(
                "tool_call",
                "write_file",
                writeArgs,
                diff,
                "succeeded",
                12
              );
            }, 400);
            window.setTimeout(() => {
              if (!activeRuns.has(runId)) return;
              appendStep(
                "model_turn",
                null,
                null,
                `Mock agent reply to “${content}”.`,
                null,
                80
              );
              finishRun("completed", `Mock agent reply to “${content}”.`, null);
            }, 900);
          }, 2000);
          active.timers.push(auto as unknown as number);
        }, 1400) as unknown as number
      );

      // For mock simplicity, provider/model args already captured
      void provider;

      return { run_id: runId };
    }
    case "kill_subagent": {
      const runId = Number(requireArg(args, "kill_subagent", "runId"));
      const subagentId = String(
        requireArg(args, "kill_subagent", "subagentId")
      );
      const active = activeRuns.get(runId);
      if (!active)
        throw {
          kind: "notFound",
          message: `no active agent run with id ${runId}`,
        };
      const run = agentRuns.find((r) => r.id === runId);
      emitAgentEvent({
        type: "governance",
        run_id: runId,
        conversation_id: run?.conversation_id ?? 0,
        event: {
          type: "subagent_updated",
          subagent: {
            subagent_id: subagentId,
            title: subagentId,
            status: "cancelled",
            model: run?.model ?? "mock-model",
            effort: null,
            access: "read_only",
            worktree_isolated: false,
            steps_taken: 0,
            max_turns: 12,
            started_at: now(),
            finished_at: now(),
            result_summary: null,
            error: "Stopped by user",
            patch_applied: false,
            patch_conflict: false,
          },
        },
      });
      return null;
    }
    case "cancel_agent_run": {
      const runId = Number(requireArg(args, "cancel_agent_run", "runId"));
      const active = activeRuns.get(runId);
      if (!active)
        throw {
          kind: "notFound",
          message: `no active agent run with id ${runId}`,
        };
      for (const t of active.timers) clearTimeout(t);
      active.timers = [];
      const run = agentRuns.find((r) => r.id === runId);
      if (run) {
        run.status = "cancelled";
        run.finished_at = now();
      }
      // Cancel wakes parked approval: emit cancelled if needed
      if (active.pendingApproval) {
        // record cancelled approval step
        const steps = agentSteps.get(runId)!;
        const seq = steps.length + 1;
        const sid = mockNextStepId++;
        steps.push({
          id: sid,
          run_id: runId,
          seq,
          kind: "approval",
          tool_name: active.pendingApproval.name,
          arguments: active.pendingApproval.arguments,
          observation: "cancelled by the user",
          observation_len: mockObsLen("cancelled by the user"),
          observation_truncated: false,
          status: "cancelled",
          started_at: now(),
          duration_ms: null,
        });
        emitAgentEvent({
          type: "step",
          conversation_id: agentRuns.find((r) => r.id === runId)!
            .conversation_id!,
          run_id: runId,
          event: {
            step_id: sid,
            seq,
            kind: "approval",
            tool_name: active.pendingApproval.name,
            arguments: active.pendingApproval.arguments,
            observation: "cancelled by the user",
            observation_len: mockObsLen("cancelled by the user"),
            observation_truncated: false,
            status: "cancelled",
            duration_ms: null,
          },
        });
        active.pendingApproval = null;
        emitAgentEvent({
          type: "governance",
          conversation_id: agentRuns.find((r) => r.id === runId)!
            .conversation_id!,
          run_id: runId,
          event: { type: "cancelled" },
        });
      } else {
        emitAgentEvent({
          type: "governance",
          conversation_id: agentRuns.find((r) => r.id === runId)!
            .conversation_id!,
          run_id: runId,
          event: { type: "cancelled" },
        });
      }
      activeRuns.delete(runId);
      const run2 = agentRuns.find((r) => r.id === runId);
      const convId = run2?.conversation_id ?? 0;
      emitAgentEvent({
        type: "finished",
        run_id: runId,
        event: {
          conversation_id: convId,
          status: "cancelled",
          final_content: null,
          error: null,
        },
      });
      return null;
    }
    case "resolve_agent_approval": {
      const runId = Number(requireArg(args, "resolve_agent_approval", "runId"));
      const callId = String(
        requireArg(args, "resolve_agent_approval", "callId")
      );
      const approved = Boolean(
        requireArg(args, "resolve_agent_approval", "approved")
      );
      const active = activeRuns.get(runId);
      if (!active)
        throw {
          kind: "notFound",
          message: `no active agent run with id ${runId}`,
        };
      if (!active.pendingApproval || active.pendingApproval.call_id !== callId)
        throw {
          kind: "notFound",
          message: "the run has no pending approval for that call",
        };
      // Clear auto-resolve timer? The pending was inside timers; easiest: mark pending null and emit.
      // We will clear all timers that would auto-resolve and handle now.
      // Find and clear the auto timer? We stored it as last timer; clear all for simplicity and re-schedule remaining steps manually.
      // But simpler: just handle approval logic now and cancel pending auto timer by clearing all and re-emitting remaining.
      for (const t of active.timers) clearTimeout(t);
      active.timers = [];
      const pending = active.pendingApproval;
      active.pendingApproval = null;
      emitAgentEvent({
        type: "governance",
        conversation_id: agentRuns.find((r) => r.id === runId)!
          .conversation_id!,
        run_id: runId,
        event: { type: "approval_resolved", call_id: callId, approved },
      });
      const steps = agentSteps.get(runId)!;
      const seq = steps.length + 1;
      const approvalText = approved ? "approved" : "denied";
      const sid = mockNextStepId++;
      steps.push({
        id: sid,
        run_id: runId,
        seq,
        kind: "approval",
        tool_name: pending.name,
        arguments: pending.arguments,
        observation: approvalText,
        observation_len: mockObsLen(approvalText),
        observation_truncated: false,
        status: approved ? "succeeded" : "denied",
        started_at: now(),
        duration_ms: null,
      });
      emitAgentEvent({
        type: "step",
        conversation_id: agentRuns.find((r) => r.id === runId)!
          .conversation_id!,
        run_id: runId,
        event: {
          step_id: sid,
          seq,
          kind: "approval",
          tool_name: pending.name,
          arguments: pending.arguments,
          observation: approvalText,
          observation_len: mockObsLen(approvalText),
          observation_truncated: false,
          status: approved ? "succeeded" : "denied",
          duration_ms: null,
        },
      });
      const run = agentRuns.find((r) => r.id === runId);
      const convId = run?.conversation_id ?? 0;
      const content =
        messages.find((m) => m.conversation_id === convId && m.role === "user")
          ?.content ?? "request";
      if (approved) {
        active.timers.push(
          window.setTimeout(() => {
            if (!activeRuns.has(runId)) return;
            const seq2 = agentSteps.get(runId)!.length + 1;
            const diff =
              "--- a/output.txt\n+++ b/output.txt\n@@ -0,0 +1,2 @@\n+hello world\n+second line\n";
            const sid2 = mockNextStepId++;
            agentSteps.get(runId)!.push({
              id: sid2,
              run_id: runId,
              seq: seq2,
              kind: "tool_call",
              tool_name: "write_file",
              arguments: pending.arguments,
              observation: diff,
              observation_len: mockObsLen(diff),
              observation_truncated: false,
              status: "succeeded",
              started_at: now(),
              duration_ms: 12,
            });
            emitAgentEvent({
              type: "step",
              conversation_id: agentRuns.find((r) => r.id === runId)!
                .conversation_id!,
              run_id: runId,
              event: {
                step_id: sid2,
                seq: seq2,
                kind: "tool_call",
                tool_name: "write_file",
                arguments: pending.arguments,
                observation: diff,
                observation_len: mockObsLen(diff),
                observation_truncated: false,
                status: "succeeded",
                duration_ms: 12,
              },
            });
          }, 400) as unknown as number
        );
        active.timers.push(
          window.setTimeout(() => {
            if (!activeRuns.has(runId)) return;
            const seq3 = agentSteps.get(runId)!.length + 1;
            const reply = `Mock agent reply to “${content}”.`;
            const sid3 = mockNextStepId++;
            agentSteps.get(runId)!.push({
              id: sid3,
              run_id: runId,
              seq: seq3,
              kind: "model_turn",
              tool_name: null,
              arguments: null,
              observation: reply,
              observation_len: mockObsLen(reply),
              observation_truncated: false,
              status: null,
              started_at: now(),
              duration_ms: 80,
            });
            emitAgentEvent({
              type: "step",
              conversation_id: agentRuns.find((r) => r.id === runId)!
                .conversation_id!,
              run_id: runId,
              event: {
                step_id: sid3,
                seq: seq3,
                kind: "model_turn",
                tool_name: null,
                arguments: null,
                observation: reply,
                observation_len: mockObsLen(reply),
                observation_truncated: false,
                status: null,
                duration_ms: 80,
              },
            });
            if (run) {
              run.status = "completed";
              run.finished_at = now();
              run.final_content = `Mock agent reply to “${content}”.`;
            }
            activeRuns.delete(runId);
            messages.push({
              id: nextMessageId++,
              conversation_id: convId,
              role: "assistant",
              content: `Mock agent reply to “${content}”.`,
              provider_id: 1,
              model_name: run?.model ?? "test",
              created_at: now(),
            });
            emitAgentEvent({
              type: "finished",
              run_id: runId,
              event: {
                conversation_id: convId,
                status: "completed",
                final_content: `Mock agent reply to “${content}”.`,
                error: null,
              },
            });
          }, 900) as unknown as number
        );
      } else {
        // Denied: record observation and finish with denied flow but still completed? For mock, treat as completed with denial note.
        active.timers.push(
          window.setTimeout(() => {
            if (!activeRuns.has(runId)) return;
            const seq2 = agentSteps.get(runId)!.length + 1;
            const sid2 = mockNextStepId++;
            agentSteps.get(runId)!.push({
              id: sid2,
              run_id: runId,
              seq: seq2,
              kind: "model_turn",
              tool_name: null,
              arguments: null,
              observation: `Request denied.`,
              observation_len: mockObsLen(`Request denied.`),
              observation_truncated: false,
              status: null,
              started_at: now(),
              duration_ms: 80,
            });
            emitAgentEvent({
              type: "step",
              conversation_id: agentRuns.find((r) => r.id === runId)!
                .conversation_id!,
              run_id: runId,
              event: {
                step_id: sid2,
                seq: seq2,
                kind: "model_turn",
                tool_name: null,
                arguments: null,
                observation: `Request denied.`,
                observation_len: mockObsLen(`Request denied.`),
                observation_truncated: false,
                status: null,
                duration_ms: 80,
              },
            });
            if (run) {
              run.status = "completed";
              run.finished_at = now();
              run.final_content = `Request denied.`;
            }
            activeRuns.delete(runId);
            emitAgentEvent({
              type: "finished",
              run_id: runId,
              event: {
                conversation_id: convId,
                status: "completed",
                final_content: `Request denied.`,
                error: null,
              },
            });
          }, 500) as unknown as number
        );
      }
      return null;
    }
    case "extend_agent_run": {
      const runId = Number(requireArg(args, "extend_agent_run", "runId"));
      const extra = Number(requireArg(args, "extend_agent_run", "extraSteps"));
      if (extra <= 0)
        throw {
          kind: "invalidInput",
          message: "extra steps must be greater than zero",
        };
      const active = activeRuns.get(runId);
      if (!active)
        throw {
          kind: "notFound",
          message: `no active agent run with id ${runId}`,
        };
      // If budget exhausted, clear flag and emit continuation steps
      if (active.budgetExhausted) {
        active.budgetExhausted = false;
        const run = agentRuns.find((r) => r.id === runId);
        if (run) run.status = "running";
        // Simulate extended steps
        active.timers.push(
          window.setTimeout(() => {
            if (!activeRuns.has(runId)) return;
            const steps = agentSteps.get(runId)!;
            const seq = steps.length + 1;
            const sid = mockNextStepId++;
            steps.push({
              id: sid,
              run_id: runId,
              seq,
              kind: "model_turn",
              tool_name: null,
              arguments: null,
              observation: "Continued after budget.",
              observation_len: mockObsLen("Continued after budget."),
              observation_truncated: false,
              status: null,
              started_at: now(),
              duration_ms: 80,
            });
            emitAgentEvent({
              type: "step",
              conversation_id: agentRuns.find((r) => r.id === runId)!
                .conversation_id!,
              run_id: runId,
              event: {
                step_id: sid,
                seq,
                kind: "model_turn",
                tool_name: null,
                arguments: null,
                observation: "Continued after budget.",
                observation_len: mockObsLen("Continued after budget."),
                observation_truncated: false,
                status: null,
                duration_ms: 80,
              },
            });
            if (run) {
              run.status = "completed";
              run.finished_at = now();
              run.final_content = "Continued after budget.";
            }
            activeRuns.delete(runId);
            emitAgentEvent({
              type: "finished",
              run_id: runId,
              event: {
                conversation_id: run?.conversation_id ?? 0,
                status: "completed",
                final_content: "Continued after budget.",
                error: null,
              },
            });
          }, 600) as unknown as number
        );
      } else {
        // Extend budget while running: just log, no immediate effect
      }
      return null;
    }
    case "agent_set_mode": {
      const runId = Number(requireArg(args, "agent_set_mode", "runId"));
      const mode = String(requireArg(args, "agent_set_mode", "mode"));
      if (
        !["supervised", "semi_autonomous", "full_autonomous"].includes(mode)
      ) {
        throw {
          kind: "invalidInput",
          message: `value '${mode}' is not a valid 'agent.autonomy' setting`,
        };
      }
      const active = activeRuns.get(runId);
      if (!active)
        throw {
          kind: "notFound",
          message: `no active agent run with id ${runId}`,
        };
      const run = agentRuns.find((r) => r.id === runId);
      if (run) run.mode = mode;
      // Mode switch never auto-resolves a parked approval (DP-AUTONOMY): do nothing to pendingApproval
      return null;
    }
    case "pause_agent_run": {
      const runId = Number(requireArg(args, "pause_agent_run", "runId"));
      const active = activeRuns.get(runId);
      if (!active)
        throw {
          kind: "notFound",
          message: `no active agent run with id ${runId}`,
        };
      if (!active.paused) {
        active.paused = true;
        emitAgentEvent({
          type: "governance",
          conversation_id: agentRuns.find((r) => r.id === runId)!
            .conversation_id!,
          run_id: runId,
          event: { type: "paused" },
        });
      }
      return null;
    }
    case "resume_agent_run": {
      const runId = Number(requireArg(args, "resume_agent_run", "runId"));
      const active = activeRuns.get(runId);
      if (!active)
        throw {
          kind: "notFound",
          message: `no active agent run with id ${runId}`,
        };
      if (active.paused) {
        active.paused = false;
        emitAgentEvent({
          type: "governance",
          conversation_id: agentRuns.find((r) => r.id === runId)!
            .conversation_id!,
          run_id: runId,
          event: { type: "resumed" },
        });
        // If we paused during a pending approval auto-resolve, the timers are still pending; resuming will let them fire
        // For budget-pending, simulate continuation if needed
        if (active.budgetExhausted) {
          // let extend handle it; just keep paused state cleared
        }
      }
      return null;
    }
    case "conversation_context_stats": {
      const convId = Number(
        requireArg(args, "conversation_context_stats", "conversationId")
      );
      const conv = conversations.find((c) => c.id === convId);
      if (!conv)
        throw {
          kind: "notFound",
          message: `conversation ${convId} does not exist`,
        };
      const convMessages = messages.filter((m) => m.conversation_id === convId);
      const userCount = convMessages.filter((m) => m.role === "user").length;
      const assistantCount = convMessages.filter(
        (m) => m.role === "assistant"
      ).length;
      const lastAssistant = [...convMessages]
        .reverse()
        .find((m) => m.role === "assistant");
      const runIds = agentRuns
        .filter((r) => r.conversation_id === convId)
        .map((r) => r.id);
      let toolCallCount = 0;
      let otherStepCount = 0;
      for (const rid of runIds) {
        for (const s of agentSteps.get(rid) ?? []) {
          if (s.kind === "tool_call") toolCallCount++;
          else otherStepCount++;
        }
      }
      return {
        conversation_id: conv.id,
        title: conv.title,
        provider: MOCK_DEFAULT_PROVIDER,
        provider_display:
          supported.find((p) => p.name === MOCK_DEFAULT_PROVIDER)
            ?.display_name ?? null,
        model: lastAssistant?.model_name ?? MOCK_DEFAULT_MODEL,
        context_limit: 128000,
        context_window: 128000,
        context_window_provenance: "built_in_catalog",
        category_breakdown: {
          system_skills_tokens: 180,
          messages_tokens: convMessages.reduce(
            (sum, m) => sum + Math.ceil(m.content.length / 4),
            0
          ),
          attachments_tokens: 0,
          tool_output_tokens: 0,
          total_accounted_tokens:
            180 +
            convMessages.reduce(
              (sum, m) => sum + Math.ceil(m.content.length / 4),
              0
            ),
        },
        compaction_count: 0,
        last_compacted_at: null,
        last_compaction: null,
        total_tokens: 0,
        input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        has_token_data: false,
        total_cost_micro_usd: 0,
        usage_percent: 0,
        message_count: convMessages.length,
        user_message_count: userCount,
        assistant_message_count: assistantCount,
        tool_call_count: toolCallCount,
        other_step_count: otherStepCount,
        created_at: conv.created_at,
        updated_at: conv.updated_at,
      };
    }
    case "compact_conversation_context": {
      const convId = Number(
        requireArg(args, "compact_conversation_context", "conversationId")
      );
      const convMessages = messages.filter((m) => m.conversation_id === convId);
      const now = Math.floor(Date.now() / 1000);
      if (convMessages.length === 0) {
        return {
          status: "nothing_to_compact",
          conversation_id: convId,
          run_id: null,
          event_id: null,
          reason: "manual",
          goal: "",
          active_plan: "",
          key_decisions: "",
          touched_files: [],
          summary: "",
          before_tokens: 0,
          after_tokens: 0,
          summarized_messages: 0,
          retained_messages: 0,
          created_at: now,
        };
      }
      return {
        status: "compacted",
        conversation_id: convId,
        run_id: null,
        event_id: 1,
        reason: "manual",
        goal: convMessages[0]?.content.slice(0, 120) ?? "none",
        active_plan: "none",
        key_decisions: "none",
        touched_files: [],
        summary: "1. Goal\n" + (convMessages[0]?.content ?? "none"),
        before_tokens: 500,
        after_tokens: 120,
        summarized_messages: Math.max(1, convMessages.length - 1),
        retained_messages: convMessages.length > 1 ? 1 : 0,
        created_at: now,
      };
    }
    case "inspect_run_live": {
      const conversationId = Number(
        requireArg(args, "inspect_run_live", "conversationId")
      );
      const run = [...agentRuns]
        .filter((row) => row.conversation_id === conversationId)
        .sort((a, b) => b.id - a.id)[0];
      if (!run) return null;
      const active = activeRuns.get(run.id);
      const pending = active?.pendingApproval;
      const steps = agentSteps.get(run.id) ?? [];
      const last = steps[steps.length - 1];
      return {
        run_id: run.id,
        status: run.status,
        parked_reason: active?.pendingQuestion
          ? "awaiting_question"
          : pending
            ? "awaiting_approval"
            : active?.budgetExhausted
              ? "awaiting_budget"
              : active?.paused
                ? "paused"
                : "none",
        pending_approval: pending
          ? {
              ...pending,
              timeout_secs: 600,
              expires_at: pending.requested_at + 600,
            }
          : null,
        pending_question: active?.pendingQuestion ?? null,
        started_at: run.started_at,
        finished_at: run.finished_at,
        current_step_seq: last?.seq ?? null,
        current_step_tool: last?.tool_name ?? null,
      };
    }
    case "list_agent_runs": {
      const convId = Number(
        requireArg(args, "list_agent_runs", "conversationId")
      );
      return [...agentRuns]
        .filter((r) => r.conversation_id === convId)
        .sort((a, b) => b.started_at - a.started_at);
    }
    case "list_run_checkpoints": {
      void Number(requireArg(args, "list_run_checkpoints", "runId"));
      return [];
    }
    case "revert_file_checkpoint": {
      const checkpointId = Number(
        requireArg(args, "revert_file_checkpoint", "checkpointId")
      );
      throw {
        kind: "notFound",
        message: `Checkpoint ${checkpointId} not found in mock session`,
      };
    }
    case "revert_turn_checkpoints": {
      void Number(requireArg(args, "revert_turn_checkpoints", "runId"));
      void Number(requireArg(args, "revert_turn_checkpoints", "turnIndex"));
      return [];
    }
    case "list_run_plans": {
      const conversationId = Number(
        requireArg(args, "list_run_plans", "conversationId")
      );
      return mockRunPlans.filter((p) => p.conversation_id === conversationId);
    }
    case "decide_run_plan": {
      const runId = Number(requireArg(args, "decide_run_plan", "runId"));
      const rawDecision = String(
        requireArg(args, "decide_run_plan", "decision")
      );
      const normalizedDecision: PlanDecisionKind =
        rawDecision === "yes" || rawDecision === "accepted"
          ? "accepted"
          : rawDecision === "no" || rawDecision === "rejected"
            ? "rejected"
            : rawDecision === "custom_reply"
              ? "custom_reply"
              : (() => {
                  throw {
                    kind: "invalidInput",
                    message: `invalid plan decision: ${rawDecision}`,
                  };
                })();
      const reply =
        typeof args.reply === "string" && args.reply.trim()
          ? args.reply.trim()
          : null;
      const plan = mockRunPlans.find((p) => p.run_id === runId);
      if (!plan) {
        throw { kind: "notFound", message: `no plan found for run ${runId}` };
      }
      if (plan.decision_status !== "pending") {
        throw {
          kind: "invalidInput",
          message: "plan decision already recorded",
        };
      }
      if (normalizedDecision === "custom_reply" && !reply) {
        throw {
          kind: "invalidInput",
          message: "custom reply text must not be empty",
        };
      }
      const nextStatus: PlanDecisionStatus = normalizedDecision;
      let launchedRunId: number | null = null;
      if (
        normalizedDecision === "accepted" ||
        normalizedDecision === "custom_reply"
      ) {
        const model =
          typeof args.model === "string" && args.model.trim()
            ? args.model
            : "gpt-5.6-terra";
        const instruction =
          normalizedDecision === "custom_reply" && reply
            ? reply
            : "Proceed with implementation of the approved plan.";
        launchedRunId = spawnSimpleMockContinuationOrImpl({
          convId: plan.conversation_id,
          model,
          summaryText: `Mock implementation complete (${instruction}).`,
          continuedFromRunId: null,
        });
      }
      const ts = now();
      plan.decision_status = nextStatus;
      plan.decision_reply =
        normalizedDecision === "custom_reply" ? reply : null;
      plan.implemented_run_id = launchedRunId;
      plan.decided_at = ts;
      const run = agentRuns.find((r) => r.id === runId);
      if (run) {
        run.plan_decision_status = nextStatus;
        run.plan_decision_reply = plan.decision_reply;
        run.plan_implemented_run_id = launchedRunId;
      }
      const result: PlanDecisionResult = {
        plan: { ...plan },
        launched_run_id: launchedRunId,
      };
      return result;
    }
    case "continue_agent_run": {
      const runId = Number(requireArg(args, "continue_agent_run", "runId"));
      const model = String(requireArg(args, "continue_agent_run", "model"));
      const parent = agentRuns.find((r) => r.id === runId);
      if (!parent || parent.conversation_id === null) {
        throw { kind: "notFound", message: `no agent run with id ${runId}` };
      }
      if (!parent.interrupted && parent.status !== "interrupted") {
        throw {
          kind: "invalidInput",
          message: `agent run ${runId} is not interrupted`,
        };
      }
      for (const [rid] of activeRuns) {
        const ar = agentRuns.find((x) => x.id === rid);
        if (ar && ar.conversation_id === parent.conversation_id) {
          throw {
            kind: "invalidInput",
            message: `an agent run is already active for conversation ${parent.conversation_id}`,
          };
        }
      }
      const newRunId = spawnSimpleMockContinuationOrImpl({
        convId: parent.conversation_id,
        model,
        summaryText: `Continued safely from interrupted run #${runId}.`,
        continuedFromRunId: runId,
      });
      return { run_id: newRunId };
    }
    case "list_run_questions": {
      const conversationId = Number(
        requireArg(args, "list_run_questions", "conversationId")
      );
      return mockRunQuestions.filter(
        (q) => q.conversation_id === conversationId
      );
    }
    case "answer_run_question": {
      const runId = Number(requireArg(args, "answer_run_question", "runId"));
      const questionId = Number(
        requireArg(args, "answer_run_question", "questionId")
      );
      const selectedOptions = Array.isArray(args.selectedOptions)
        ? args.selectedOptions.map(String)
        : [];
      const freeText =
        typeof args.freeText === "string" && args.freeText.trim()
          ? args.freeText.trim()
          : null;
      const row = mockRunQuestions.find(
        (q) =>
          q.id === questionId && q.run_id === runId && q.status === "pending"
      );
      if (!row) {
        throw { kind: "notFound", message: "question is not pending" };
      }
      row.status = "answered";
      row.selected_options = selectedOptions;
      row.free_text = freeText;
      row.answered_at = Math.floor(Date.now() / 1000);
      const active = activeRuns.get(runId);
      if (active) active.pendingQuestion = null;
      emitAgentEvent({
        type: "governance",
        run_id: runId,
        conversation_id: row.conversation_id,
        event: {
          type: "question_resolved",
          question_id: row.id,
          call_id: row.call_id,
          status: "answered",
        },
      });
      return { ...row };
    }
    case "clarify_agent_run": {
      const runId = Number(requireArg(args, "clarify_agent_run", "runId"));
      const content = String(requireArg(args, "clarify_agent_run", "content"));
      const run = agentRuns.find((r) => r.id === runId);
      if (!run) {
        throw {
          kind: "notFound",
          message: `no active agent run with id ${runId}`,
        };
      }
      emitAgentEvent({
        type: "governance",
        run_id: runId,
        conversation_id: run.conversation_id,
        event: {
          type: "steering_injected",
          content,
        },
      });
      return null;
    }
    case "enqueue_work_item": {
      const conversationId = Number(
        requireArg(args, "enqueue_work_item", "conversationId")
      );
      const content = String(
        requireArg(args, "enqueue_work_item", "content")
      ).trim();
      const provider = String(
        requireArg(args, "enqueue_work_item", "provider")
      );
      const model = String(requireArg(args, "enqueue_work_item", "model"));
      const effort = typeof args.effort === "string" ? args.effort : null;
      const existing = mockWorkQueue.filter(
        (w) => w.conversation_id === conversationId && w.status === "queued"
      );
      const now = Math.floor(Date.now() / 1000);
      const item: MockWorkQueueItem = {
        id: nextWorkQueueId++,
        conversation_id: conversationId,
        content,
        provider,
        model,
        effort,
        position: existing.length,
        status: "queued",
        created_at: now,
        updated_at: now,
      };
      mockWorkQueue.push(item);
      emitWorkQueueUpdated(conversationId);
      return { ...item };
    }
    case "list_work_queue": {
      const conversationId = Number(
        requireArg(args, "list_work_queue", "conversationId")
      );
      return mockWorkQueue
        .filter(
          (w) => w.conversation_id === conversationId && w.status === "queued"
        )
        .sort((a, b) => a.position - b.position || a.id - b.id);
    }
    case "update_work_queue_item": {
      const itemId = Number(
        requireArg(args, "update_work_queue_item", "itemId")
      );
      const content = String(
        requireArg(args, "update_work_queue_item", "content")
      ).trim();
      const item = mockWorkQueue.find(
        (w) => w.id === itemId && w.status === "queued"
      );
      if (!item) {
        throw { kind: "notFound", message: "queued item not found" };
      }
      item.content = content;
      item.updated_at = Math.floor(Date.now() / 1000);
      emitWorkQueueUpdated(item.conversation_id);
      return { ...item };
    }
    case "reorder_work_queue": {
      const conversationId = Number(
        requireArg(args, "reorder_work_queue", "conversationId")
      );
      const orderedIds = Array.isArray(args.orderedIds)
        ? args.orderedIds.map(Number)
        : [];
      const queued = mockWorkQueue.filter(
        (w) => w.conversation_id === conversationId && w.status === "queued"
      );
      let pos = 0;
      for (const id of orderedIds) {
        const found = queued.find((w) => w.id === id);
        if (found) found.position = pos++;
      }
      for (const w of queued) {
        if (!orderedIds.includes(w.id)) w.position = pos++;
      }
      emitWorkQueueUpdated(conversationId);
      return queued.sort((a, b) => a.position - b.position || a.id - b.id);
    }
    case "cancel_work_queue_item": {
      const itemId = Number(
        requireArg(args, "cancel_work_queue_item", "itemId")
      );
      const item = mockWorkQueue.find(
        (w) => w.id === itemId && w.status === "queued"
      );
      if (!item) {
        throw { kind: "notFound", message: "queued item not found" };
      }
      item.status = "cancelled";
      emitWorkQueueUpdated(item.conversation_id);
      return null;
    }
    case "run_next_work_item": {
      const conversationId = Number(
        requireArg(args, "run_next_work_item", "conversationId")
      );
      const next = mockWorkQueue
        .filter(
          (w) => w.conversation_id === conversationId && w.status === "queued"
        )
        .sort((a, b) => a.position - b.position || a.id - b.id)[0];
      if (!next) return null;
      next.status = "completed";
      emitWorkQueueUpdated(conversationId);
      return { run_id: Date.now() };
    }
    // The visual mock predates the journal; its existing run/step history
    // stays readable through the legacy APIs without inventing chronology.
    case "list_conversation_run_events":
      return {
        events: [],
        next_cursor: Number(args.afterEventId ?? 0),
        high_water_event_id: Number(args.afterEventId ?? 0),
        has_more: false,
      };
    case "list_agent_steps": {
      const runId = Number(requireArg(args, "list_agent_steps", "runId"));
      const steps = agentSteps.get(runId) ?? [];
      return [...steps].sort((a, b) => a.seq - b.seq);
    }
    case "get_agent_step_observation": {
      const stepId = Number(
        requireArg(args, "get_agent_step_observation", "stepId")
      );
      for (const steps of agentSteps.values()) {
        const found = steps.find((s) => s.id === stepId);
        if (found) return found.observation ?? "";
      }
      throw { kind: "notFound", message: `no agent step with id ${stepId}` };
    }
    case "activity_feed": {
      // Read-only aggregate mirror of the backend `activity_feed` command:
      // capped secret-free run rows (newest first) plus cross-run totals.
      // Rows deliberately omit `final_content`/`error` (secret-free rule).
      const rawLimit = args.limit;
      const parsed =
        typeof rawLimit === "number" && Number.isFinite(rawLimit)
          ? Math.floor(rawLimit)
          : 50;
      const count = Math.min(100, Math.max(1, parsed));
      const ordered = [...agentRuns].sort(
        (a, b) => b.started_at - a.started_at
      );
      const runs = ordered.slice(0, count).map((r) => ({
        run_id: r.id,
        conversation_id: r.conversation_id,
        model: r.model,
        mode: r.mode,
        status: r.status,
        started_at: r.started_at,
        finished_at: r.finished_at,
        total_steps: r.total_steps,
        spent_micro_usd: r.spent_micro_usd,
        limit_micro_usd: r.limit_micro_usd,
      }));
      let totalSteps = 0;
      let totalSpent = 0;
      let withSpend = 0;
      let withLimit = 0;
      for (const r of agentRuns) {
        totalSteps += r.total_steps;
        if (r.spent_micro_usd !== null) {
          totalSpent += r.spent_micro_usd;
          withSpend++;
        }
        if (r.limit_micro_usd !== null) withLimit++;
      }
      return {
        runs,
        totals: {
          runs: agentRuns.length,
          total_steps: totalSteps,
          total_spent_micro_usd: totalSpent,
          runs_with_spend: withSpend,
          runs_with_limit: withLimit,
        },
      };
    }
    case "plugin:event|listen": {
      const event = String(args.event);
      const handler = args.handler as unknown as (ev: unknown) => void;
      if (typeof handler !== "function") return 0;
      const id = nextEventId++;
      if (!eventListeners.has(event)) eventListeners.set(event, new Map());
      eventListeners.get(event)!.set(id, handler);
      return id;
    }
    case "plugin:event|unlisten": {
      const event = String(args.event);
      const eventId = Number(args.eventId ?? args.event_id ?? args.id);
      const map = eventListeners.get(event);
      if (map) map.delete(eventId);
      return null;
    }
    // Tauri plugin surfaces used by the import/export flow.
    case "plugin:dialog|save":
      return "C:\\mock\\conversation.json";
    case "plugin:dialog|open":
      return "C:\\mock\\conversation.json";
    case "plugin:fs|read_text_file":
      return JSON.stringify({ version: 1, title: "Imported conversation" });
    case "privacy_status": {
      // Static DEV-ONLY mirror of the backend `privacy_status` command:
      // surface inventory with honest states plus counts-only aggregates.
      const day = Math.floor(Date.now() / 1000 / 86400);
      return {
        surfaces: [
          {
            id: "provider:openai",
            title: "OpenAI API",
            destination: "api.openai.com",
            state: "off",
            detail: "no key stored — sends nothing",
          },
          {
            id: "github_api",
            title: "GitHub issues / PRs / Actions",
            destination: "api.github.com",
            state: "manual",
            detail: "no token — unauthenticated reads on panel open only",
          },
          {
            id: "release_check",
            title: "Release check",
            destination: "api.github.com (releases/latest)",
            state: "manual",
            detail: "button only — never automatic",
          },
          {
            id: "crash_upload",
            title: "Crash upload",
            destination: "none",
            state: "off",
            detail: "no crash upload exists",
          },
          {
            id: "usage_ledger",
            title: "Usage ledger",
            destination: "local SQLite (usage_ledger, 90 days)",
            state: "local",
            detail: "per-day counters only",
          },
        ],
        stats: [{ kind: "message", day, count: 3 }],
        total_events: 3,
        retention_days: 90,
        oldest_day: day,
      };
    }
    case "privacy_export": {
      const day = Math.floor(Date.now() / 1000 / 86400);
      return {
        exported_at: Math.floor(Date.now() / 1000),
        retention_days: 90,
        stats: [{ kind: "message", day, count: 3 }],
      };
    }
    case "privacy_wipe":
      return { deleted_rows: 1 };
    case "terminal_run": {
      const cmd = String(args.command ?? "").trim();
      const confirmationId = String(args.confirmationId ?? "");
      if (!cmd) {
        throw {
          kind: "invalidInput",
          message: "the terminal command must not be empty",
        };
      }
      if (!confirmationId) {
        throw {
          kind: "confirmationRequired",
          message: "explicit confirmation is required before running terminal",
        };
      }
      if (activeMockTerminalKill !== null) {
        throw {
          kind: "invalidInput",
          message: "a terminal command is already running — stop it first",
        };
      }
      const runId = nextMockTerminalRunId++;
      const cwd = typeof args.cwd === "string" && args.cwd ? args.cwd : ".";
      const scrubMockSecrets = (text: string) =>
        text
          .replace(/sk-(?:ant-|proj-)?[A-Za-z0-9_-]{8,}/g, "[REDACTED]")
          .replace(/Bearer\s+[A-Za-z0-9._-]+/gi, "Bearer [REDACTED]");
      const shouldFail = /\bfail\b|\bexit\s+1\b|\berr\b/i.test(cmd);
      const line1 = scrubMockSecrets(`[${cwd}] $ ${cmd}\n`);
      const line2 = shouldFail
        ? "stderr: mock non-zero exit status 1\n"
        : "mock ok: finished cleanly\n";

      emitTerminalOutput({
        run_id: runId,
        stream: "status",
        chunk: "",
        done: false,
        truncated: false,
        killed: false,
        success: null,
      });

      return new Promise((resolve, reject) => {
        let settled = false;
        const timer1 = window.setTimeout(() => {
          if (settled) return;
          emitTerminalOutput({
            run_id: runId,
            stream: "stdout",
            chunk: line1,
            done: false,
            truncated: false,
            killed: false,
            success: null,
          });
        }, 25);
        const timer2 = window.setTimeout(() => {
          if (settled) return;
          settled = true;
          activeMockTerminalKill = null;
          emitTerminalOutput({
            run_id: runId,
            stream: shouldFail ? "stderr" : "stdout",
            chunk: line2,
            done: false,
            truncated: false,
            killed: false,
            success: null,
          });
          const output = shouldFail
            ? `${line1}--- stderr ---\n${line2}\n[command exited with status 1]`
            : `${line1}${line2}`;
          emitTerminalOutput({
            run_id: runId,
            stream: "status",
            chunk: "",
            done: true,
            truncated: false,
            killed: false,
            success: !shouldFail,
          });
          resolve({
            run_id: runId,
            output,
            truncated: false,
            success: !shouldFail,
          });
        }, 75);
        activeMockTerminalKill = () => {
          if (settled) return;
          settled = true;
          window.clearTimeout(timer1);
          window.clearTimeout(timer2);
          activeMockTerminalKill = null;
          emitTerminalOutput({
            run_id: runId,
            stream: "status",
            chunk: "",
            done: true,
            truncated: false,
            killed: true,
            success: false,
          });
          reject({
            kind: "cancelled",
            message: "terminal command cancelled",
          });
        };
      });
    }
    case "terminal_kill": {
      if (activeMockTerminalKill) {
        activeMockTerminalKill();
        return true;
      }
      return false;
    }
    case "terminal_explain": {
      const out = String(args.output ?? "");
      return {
        explanation:
          "Mock diagnosis: the command returned a non-zero exit status." +
          (out ? " Review the command path, arguments, and stderr above." : ""),
        suggested_fix: 'echo "fixed" && npm test',
        truncated_input: false,
      };
    }
    case "terminal_pty_spawn": {
      const sessionId = `mock-pty-${nextMockPtyId++}`;
      const cols = typeof args.cols === "number" ? args.cols : 80;
      const rows = typeof args.rows === "number" ? args.rows : 24;
      const cwd = mockWorkspaceRoot || "C:\\mock\\workspace";
      const shellName = "pwsh";
      mockPtySessions.set(sessionId, {
        sessionId,
        shellName,
        cwd,
        cols,
        rows,
        lineBuffer: "",
      });
      window.setTimeout(() => {
        if (!mockPtySessions.has(sessionId)) return;
        emitTerminalPtyOutput(sessionId, `PS ${cwd}> `);
      }, 0);
      return {
        session_id: sessionId,
        shell_name: shellName,
        cwd,
      };
    }
    case "terminal_pty_write": {
      const sessionId = String(args.sessionId ?? "");
      const data = String(args.data ?? "");
      const session = mockPtySessions.get(sessionId);
      if (!session) {
        throw {
          kind: "notFound",
          message: "the terminal session is no longer active",
        };
      }
      for (const ch of data) {
        if (ch === "\x03") {
          session.lineBuffer = "";
          emitTerminalPtyOutput(sessionId, `^C\r\nPS ${session.cwd}> `);
        } else if (ch === "\r" || ch === "\n") {
          const cmd = session.lineBuffer.trim();
          session.lineBuffer = "";
          if (cmd === "exit") {
            emitTerminalPtyOutput(sessionId, "\r\n");
            mockPtySessions.delete(sessionId);
            emitTerminalPtyExit(sessionId, 0);
            break;
          } else if (cmd === "clear" || cmd === "cls") {
            emitTerminalPtyOutput(
              sessionId,
              `\x1b[2J\x1b[HPS ${session.cwd}> `
            );
          } else if (cmd === "pwd") {
            emitTerminalPtyOutput(
              sessionId,
              `\r\n${session.cwd}\r\nPS ${session.cwd}> `
            );
          } else if (cmd.startsWith("echo ")) {
            emitTerminalPtyOutput(
              sessionId,
              `\r\n${cmd.slice(5)}\r\nPS ${session.cwd}> `
            );
          } else if (cmd.length > 0) {
            emitTerminalPtyOutput(
              sessionId,
              `\r\n[mock-pty] ${cmd}\r\nPS ${session.cwd}> `
            );
          } else {
            emitTerminalPtyOutput(sessionId, `\r\nPS ${session.cwd}> `);
          }
        } else if (ch === "\x7f" || ch === "\b") {
          if (session.lineBuffer.length > 0) {
            session.lineBuffer = session.lineBuffer.slice(0, -1);
            emitTerminalPtyOutput(sessionId, "\b \b");
          }
        } else {
          session.lineBuffer += ch;
          emitTerminalPtyOutput(sessionId, ch);
        }
      }
      return null;
    }
    case "terminal_pty_resize": {
      const sessionId = String(args.sessionId ?? "");
      const session = mockPtySessions.get(sessionId);
      if (!session) {
        throw {
          kind: "notFound",
          message: "the terminal session is no longer active",
        };
      }
      if (typeof args.cols === "number") session.cols = args.cols;
      if (typeof args.rows === "number") session.rows = args.rows;
      return null;
    }
    case "terminal_pty_kill": {
      const sessionId = String(args.sessionId ?? "");
      const existed = mockPtySessions.delete(sessionId);
      if (existed) {
        emitTerminalPtyExit(sessionId, 0);
      }
      return existed;
    }
    case "mcp_list_servers": {
      const raw = settings.get("mcp.servers");
      if (!raw) return [];
      try {
        const list = JSON.parse(raw) as Record<string, unknown>[];
        return Array.isArray(list)
          ? list.map((item) => ({
              transport: "stdio",
              enabled: true,
              command: "",
              args: [],
              env: {},
              url: null,
              oauth: null,
              has_auth: false,
              auth_kind: "none",
              expires_at: null,
              ...item,
            }))
          : [];
      } catch {
        return [];
      }
    }
    case "mcp_upsert_server": {
      const server = args.server as Record<string, unknown>;
      const raw = settings.get("mcp.servers");
      const list: Record<string, unknown>[] = raw ? JSON.parse(raw) : [];
      const idx = list.findIndex((s) => s.name === server.name);
      if (idx >= 0) list[idx] = server;
      else list.push(server);
      settings.set("mcp.servers", JSON.stringify(list));
      return list.map((item) => ({
        has_auth: false,
        auth_kind: "none",
        expires_at: null,
        ...item,
      }));
    }
    case "mcp_remove_server": {
      const name = String(args.name ?? "");
      const raw = settings.get("mcp.servers");
      const list: Record<string, unknown>[] = raw ? JSON.parse(raw) : [];
      const next = list.filter((s) => s.name !== name);
      settings.set("mcp.servers", JSON.stringify(next));
      return next.map((item) => ({
        has_auth: false,
        auth_kind: "none",
        expires_at: null,
        ...item,
      }));
    }
    case "mcp_set_server_enabled": {
      const name = String(args.name ?? "");
      const enabled = Boolean(args.enabled);
      const raw = settings.get("mcp.servers");
      const list: Record<string, unknown>[] = raw ? JSON.parse(raw) : [];
      const next = list.map((s) => (s.name === name ? { ...s, enabled } : s));
      settings.set("mcp.servers", JSON.stringify(next));
      return next.map((item) => ({
        has_auth: false,
        auth_kind: "none",
        expires_at: null,
        ...item,
      }));
    }
    case "mcp_list_server_tools":
      return [];
    case "mcp_set_bearer_token":
    case "mcp_clear_auth":
      return null;
    case "mcp_start_oauth":
    case "mcp_refresh_oauth":
      return { has_auth: true, auth_kind: "oauth", expires_at: null };
    case "docs_manifest": {
      // Static DEV-ONLY mirror of the backend `docs_manifest` command: the
      // migration spine mirrors MIGRATIONS v1..v11 (database.rs) — update
      // this list when a migration lands so visual QA keeps showing the
      // full guide. Production always reads the backend-computed spine.
      const migrations = Array.from({ length: 11 }, (_, index) => ({
        version: index + 1,
      }));
      return { schema_version: 11, migrations };
    }
    case "git_info":
      return {
        branch: "main",
        files: [
          { path: "src/App.tsx", status: "modified" },
          { path: "src/components/VersionControlPanel.tsx", status: "staged" },
        ],
        files_overflow: 0,
        commits: [
          {
            hash: "1234567890abcdef1234567890abcdef12345678",
            message: "feat(vcs): extend git workflow",
            author: "developer",
            time: Math.floor(Date.now() / 1000) - 3600,
            files_changed: 3,
            insertions: 48,
            deletions: 12,
            risk_signals: [],
          },
        ],
        conflicts: [],
        merge_in_progress: false,
      };
    case "git_file_diff":
      return {
        path: String(args.path ?? ""),
        diff: "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n",
        truncated: false,
        binary: false,
      };
    case "git_commit_diff":
      return {
        hash: String(args.hash ?? ""),
        diff: "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n",
        truncated: false,
        binary: false,
        files: ["src/App.tsx"],
        files_overflow: 0,
      };
    case "git_fetch":
      return { remote: String(args.remote ?? "origin"), updated_refs: 1 };
    case "git_pull":
      return { status: "up_to_date", commit_hash: null, conflicts: [] };
    case "git_branches":
      return [
        {
          name: "main",
          is_current: true,
          upstream: "origin/main",
          last_commit_hash: "1234567890abcdef1234567890abcdef12345678",
        },
      ];
    case "git_branch_create":
      return {
        name: String(args.name ?? "feature"),
        is_current: Boolean(args.switch),
        upstream: null,
        last_commit_hash: "1234567890abcdef1234567890abcdef12345678",
      };
    case "git_branch_switch":
    case "git_branch_delete":
    case "git_abort_merge":
    case "git_push":
      return null;
    case "git_stage":
    case "git_unstage":
      return Array.isArray(args.paths) ? args.paths.length : 1;
    case "git_commit":
      return "abcdef1234567890abcdef1234567890abcdef12";
    case "git_stash_list":
      return [];
    case "git_stash_push":
      return {
        index: 0,
        message: String(args.message ?? "WIP"),
        hash: "abcdef1234567890abcdef1234567890abcdef12",
      };
    case "git_stash_pop":
      return { status: "applied", conflicts: [] };
    case "git_conflict_diff":
      return {
        path: String(args.path ?? ""),
        status: "UU",
        ours: "local version\n",
        theirs: "incoming version\n",
        working:
          "<<<<<<< HEAD\nlocal version\n=======\nincoming version\n>>>>>>> origin/main\n",
        truncated: false,
        binary: false,
      };
    case "git_resolve_conflict":
      return 0;
    case "git_prepare_review_diff":
      return {
        scope: String(args.scope ?? "working"),
        base_branch: args.baseBranch ? String(args.baseBranch) : null,
        files: ["src/App.tsx"],
        files_overflow: 0,
        diff: "--- a/src/App.tsx\n+++ b/src/App.tsx\n@@ -1 +1 @@\n-old\n+new\n",
        truncated: false,
        redacted: false,
        prompt:
          "/review\n\nReview the current working-tree changes (1 file(s)).\n",
      };
    default:
      fail(`Mock backend: unhandled command “${command}”.`);
  }
}

if (!(
  "__TAURI_INTERNALS__" in window &&
  (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__
)) {
  Object.defineProperty(window, "__TAURI_INTERNALS__", {
    configurable: true,
    value: {
      invoke,
      transformCallback: (callback: unknown) => callback,
      metadata: {
        currentWindow: { label: "mock" },
        currentWebview: { label: "mock" },
      },
    },
  });
}
