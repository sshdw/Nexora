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

import M3Button from "./M3Button";
import M3IconButton from "./M3IconButton";
import M3Toolbar from "./M3Toolbar";
import ModalShell from "./Modal";
import NexoraMark from "./NexoraMark";
import { PencilIcon, SearchIcon, TrashIcon } from "./icons";
import type { Prompt } from "../lib/tauri";
import { formatRelativeTime } from "../lib/format";
import { useStrings } from "../lib/useLocale";
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
  const { locale, t } = useStrings();

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

  // NEX-SEC-004: there is NO in-app confirm dialog here. `deletePrompt`
  // raises a blocking NATIVE OS confirmation prompt showing the exact row,
  // and mints a single-use id bound to that row only if the user accepts.
  //
  // A cancel at that prompt is reported honestly and distinctly: the backend
  // rejects with `confirmationRequired`, which is rendered below as a calm
  // status line (not the error panel), the library stays on screen, and
  // nothing was deleted. Derived from `store.error` rather than from a local
  // flag set in the click handler, so it cannot go stale against the store's
  // own async state; the next operation clears it with the error.
  const deleteCancelled = store.error?.kind === "confirmationRequired";

  const handleDelete = (prompt: Prompt) => {
    void store.remove(prompt.id);
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
          <h2 className="nex-prompt-library-title">{t("prompts.title")}</h2>
          <p className="nex-prompt-library-subtitle">
            {t("prompts.subtitle")}
          </p>
        </div>
        <M3Button variant="quiet" onClick={onClose}>
          {t("common.backToConversations")}
        </M3Button>
      </header>

      <M3Toolbar label={t("prompts.toolbar")} className="nex-prompt-toolbar">
        <div className="nex-search nex-prompt-search">
          <label htmlFor="nex-prompt-search-input" className="nex-sr-only">
            {t("prompts.searchLabel")}
          </label>
          <SearchIcon className="nex-search-icon" />
          <input
            id="nex-prompt-search-input"
            type="search"
            className="nex-search-input"
            placeholder={t("prompts.searchPh")}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            autoComplete="off"
            spellCheck={false}
          />
        </div>
        <M3Button
          variant="primary"
          expressive
          onClick={openCreate}
          disabled={saving}
        >
          {t("prompts.newPrompt")}
        </M3Button>
      </M3Toolbar>

      <div className="nex-prompt-library-body">
        {deleteCancelled && (
          // Honest report of a cancel at the native system prompt: nothing was
          // deleted, the list is untouched, and the prompt clears on the next
          // operation. Deliberately a status line, not the error panel — the
          // user chose to cancel, so the library stays on screen.
          <p className="nex-prompt-status nex-fade-in" role="status">
            {t("prompts.deleteCancelled")}
          </p>
        )}
        {store.loading ? (
          // Skeleton rows: the shared loading primitive from components.css
          // (same treatment as the sidebar's loading list), announced politely.
          <div
            className="nex-prompt-list nex-skeleton-list"
            role="status"
            aria-label={t("prompts.loading")}
          >
            {Array.from({ length: 4 }).map((_, index) => (
              <div key={index} className="nex-skeleton-row" />
            ))}
          </div>
        ) : store.error && !deleteCancelled && editor === null ? (
          <div className="nex-prompt-error nex-fade-in" role="alert">
            <span className="nex-prompt-error-text">{store.error.message}</span>
            <M3Button
              variant="quiet"
              size="sm"
              onClick={() => void store.reload()}
            >
              {t("common.retry")}
            </M3Button>
          </div>
        ) : store.prompts.length === 0 && filtered.length === 0 ? (
          <div className="nex-prompt-empty nex-empty-enter">
            <span className="nex-empty-mark-wrap" aria-hidden="true">
              <NexoraMark className="nex-empty-mark" width={26} height={26} />
            </span>
            <h3 className="nex-prompt-empty-title">{t("prompts.emptyTitle")}</h3>
            <p className="nex-prompt-empty-text">
              {t("prompts.emptyText")}
            </p>
            <div className="nex-empty-actions">
              <M3Button
                variant="primary"
                expressive
                onClick={openCreate}
                disabled={saving}
              >
                {t("prompts.newPrompt")}
              </M3Button>
            </div>
          </div>
        ) : filtered.length === 0 ? (
          <p className="nex-prompt-status nex-fade-in">
            {t("prompts.noMatch", { q: query.trim() })}
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
                    {formatRelativeTime(prompt.updated_at, locale)}
                  </time>
                </div>
                <span className="nex-prompt-preview">{prompt.content}</span>
                <div className="nex-prompt-card-foot">
                  <M3Button
                    variant="primary"
                    size="sm"
                    onClick={() => handleUse(prompt)}
                    disabled={!hasActiveConversation || saving}
                    aria-label={t("prompts.useAria", { title: prompt.title })}
                    title={
                      hasActiveConversation
                        ? t("prompts.useTitleOk")
                        : t("prompts.useTitleNeed")
                    }
                  >
                    {t("prompts.use")}
                  </M3Button>
                  <span className="nex-prompt-card-foot-spacer" />
                  <M3Toolbar
                    label={t("prompts.toolsAria", { title: prompt.title })}
                    className="nex-prompt-card-tools"
                  >
                    <M3IconButton
                      size="sm"
                      label={t("prompts.editAria", { title: prompt.title })}
                      onClick={() => openEdit(prompt)}
                      disabled={saving}
                    >
                      <PencilIcon />
                    </M3IconButton>
                    <M3IconButton
                      size="sm"
                      danger
                      label={t("prompts.deleteAria", { title: prompt.title })}
                      onClick={() => handleDelete(prompt)}
                      disabled={saving}
                    >
                      <TrashIcon />
                    </M3IconButton>
                  </M3Toolbar>
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
  const { t } = useStrings();
  return (
    <ModalShell
      title={editor.editing ? t("prompts.editorEdit") : t("prompts.editorNew")}
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
            {t("prompts.titleLabel")}
          </label>
          <input
            id="nex-prompt-title-input"
            className="nex-input"
            value={editor.title}
            maxLength={TITLE_MAX}
            placeholder={t("prompts.titlePh")}
            autoFocus
            disabled={saving}
            aria-describedby={error ? "nex-prompt-editor-error" : undefined}
            onChange={(event) => onTitleChange(event.target.value)}
            onKeyDown={(event) => {
              // shortcut:prompt.save.
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
            {t("prompts.contentLabel")}
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
        <M3Button variant="quiet" onClick={onCancel} disabled={saving}>
          {t("common.cancel")}
        </M3Button>
        <M3Button
          variant="primary"
          loading={saving}
          disabled={
            editor.title.trim() === "" ||
            editor.content.trim() === ""
          }
          onClick={onSave}
        >
          {editor.editing ? t("prompts.saveChanges") : t("prompts.createPrompt")}
        </M3Button>
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


