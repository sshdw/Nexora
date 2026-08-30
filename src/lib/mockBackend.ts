//! DEV-ONLY visual-QA mock backend — never active in production.
//! Updated for Task 5.1 with agent run parity.

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

const conversations = [
  { id: 1, title: "Designing the composer", status: "active", created_at: hoursAgo(30), updated_at: hoursAgo(1) },
  { id: 2, title: "Rust migration notes", status: "active", created_at: hoursAgo(50), updated_at: hoursAgo(5) },
  { id: 3, title: "Old research thread", status: "archived", created_at: hoursAgo(200), updated_at: hoursAgo(96) },
];

const messages: MockMessage[] = [
  { id: 1, conversation_id: 1, role: "user", content: "Hello", provider_id: null, model_name: null, created_at: hoursAgo(1) },
  { id: 2, conversation_id: 1, role: "assistant", content: "Hello! How can I help you today?", provider_id: 1, model_name: "gemini-3.6-flash", created_at: hoursAgo(1) + 20 },
  { id: 3, conversation_id: 1, role: "user", content: "Summarize the Material 3 Expressive motion system in two sentences.", provider_id: null, model_name: null, created_at: hoursAgo(1) + 60 },
];

const prompts = [
  { id: 1, title: "Weekly review", content: "Summarize this week's progress, list blockers, and propose the top three priorities for next week.", created_at: hoursAgo(20), updated_at: hoursAgo(2) },
  { id: 2, title: "Code review checklist", content: "Review the attached diff for correctness, edge cases, naming, and test coverage. Flag anything that changes public behavior.", created_at: hoursAgo(40), updated_at: hoursAgo(10) },
];

const providers = [{ id: 1, name: "gemini", display_name: "Gemini" }];
const credentialed = new Set<string>(["gemini"]);
const settings = new Map<string, string>([
  ["provider.selected", "gemini"],
  ["provider.model", "gemini-3.6-flash"],
  ["appearance.theme", "dark"],
]);

const attachments: Array<{
  id: number;
  conversation_id: number;
  message_id: number | null;
  file_name: string;
  file_path: string;
  file_size_bytes: number | null;
  mime_type: string | null;
}> = [];

const supported = [
  {
    name: "openai",
    display_name: "OpenAI",
    models: ["gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.6-sol"],
  },
  {
    name: "anthropic",
    display_name: "Anthropic",
    models: ["claude-sonnet-5", "claude-haiku-4-5-20251001", "claude-opus-4-8"],
  },
  {
    name: "gemini",
    display_name: "Gemini",
    models: ["gemini-3.6-flash", "gemini-3.1-flash-lite", "gemini-3.1-pro-preview"],
  },
];

// ---- Agent run mock state (Task 5.1) ----
let nextAgentRunId = 1;
interface MockAgentRun {
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
}
interface MockAgentStep {
  id: number;
  run_id: number;
  seq: number;
  kind: string;
  tool_name: string | null;
  arguments: string | null;
  observation: string | null;
  status: string | null;
  started_at: number;
  duration_ms: number | null;
}
const agentRuns: MockAgentRun[] = [];
const agentSteps = new Map<number, MockAgentStep[]>();
let mockNextStepId = 1;

type ActiveMockRun = {
  status: string;
  timers: number[];
  pendingApproval: { call_id: string; name: string; arguments: string } | null;
  budgetExhausted: boolean;
};
const activeRuns = new Map<number, ActiveMockRun>();

// Event channel mock
let nextEventId = 1;
const eventListeners = new Map<string, Map<number, (ev: unknown) => void>>();

function emitAgentEvent(frame: unknown) {
  const listeners = eventListeners.get("agent-run-event");
  if (!listeners) return;
  for (const [, cb] of listeners) {
    try {
      (cb as (v: unknown) => void)({ event: "agent-run-event", id: 0, payload: frame });
    } catch {
      // ignore handler errors
    }
  }
}

function argNumber(args: Record<string, unknown>, ...keys: string[]): number | undefined {
  for (const k of keys) if (k in args && args[k] !== undefined && args[k] !== null) return Number(args[k]);
  return undefined;
}
function argString(args: Record<string, unknown>, ...keys: string[]): string | undefined {
  for (const k of keys) if (k in args && args[k] !== undefined && args[k] !== null) return String(args[k]);
  return undefined;
}
function argBool(args: Record<string, unknown>, ...keys: string[]): boolean | undefined {
  for (const k of keys) if (k in args && args[k] !== undefined) return Boolean(args[k]);
  return undefined;
}

function fail(message: string): never {
  throw { kind: "unknown", message };
}

async function invoke(command: string, args: Record<string, unknown> = {}): Promise<unknown> {
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
    case "delete_conversation": {
      const id = Number(args.id);
      const index = conversations.findIndex((c) => c.id === id);
      if (index >= 0) conversations.splice(index, 1);
      for (let i = messages.length - 1; i >= 0; i--) {
        if (messages[i].conversation_id === id) messages.splice(i, 1);
      }
      // cascade agent runs
      for (let i = agentRuns.length - 1; i >= 0; i--) if (agentRuns[i].conversation_id === id) agentRuns.splice(i, 1);
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
      return providers.some((p) => p.name === (args.name as string)) && credentialed.has(String(args.name));
    case "has_provider_credential":
      return credentialed.has(String(args.provider));
    case "add_provider_credential":
    case "update_provider_credential":
      credentialed.add(String(args.provider));
      return null;
    case "remove_provider_credential":
      credentialed.delete(String(args.provider));
      return null;
    case "get_setting":
      return settings.get(String(args.key)) ?? null;
    case "set_setting":
      if (args.value === null || args.value === undefined) settings.delete(String(args.key));
      else settings.set(String(args.key), String(args.value));
      return null;
    case "delete_setting":
      settings.delete(String(args.key));
      return null;
    case "clear_application_data": {
      if (args.confirmation !== "confirm") fail("Confirmation phrase mismatch.");
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
        conversations: conversations.filter((c) => c.title.toLowerCase().includes(q)),
        message_matches: messages.filter((m) => m.content.toLowerCase().includes(q)),
        prompts: prompts.filter(
          (p) =>
            p.title.toLowerCase().includes(q) || p.content.toLowerCase().includes(q),
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
      if (args.confirmation !== "confirm") fail("Confirmation phrase mismatch.");
      const index = prompts.findIndex((p) => p.id === Number(args.id));
      if (index >= 0) prompts.splice(index, 1);
      return null;
    }
    case "export_conversation_to_file":
      return null;
    case "import_conversation": {
      try {
        JSON.parse(String(args.json));
      } catch {
        fail("Unsupported file.");
      }
      const id = nextConversationId++;
      conversations.push({
        id,
        title: "Imported conversation",
        status: "active",
        created_at: now(),
        updated_at: now(),
      });
      return id;
    }
    case "attach_file": {
      const id = nextAttachmentId++;
      attachments.push({
        id,
        conversation_id: Number(args.conversationId ?? args.conversation_id),
        message_id: null,
        file_name: String(args.fileName ?? args.file_name),
        file_path: String(args.filePath ?? args.file_path),
        file_size_bytes: typeof (args.fileSizeBytes ?? args.file_size_bytes) === "number" ? (args.fileSizeBytes ?? args.file_size_bytes) as number : null,
        mime_type: typeof (args.mimeType ?? args.mime_type) === "string" ? (args.mimeType ?? args.mime_type) as string : null,
      });
      return attachments[attachments.length - 1];
    }
    case "list_attachments":
      return attachments.filter(
        (a) => a.conversation_id === Number(args.conversationId ?? args.conversation_id) && a.message_id === null,
      );
    case "remove_attachment": {
      const index = attachments.findIndex((a) => a.id === Number(args.id));
      if (index >= 0) attachments.splice(index, 1);
      return null;
    }
    case "send_message": {
      const conversationId = Number(args.conversationId ?? args.conversation_id);
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
      for (const id of ((args.attachmentIds ?? args.attachment_ids) as number[]) ?? []) {
        const draft = attachments.find((a) => a.id === id);
        if (draft) draft.message_id = userId;
      }
      return { content: "ok", model: String(args.model) };
    }
    // ---- Agent runs (Task 5.1) ----
    case "start_agent_run": {
      const conversationId = argNumber(args, "conversation_id", "conversationId");
      const content = argString(args, "content") ?? "";
      const provider = argString(args, "provider") ?? "openai";
      const model = argString(args, "model") ?? "test-model";
      if (conversationId === undefined) fail("conversation_id required");
      const convId = conversationId as number;
      if (!conversations.some((c) => c.id === convId)) {
        throw { kind: "notFound", message: `conversation ${convId} does not exist` };
      }
      for (const [rid] of activeRuns) {
        const ar = agentRuns.find((x) => x.id === rid);
        if (ar && ar.conversation_id === convId) throw { kind: "invalidInput", message: `an agent run is already active for conversation ${convId}` };
      }
      if (content.trim() === "") throw { kind: "invalidInput", message: "the user message must not be empty" };
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
      const run: MockAgentRun = {
        id: runId,
        conversation_id: convId,
        model,
        mode: "semi_autonomous",
        status: "running",
        started_at: now(),
        finished_at: null,
        total_steps: 0,
        final_content: null,
        error: null,
        spent_micro_usd: null,
        limit_micro_usd: null,
      };
      agentRuns.push(run);
      agentSteps.set(runId, []);
      const active: ActiveMockRun = { status: "running", timers: [], pendingApproval: null, budgetExhausted: false };
      activeRuns.set(runId, active);

      // Helper to append persisted step and emit live frame
      const appendStep = (kind: string, tool_name: string | null, argsStr: string | null, observation: string | null, status: string | null, duration_ms: number | null) => {
        const steps = agentSteps.get(runId)!;
        const seq = steps.length + 1;
        const step: MockAgentStep = {
          id: mockNextStepId++,
          run_id: runId,
          seq,
          kind,
          tool_name,
          arguments: argsStr,
          observation,
          status,
          started_at: now(),
          duration_ms,
        };
        steps.push(step);
        run.total_steps = steps.length;
        emitAgentEvent({ type: "step", run_id: runId, event: { seq, kind, tool_name, arguments: argsStr, observation, status, duration_ms } });
      };

      const finishRun = (status: string, finalContent: string | null, error: string | null) => {
        // Clear timers
        for (const t of active.timers) clearTimeout(t);
        active.timers = [];
        run.status = status;
        run.finished_at = now();
        run.final_content = finalContent;
        run.error = error;
        activeRuns.delete(runId);
        // If completed, persist assistant message
        if (status === "completed" && finalContent) {
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
        emitAgentEvent({ type: "finished", run_id: runId, event: { conversation_id: convId, status, final_content: finalContent, error } });
      };

      // Synthetic stream: 5 steps over ~4s with an approval park ~2s auto-resolved
      // Step 1: model_turn thinking (500ms)
      active.timers.push(window.setTimeout(() => {
        if (!activeRuns.has(runId)) return;
        appendStep("model_turn", null, null, "Thinking about your request…", null, 120);
        emitAgentEvent({ type: "governance", run_id: runId, event: { type: "completed", steps: 1 } });
      }, 400) as unknown as number);

      // Step 2: tool_call read_file succeeded (900ms)
      active.timers.push(window.setTimeout(() => {
        if (!activeRuns.has(runId)) return;
        appendStep("tool_call", "read_file", JSON.stringify({ path: "notes.txt" }), "file body preview…", "succeeded", 35);
      }, 900) as unknown as number);

      // Step 3: approval park for write_file (1400ms) -> emit ApprovalRequested, park until resolved
      active.timers.push(window.setTimeout(() => {
        if (!activeRuns.has(runId)) return;
        const callId = `call-${runId}-approval`;
        active.pendingApproval = { call_id: callId, name: "write_file", arguments: JSON.stringify({ path: "output.txt", content: "hello" }) };
        emitAgentEvent({ type: "governance", run_id: runId, event: { type: "approval_requested", call_id: callId, name: "write_file", arguments: JSON.stringify({ path: "output.txt", content: "hello" }) } });
        // Auto-resolve after ~2s if not manually resolved
        const auto = window.setTimeout(() => {
          if (!activeRuns.has(runId) || !active.pendingApproval) return;
          const approved = true;
          emitAgentEvent({ type: "governance", run_id: runId, event: { type: "approval_resolved", call_id: callId, approved } });
          appendStep("approval", "write_file", JSON.stringify({ path: "output.txt", content: "hello" }), approved ? "approved" : "denied", approved ? "succeeded" : "denied", null);
          active.pendingApproval = null;
          // After approval, continue with tool_call and final model_turn
          window.setTimeout(() => {
            if (!activeRuns.has(runId)) return;
            appendStep("tool_call", "write_file", JSON.stringify({ path: "output.txt", content: "hello" }), "Successfully wrote 5 bytes to 'output.txt'", "succeeded", 12);
          }, 400);
          window.setTimeout(() => {
            if (!activeRuns.has(runId)) return;
            appendStep("model_turn", null, null, `Mock agent reply to “${content}”.`, null, 80);
            finishRun("completed", `Mock agent reply to “${content}”.`, null);
          }, 900);
        }, 2000);
        active.timers.push(auto as unknown as number);
      }, 1400) as unknown as number);

      // For mock simplicity, provider/model args already captured
      void provider;

      return { run_id: runId };
    }
    case "cancel_agent_run": {
      const runId = argNumber(args, "run_id", "runId");
      if (runId === undefined) fail("run_id required");
      const active = activeRuns.get(runId);
      if (!active) throw { kind: "notFound", message: `no active agent run with id ${runId}` };
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
        steps.push({ id: mockNextStepId++, run_id: runId, seq, kind: "approval", tool_name: active.pendingApproval.name, arguments: active.pendingApproval.arguments, observation: "cancelled by the user", status: "cancelled", started_at: now(), duration_ms: null });
        emitAgentEvent({ type: "step", run_id: runId, event: { seq, kind: "approval", tool_name: active.pendingApproval.name, arguments: active.pendingApproval.arguments, observation: "cancelled by the user", status: "cancelled", duration_ms: null } });
        active.pendingApproval = null;
        emitAgentEvent({ type: "governance", run_id: runId, event: { type: "cancelled" } });
      } else {
        emitAgentEvent({ type: "governance", run_id: runId, event: { type: "cancelled" } });
      }
      activeRuns.delete(runId);
      const run2 = agentRuns.find((r) => r.id === runId);
      const convId = run2?.conversation_id ?? 0;
      emitAgentEvent({ type: "finished", run_id: runId, event: { conversation_id: convId, status: "cancelled", final_content: null, error: null } });
      return null;
    }
    case "resolve_agent_approval": {
      const runId = argNumber(args, "run_id", "runId");
      const callId = argString(args, "call_id", "callId") ?? "";
      const approved = argBool(args, "approved") ?? false;
      if (runId === undefined) fail("run_id required");
      const active = activeRuns.get(runId);
      if (!active) throw { kind: "notFound", message: `no active agent run with id ${runId}` };
      if (!active.pendingApproval || active.pendingApproval.call_id !== callId) throw { kind: "notFound", message: "the run has no pending approval for that call" };
      // Clear auto-resolve timer? The pending was inside timers; easiest: mark pending null and emit.
      // We will clear all timers that would auto-resolve and handle now.
      // Find and clear the auto timer? We stored it as last timer; clear all for simplicity and re-schedule remaining steps manually.
      // But simpler: just handle approval logic now and cancel pending auto timer by clearing all and re-emitting remaining.
      for (const t of active.timers) clearTimeout(t);
      active.timers = [];
      const pending = active.pendingApproval;
      active.pendingApproval = null;
      emitAgentEvent({ type: "governance", run_id: runId, event: { type: "approval_resolved", call_id: callId, approved } });
      const steps = agentSteps.get(runId)!;
      const seq = steps.length + 1;
      steps.push({ id: mockNextStepId++, run_id: runId, seq, kind: "approval", tool_name: pending.name, arguments: pending.arguments, observation: approved ? "approved" : "denied", status: approved ? "succeeded" : "denied", started_at: now(), duration_ms: null });
      emitAgentEvent({ type: "step", run_id: runId, event: { seq, kind: "approval", tool_name: pending.name, arguments: pending.arguments, observation: approved ? "approved" : "denied", status: approved ? "succeeded" : "denied", duration_ms: null } });
      const run = agentRuns.find((r) => r.id === runId);
      const convId = run?.conversation_id ?? 0;
      const content = messages.find((m) => m.conversation_id === convId && m.role === "user")?.content ?? "request";
      if (approved) {
        active.timers.push(window.setTimeout(() => {
          if (!activeRuns.has(runId)) return;
          const seq2 = (agentSteps.get(runId)!.length) + 1;
          agentSteps.get(runId)!.push({ id: mockNextStepId++, run_id: runId, seq: seq2, kind: "tool_call", tool_name: "write_file", arguments: pending.arguments, observation: "Successfully wrote 5 bytes to 'output.txt'", status: "succeeded", started_at: now(), duration_ms: 12 });
          emitAgentEvent({ type: "step", run_id: runId, event: { seq: seq2, kind: "tool_call", tool_name: "write_file", arguments: pending.arguments, observation: "Successfully wrote 5 bytes to 'output.txt'", status: "succeeded", duration_ms: 12 } });
        }, 400) as unknown as number);
        active.timers.push(window.setTimeout(() => {
          if (!activeRuns.has(runId)) return;
          const seq3 = (agentSteps.get(runId)!.length) + 1;
          agentSteps.get(runId)!.push({ id: mockNextStepId++, run_id: runId, seq: seq3, kind: "model_turn", tool_name: null, arguments: null, observation: `Mock agent reply to “${content}”.`, status: null, started_at: now(), duration_ms: 80 });
          emitAgentEvent({ type: "step", run_id: runId, event: { seq: seq3, kind: "model_turn", tool_name: null, arguments: null, observation: `Mock agent reply to “${content}”.`, status: null, duration_ms: 80 } });
          if (run) { run.status = "completed"; run.finished_at = now(); run.final_content = `Mock agent reply to “${content}”.`; }
          activeRuns.delete(runId);
          messages.push({ id: nextMessageId++, conversation_id: convId, role: "assistant", content: `Mock agent reply to “${content}”.`, provider_id: 1, model_name: run?.model ?? "test", created_at: now() });
          emitAgentEvent({ type: "finished", run_id: runId, event: { conversation_id: convId, status: "completed", final_content: `Mock agent reply to “${content}”.`, error: null } });
        }, 900) as unknown as number);
      } else {
        // Denied: record observation and finish with denied flow but still completed? For mock, treat as completed with denial note.
        active.timers.push(window.setTimeout(() => {
          if (!activeRuns.has(runId)) return;
          const seq2 = (agentSteps.get(runId)!.length) + 1;
          agentSteps.get(runId)!.push({ id: mockNextStepId++, run_id: runId, seq: seq2, kind: "model_turn", tool_name: null, arguments: null, observation: `Request denied.`, status: null, started_at: now(), duration_ms: 80 });
          emitAgentEvent({ type: "step", run_id: runId, event: { seq: seq2, kind: "model_turn", tool_name: null, arguments: null, observation: `Request denied.`, status: null, duration_ms: 80 } });
          if (run) { run.status = "completed"; run.finished_at = now(); run.final_content = `Request denied.`; }
          activeRuns.delete(runId);
          emitAgentEvent({ type: "finished", run_id: runId, event: { conversation_id: convId, status: "completed", final_content: `Request denied.`, error: null } });
        }, 500) as unknown as number);
      }
      return null;
    }
    case "extend_agent_run": {
      const runId = argNumber(args, "run_id", "runId");
      const extra = argNumber(args, "extra_steps", "extraSteps") ?? 1;
      if (runId === undefined) fail("run_id required");
      if (extra <= 0) throw { kind: "invalidInput", message: "extra steps must be greater than zero" };
      const active = activeRuns.get(runId);
      if (!active) throw { kind: "notFound", message: `no active agent run with id ${runId}` };
      // If budget exhausted, clear flag and emit continuation steps
      if (active.budgetExhausted) {
        active.budgetExhausted = false;
        const run = agentRuns.find((r) => r.id === runId);
        if (run) run.status = "running";
        // Simulate extended steps
        active.timers.push(window.setTimeout(() => {
          if (!activeRuns.has(runId)) return;
          const steps = agentSteps.get(runId)!;
          const seq = steps.length + 1;
          steps.push({ id: mockNextStepId++, run_id: runId, seq, kind: "model_turn", tool_name: null, arguments: null, observation: "Continued after budget.", status: null, started_at: now(), duration_ms: 80 });
          emitAgentEvent({ type: "step", run_id: runId, event: { seq, kind: "model_turn", tool_name: null, arguments: null, observation: "Continued after budget.", status: null, duration_ms: 80 } });
          if (run) { run.status = "completed"; run.finished_at = now(); run.final_content = "Continued after budget."; }
          activeRuns.delete(runId);
          emitAgentEvent({ type: "finished", run_id: runId, event: { conversation_id: run?.conversation_id ?? 0, status: "completed", final_content: "Continued after budget.", error: null } });
        }, 600) as unknown as number);
      } else {
        // Extend budget while running: just log, no immediate effect
      }
      return null;
    }
    case "list_agent_runs": {
      const convId = argNumber(args, "conversation_id", "conversationId");
      if (convId === undefined) return [];
      return [...agentRuns].filter((r) => r.conversation_id === convId).sort((a, b) => b.started_at - a.started_at);
    }
    case "list_agent_steps": {
      const runId = argNumber(args, "run_id", "runId");
      if (runId === undefined) return [];
      const steps = agentSteps.get(runId) ?? [];
      return [...steps].sort((a, b) => a.seq - b.seq);
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
    default:
      fail(`Mock backend: unhandled command “${command}”.`);
  }
}

if (
  !(
    "__TAURI_INTERNALS__" in window &&
    (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__
  )
) {
  Object.defineProperty(window, "__TAURI_INTERNALS__", {
    configurable: true,
    value: {
      invoke,
      transformCallback: (callback: unknown) => callback,
      metadata: { currentWindow: { label: "mock" }, currentWebview: { label: "mock" } },
    },
  });
}
