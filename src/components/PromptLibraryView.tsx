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

import ConfirmDialog from "./ConfirmDialog";
import ModalShell from "./Modal";
import NexoraMark from "./NexoraMark";
import { PencilIcon, SearchIcon, TrashIcon } from "./icons";
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
  // 0.3.0: deletion confirms in the Nexora dialog system (was
  // window.confirm) — same explicit-confirm behavior, in-app chrome.
  const [pendingDelete, setPendingDelete] = useState<Prompt | null>(null);

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
    setPendingDelete(prompt);
  };

  const confirmDelete = () => {
    if (pendingDelete) void store.remove(pendingDelete.id);
    setPendingDelete(null);
  };

  const handleUse = (prompt: Prompt) => {
    if (hasActiveConversation) onUse(prompt.content);
  };

  const filtered = filterPrompts(store.prompts, query);
  const saving = store.working;

  return (
    <div className="nex-prompt-library nex-view-enter">
      <header className="nex-prompt-library-header">
        <div className="nex-prompt-library-heading">
          <h2 className="nex-prompt-library-title">Prompt Library</h2>
          <p className="nex-prompt-library-subtitle">
            Reusable message templates for any conversation.
          </p>
        </div>
        <button type="button" className="nex-btn nex-btn-ghost" onClick={onClose}>
          Back to conversations
        </button>
      </header>

      <div className="nex-prompt-toolbar">
        <div className="nex-search nex-prompt-search">
          <label htmlFor="nex-prompt-search-input" className="nex-sr-only">
            Search prompts
          </label>
          <SearchIcon className="nex-search-icon" />
          <input
            id="nex-prompt-search-input"
            type="search"
            className="nex-search-input"
            placeholder="Search prompts"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            autoComplete="off"
            spellCheck={false}
          />
        </div>
        <button
          type="button"
          className="nex-btn nex-btn-primary nex-btn-expressive"
          onClick={openCreate}
          disabled={saving}
        >
          New Prompt
        </button>
      </div>

      <div className="nex-prompt-library-body">
        {store.loading ? (
          // Skeleton rows: the shared loading primitive from components.css
          // (same treatment as the sidebar's loading list), announced politely.
          <ul
            className="nex-prompt-list nex-skeleton-list"
            role="status"
            aria-label="Loading prompts"
          >
            {Array.from({ length: 4 }).map((_, index) => (
              <li key={index} className="nex-skeleton-row" />
            ))}
          </ul>
        ) : store.error && editor === null ? (
          <div className="nex-prompt-error nex-fade-in" role="alert">
            <span className="nex-prompt-error-text">{store.error.message}</span>
            <button
              type="button"
              className="nex-btn nex-btn-ghost nex-btn-sm"
              onClick={() => void store.reload()}
            >
              Try again
            </button>
          </div>
        ) : store.prompts.length === 0 && filtered.length === 0 ? (
          <div className="nex-prompt-empty nex-empty-enter">
            <span className="nex-empty-mark-wrap" aria-hidden="true">
              <NexoraMark className="nex-empty-mark" width={26} height={26} />
            </span>
            <h3 className="nex-prompt-empty-title">No prompts yet</h3>
            <p className="nex-prompt-empty-text">
              Create reusable message templates and insert them into any
              conversation with “Use”.
            </p>
            <div className="nex-empty-actions">
              <button
                type="button"
                className="nex-btn nex-btn-primary nex-btn-expressive"
                onClick={openCreate}
                disabled={saving}
              >
                New Prompt
              </button>
            </div>
          </div>
        ) : filtered.length === 0 ? (
          <p className="nex-prompt-status nex-fade-in">
            No prompts match “{query.trim()}”.
          </p>
        ) : (
          <ul className="nex-prompt-grid nex-stagger">
            {filtered.map((prompt) => (
              <li key={prompt.id} className="nex-prompt-card">
                <div className="nex-prompt-card-head">
                  <span className="nex-prompt-title" title={prompt.title}>
                    {prompt.title}
                  </span>
                  <time
                    className="nex-prompt-time"
                    dateTime={new Date(prompt.updated_at * 1000).toISOString()}
                  >
                    {formatRelativeTime(prompt.updated_at)}
                  </time>
                </div>
                <span className="nex-prompt-preview">{prompt.content}</span>
                <div className="nex-prompt-card-foot">
                  <button
                    type="button"
                    className="nex-btn nex-btn-primary nex-btn-sm"
                    onClick={() => handleUse(prompt)}
                    disabled={!hasActiveConversation || saving}
                    aria-label={`Insert ${prompt.title} into the active conversation`}
                    title={
                      hasActiveConversation
                        ? "Insert into the active conversation"
                        : "Open a conversation first"
                    }
                  >
                    Use
                  </button>
                  <span className="nex-prompt-card-foot-spacer" />
                  <div className="nex-prompt-card-tools">
                    <button
                      type="button"
                      className="nex-icon-btn nex-icon-btn-sm"
                      onClick={() => openEdit(prompt)}
                      disabled={saving}
                      aria-label={`Edit ${prompt.title}`}
                      title="Edit"
                    >
                      <PencilIcon />
                    </button>
                    <button
                      type="button"
                      className="nex-icon-btn nex-icon-btn-sm nex-icon-btn-danger"
                      onClick={() => handleDelete(prompt)}
                      disabled={saving}
                      aria-label={`Delete ${prompt.title}`}
                      title="Delete"
                    >
                      <TrashIcon />
                    </button>
                  </div>
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>

      {pendingDelete && (
        <ConfirmDialog
          title="Delete prompt?"
          body={`“${pendingDelete.title}” will be permanently deleted. This cannot be undone.`}
          confirmLabel="Delete"
          danger
          onConfirm={confirmDelete}
          onCancel={() => setPendingDelete(null)}
        />
      )}

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
    <ModalShell
      title={editor.editing ? "Edit prompt" : "New prompt"}
      onClose={onCancel}
    >
      <div className="nex-io-body">
        {error && (
          <p id="nex-prompt-editor-error" className="nex-dialog-error nex-fade-in" role="alert">
            {error.message}
          </p>
        )}

        <div className="nex-prompt-field">
          <label className="nex-prompt-label" htmlFor="nex-prompt-title-input">
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
            aria-describedby={error ? "nex-prompt-editor-error" : undefined}
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
          <label className="nex-prompt-label" htmlFor="nex-prompt-content-input">
            Content
          </label>
          <textarea
            id="nex-prompt-content-input"
            className="nex-textarea"
            rows={6}
            value={editor.content}
            maxLength={CONTENT_MAX}
            disabled={saving}
            aria-describedby={error ? "nex-prompt-editor-error" : undefined}
            onChange={(event) => onContentChange(event.target.value)}
          />
          <p className="nex-prompt-count">
            {editor.content.length}/{CONTENT_MAX}
          </p>
        </div>
      </div>

      <div className="nex-dialog-actions">
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
          className="nex-btn nex-btn-primary"
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
    </ModalShell>
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


