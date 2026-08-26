//! DEV-ONLY visual-QA mock backend — never active in production.
//!
//! Installs an in-memory stand-in for the Tauri IPC surface so the real UI
//! can be rendered in a plain browser (Puppeteer visual validation) with
//! representative data. Activated only when ALL of the following hold:
//!   - Vite dev build (import.meta.env.DEV — dead-code-eliminated otherwise)
//!   - the page URL carries ?mock
//! No product component imports this module; main.tsx conditionally
//! dynamic-imports it before mounting React. Data lives in memory only and
//! every command mirrors the contract in tauri.ts (snake_case payloads).

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
    models: ["gpt-5.2", "gpt-5.2-mini", "o5-pro"],
  },
  {
    name: "anthropic",
    display_name: "Anthropic",
    models: ["claude-opus-4.6", "claude-sonnet-4.5", "claude-haiku-4.5"],
  },
  {
    name: "gemini",
    display_name: "Gemini",
    models: ["gemini-3.6-flash", "gemini-3.6-pro", "gemini-3.1-ultra"],
  },
];

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
      const id = Number(args.conversationId);
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
        display_name: String(args.displayName),
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
      return providers.some((p) => p.name === args.name) && credentialed.has(String(args.name));
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
        conversation_id: Number(args.conversationId),
        message_id: null,
        file_name: String(args.fileName),
        file_path: String(args.filePath),
        file_size_bytes: typeof args.fileSizeBytes === "number" ? args.fileSizeBytes : null,
        mime_type: typeof args.mimeType === "string" ? args.mimeType : null,
      });
      return attachments[attachments.length - 1];
    }
    case "list_attachments":
      return attachments.filter(
        (a) => a.conversation_id === Number(args.conversationId) && a.message_id === null,
      );
    case "remove_attachment": {
      const index = attachments.findIndex((a) => a.id === Number(args.id));
      if (index >= 0) attachments.splice(index, 1);
      return null;
    }
    case "send_message": {
      // Simulated round trip: persist the user message, wait, persist the
      // assistant reply — enough to observe sending/typing states.
      const conversationId = Number(args.conversationId);
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
      for (const id of (args.attachmentIds as number[]) ?? []) {
        const draft = attachments.find((a) => a.id === id);
        if (draft) draft.message_id = userId;
      }
      return { content: "ok", model: String(args.model) };
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
