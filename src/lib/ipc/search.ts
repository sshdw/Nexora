//! Search IPC area (AC-205): the local grouped search over conversations,
//! messages, and prompts (FR-006, FR-009).

import { invoke } from "./core";
import type { Conversation, Message } from "./conversations";
import type { Prompt } from "./prompts";

/** Grouped results of one `search` call (BACKEND application/search.rs). */
export interface SearchResults {
  /** Conversations whose title matched, ordered by relevance. */
  conversations: Conversation[];
  /** Messages whose content matched; each opens its `conversation_id`. */
  message_matches: Message[];
  /** Prompts whose title/content matched. */
  prompts: Prompt[];
}

/** Run the existing local `search` command over conversations, messages, and
 * prompts. A blank query yields empty results (backend contract). */
export function search(query: string): Promise<SearchResults> {
  return invoke<SearchResults>("search", { query });
}
