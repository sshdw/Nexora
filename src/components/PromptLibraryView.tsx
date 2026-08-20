//! Prompt Library screen (Phase 10.4 — FR-007, FR-009 prompts subset).
//!
//! A dedicated view (not a modal screen and not a Settings tab) reached from the
//! sidebar. It lists every saved prompt (already sorted `updated_at` DESC by the
//! store), searches locally by title/content (frontend filter for MVP — FTS5
//! backend search is future scope), and offers create / edit / delete through a
//! modal dialog plus "Use" to place a prompt's content into the active
//! conversation's composer. Backend commands are used as-is; the presentation
//! only filters.

import { useEffect, useState } from "react";

import { SearchIcon } from "./icons";
import type { Prompt } from "../lib/tauri";
import { formatRelativeTime } from "../lib/format";
import { usePrompts } from "../lib/usePrompts";

/** Backend `prompts` schema limits, mirrored in the editor (DATABASE.md §7.3). */
const TITLE_MAX = 200;
const CONTENT_MAX = 10_000;

export interface PromptLibraryViewProps {
  onClose: () => void;
  /** Whether a conversation is currently open, so a prompt can be staged. */
  hasActiveConversation: boolean;
  /** Stage a prompt's content into the active conversation's composer. */
  onUse: (content: string) => void;
  /**
   * When set (a prompt search result was chosen), open that prompt's existing
   * Edit modal once its row is available (FR-009). Consumers reset it between
   * library sessions so it never re-opens a stale editor.
   */
  initialEditId?: number | null;
}

interface EditorState {
  /** The prompt being edited, or null when creating a new prompt. */
  editing: Prompt | null;
  title: string;
  content: string;
}

export default function PromptLibraryView({
  onClose,
  hasActiveConversation,
  onUse,
  initialEditId = null,
}: PromptLibraryViewProps) {
  const store = usePrompts();
  const [query, setQuery] = useState("");
  const [editor, setEditor] = useState<EditorState | null>(null);
  // Remember the prompt already opened through `initialEditId` so the effect
  // below runs once per requested id and cannot re-open the editor on unrelated
  // re-renders (list reloads, typing, Cancel, etc.).
  const [openedInitial, setOpenedInitial] = useState<number | null>(null);

  useEffect(() => {
    if (initialEditId == null) return;
    if (openedInitial === initialEditId) return;
    if (store.loading) return;
    const target = store.prompts.find((prompt) => prompt.id === initialEditId);
    if (!target) return;
    setEditor({ editing: target, title: target.title, content: target.content });
    setOpenedInitial(initialEditId);
  }, [initialEditId, openedInitial, store.loading, store.prompts]);

  const openCreate = () => {
    setEditor({ editing: null, title: "", content: "" });
  };
  const openEdit = (prompt: Prompt) => {
    setEditor({ editing: prompt, title: prompt.title, content: prompt.content });
  };
  const closeEditor = () => setEditor(null);

  const saveEditor = async () => {
    if (!editor) return;
    const title = editor.title.trim();
    const content = editor.content.trim();
    if (title === "" || content === "") return;
    if (editor.editing === null) {
      const saved = await store.create(title, content);
      if (saved !== null) closeEditor();
    } else if (await store.update(editor.editing.id, title, content)) {
      closeEditor();
    }
  };

  const handleDelete = (prompt: Prompt) => {
    const confirmed = window.confirm(
      `Delete "${prompt.title}"? This cannot be undone.`,
    );
    if (confirmed) void store.remove(prompt.id);
  };

  const handleUse = (prompt: Prompt) => {
    if (hasActiveConversation) onUse(prompt.content);
  };

    const filtered = filterPrompts(store.prompts, query);
  const saving = store.working;

  return (
    <div className="nex-prompt-library">
      <header className="nex-prompt-library-header">
        <h2 className="nex-prompt-library-title">Prompt Library</h2>
        <button type="button" className="nex-btn nex-btn-ghost" onClick={onClose}>
          Back to conversations
        </button>
      </header>

      <div className="nex-prompt-toolbar">
        <div className="nex-prompt-search">
          <label htmlFor="nex-prompt-search-input" className="nex-sr-only">
            Search prompts
          </label>
          <SearchIcon className="nex-prompt-search-icon" />
          <input
            id="nex-prompt-search-input"
            type="search"
            className="nex-prompt-search-input"
            placeholder="Search prompts"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            autoComplete="off"
            spellCheck={false}
          />
        </div>
        <button
          type="button"
          className="nex-btn nex-btn-accent"
          onClick={openCreate}
          disabled={saving}
        >
                    New Prompt
        </button>
      </div>

      <div className="nex-prompt-library-body">
        {store.loading ? (
          <p className="nex-prompt-status">Loading prompts…</p>
        ) : store.error && editor === null ? (
          <p className="nex-prompt-status" role="alert">
            {store.error.message}
          </p>
        ) : store.prompts.length === 0 && filtered.length === 0 ? (
          <div className="nex-prompt-empty">
            <h3 className="nex-prompt-empty-title">No prompts yet</h3>
            <p className="nex-prompt-empty-text">
              Create reusable message templates and insert them into any
              conversation with “Use”.
            </p>
          </div>
        ) : filtered.length === 0 ? (
          <p className="nex-prompt-status">No prompts match “{query.trim()}”.</p>
        ) : (
          <ul className="nex-prompt-list">
            {filtered.map((prompt) => (
              <li key={prompt.id} className="nex-prompt-item">
                <div className="nex-prompt-item-main">
                  <span className="nex-prompt-title" title={prompt.title}>
                    {prompt.title}
                  </span>
                  <span className="nex-prompt-preview">{prompt.content}</span>
                </div>
                <div className="nex-prompt-meta">
                  <span className="nex-prompt-label-muted">
                    {formatRelativeTime(prompt.updated_at)}
                  </span>
                </div>
                <div className="nex-prompt-actions">
                  <button
                    type="button"
                    className="nex-btn nex-btn-accent nex-btn-sm"
                    onClick={() => handleUse(prompt)}
                    disabled={!hasActiveConversation || saving}
                    title={
                      hasActiveConversation
                        ? "Insert into the active conversation"
                        : "Open a conversation first"
                    }
                  >
                    Use
                  </button>
                  <button
                    type="button"
                    className="nex-btn nex-btn-ghost nex-btn-sm"
                    onClick={() => openEdit(prompt)}
                    disabled={saving}
                  >
                    Edit
                  </button>
                  <button
                    type="button"
                    className="nex-btn nex-btn-danger nex-btn-sm"
                    onClick={() => handleDelete(prompt)}
                    disabled={saving}
                  >
                    Delete
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>

            {editor && (
        <PromptEditor
          editor={editor}
          saving={saving}
          error={store.error}
          onTitleChange={(title) =>
            setEditor((prev) => (prev ? { ...prev, title } : prev))
          }
          onContentChange={(content) =>
            setEditor((prev) => (prev ? { ...prev, content } : prev))
          }
          onSave={() => void saveEditor()}
          onCancel={closeEditor}
        />
      )}
    </div>
  );
}

interface PromptEditorProps {
  editor: EditorState;
  saving: boolean;
  error: { kind: string; message: string } | null;
  onTitleChange: (title: string) => void;
  onContentChange: (content: string) => void;
  onSave: () => void;
  onCancel: () => void;
}

function PromptEditor({
  editor,
  saving,
  error,
  onTitleChange,
  onContentChange,
  onSave,
  onCancel,
}: PromptEditorProps) {
  return (
    <div
      className="nex-prompt-modal-backdrop"
      role="presentation"
      onClick={onCancel}
    >
      <div
        className="nex-prompt-modal-card"
        role="dialog"
        aria-modal="true"
        aria-label={editor.editing ? "Edit prompt" : "New prompt"}
        onClick={(event) => event.stopPropagation()}
      >
        <h3 className="nex-prompt-modal-title">
          {editor.editing ? "Edit prompt" : "New prompt"}
        </h3>

        {error && (
          <p className="nex-prompt-modal-error" role="alert">
            {error.message}
          </p>
        )}

        <div className="nex-prompt-field">
          <label
            className="nex-prompt-label"
            htmlFor="nex-prompt-title-input"
          >
            Title
          </label>
          <input
            id="nex-prompt-title-input"
            className="nex-input"
            value={editor.title}
            maxLength={TITLE_MAX}
            placeholder="Prompt name"
            autoFocus
            disabled={saving}
            onChange={(event) => onTitleChange(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                onSave();
              }
            }}
          />
          <p className="nex-prompt-count">
            {editor.title.length}/{TITLE_MAX}
          </p>
        </div>

        <div className="nex-prompt-field">
          <label
            className="nex-prompt-label"
            htmlFor="nex-prompt-content-input"
          >
            Content
          </label>
          <textarea
            id="nex-prompt-content-input"
            className="nex-textarea"
            rows={6}
            value={editor.content}
            maxLength={CONTENT_MAX}
            disabled={saving}
            onChange={(event) => onContentChange(event.target.value)}
          />
          <p className="nex-prompt-count">
            {editor.content.length}/{CONTENT_MAX}
          </p>
        </div>

        <div className="nex-prompt-modal-actions">
          <button
            type="button"
            className="nex-btn nex-btn-ghost"
            onClick={onCancel}
            disabled={saving}
          >
            Cancel
          </button>
          <button
            type="button"
            className="nex-btn nex-btn-accent"
            onClick={onSave}
            disabled={
              editor.title.trim() === "" ||
              editor.content.trim() === "" ||
              saving
            }
          >
            {editor.editing ? "Save changes" : "Create prompt"}
          </button>
        </div>
      </div>
    </div>
  );
}

function filterPrompts(prompts: Prompt[], query: string): Prompt[] {
  const q = query.trim().toLowerCase();
  if (q === "") return prompts;
  return prompts.filter(
    (prompt) =>
      prompt.title.toLowerCase().includes(q) ||
      prompt.content.toLowerCase().includes(q),
  );
}


