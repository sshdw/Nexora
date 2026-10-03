//! Task manager panel: user-defined task lists with agent-executable steps,
//! plus the autonomous loop (plan → act → verify → report) controls.
//!
//! Presentational over the `create_task` / `list_tasks` / `list_task_steps` /
//! `update_task` / `delete_task` / `start_task_run` / `stop_task_run` IPC
//! wrappers through the `useTasks` store: the store reloads from the
//! persisted rows on every `agent-task-event` frame (ids only — results and
//! reports always come from the backend, never from the event payload).
//!
//! Display rules (secret-free): rows render ids, fixed-vocabulary statuses,
//! counters, and the loop's own recorded step results + composed report
//! (the task's own data, like `final_content` in the conversation view) —
//! never credentials, SQL, or provider internals. Steps beyond the cap read
//! `skipped`; the terminal report names the stop reason. The panel never
//! animates on entry (instant render under reduced motion); all visuals ride
//! the shared panel/tag primitives — zero new CSS, zero raw values.

import { useCallback, useState } from "react";

import { useTasks, type TaskView } from "../lib/useTasks";
import type { AgentTask, AgentTaskStep } from "../lib/tauri";
import { useStrings, type Strings } from "../lib/useLocale";
import M3Button from "./M3Button";
import M3LoadingIndicator from "./M3LoadingIndicator";

export interface TaskPanelProps {
  onClose: () => void;
  /** Current provider/model selection (prefills new tasks). */
  defaultProvider: string | null;
  defaultModel: string | null;
}

/** Fixed-vocabulary task-status label; unknown values echo defensively. */
function taskStatusLabel(status: AgentTask["status"], t: Strings["t"]): string {
  switch (status) {
    case "pending":
      return t("task.statusPending");
    case "running":
      return t("task.statusRunning");
    case "completed":
      return t("task.statusCompleted");
    case "failed":
      return t("task.statusFailed");
    case "cancelled":
      return t("task.statusCancelled");
    default:
      return status;
  }
}

/** Fixed-vocabulary step-status label; unknown values echo defensively. */
function stepStatusLabel(status: AgentTaskStep["status"], t: Strings["t"]): string {
  switch (status) {
    case "pending":
      return t("task.statusPending");
    case "running":
      return t("task.statusRunning");
    case "completed":
      return t("task.statusCompleted");
    case "failed":
      return t("task.statusFailed");
    case "skipped":
      return t("task.statusSkipped");
    case "cancelled":
      return t("task.statusCancelled");
    default:
      return status;
  }
}

interface TaskFormState {
  title: string;
  description: string;
  stepsText: string;
  provider: string;
  model: string;
  cap: string;
}

function blankForm(defaultProvider: string | null, defaultModel: string | null): TaskFormState {
  return {
    title: "",
    description: "",
    stepsText: "",
    provider: defaultProvider ?? "",
    model: defaultModel ?? "",
    cap: "",
  };
}

function parseSteps(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "");
}

export default function TaskPanel({ onClose, defaultProvider, defaultModel }: TaskPanelProps) {
  const { t } = useStrings();
  const store = useTasks();
  const [formOpen, setFormOpen] = useState(false);
  const [editingId, setEditingId] = useState<number | null>(null);
  const [form, setForm] = useState<TaskFormState>(() => blankForm(defaultProvider, defaultModel));
  const [expandedSteps, setExpandedSteps] = useState<Record<number, boolean>>({});
  const [expandedReport, setExpandedReport] = useState<Record<number, boolean>>({});
  const [busyId, setBusyId] = useState<number | null>(null);

  const openCreate = useCallback(() => {
    setEditingId(null);
    setForm(blankForm(defaultProvider, defaultModel));
    setFormOpen(true);
  }, [defaultModel, defaultProvider]);

  const openEdit = useCallback((view: TaskView) => {
    setEditingId(view.task.id);
    setForm({
      title: view.task.title,
      description: view.task.description ?? "",
      stepsText: "",
      provider: view.task.provider ?? "",
      model: view.task.model ?? "",
      cap: String(view.task.max_steps),
    });
    setFormOpen(true);
  }, []);

  const closeForm = useCallback(() => {
    setFormOpen(false);
    setEditingId(null);
  }, []);

  const submitForm = useCallback(async () => {
    const title = form.title.trim();
    if (title === "") return;
    if (editingId !== null) {
      const description = form.description.trim() === "" ? null : form.description.trim();
      setBusyId(editingId);
      try {
        await store.update(editingId, title, description);
      } finally {
        setBusyId(null);
      }
      closeForm();
      return;
    }
    const steps = parseSteps(form.stepsText);
    if (steps.length === 0) return;
    const capRaw = form.cap.trim();
    const maxSteps = capRaw === "" ? steps.length : Number(capRaw);
    if (!Number.isInteger(maxSteps) || maxSteps < 1 || maxSteps > 25) return;
    const description = form.description.trim() === "" ? null : form.description.trim();
    const provider = form.provider.trim() === "" ? null : form.provider.trim();
    const model = form.model.trim() === "" ? null : form.model.trim();
    setBusyId(-1);
    try {
      const id = await store.create(title, description, provider, model, steps, maxSteps);
      if (id !== null) {
        setExpandedSteps((prev) => ({ ...prev, [id]: true }));
      }
    } finally {
      setBusyId(null);
    }
    closeForm();
  }, [closeForm, editingId, form, store]);

  const handleRun = useCallback(
    async (view: TaskView) => {
      if (view.task.provider === null || view.task.model === null) return;
      setBusyId(view.task.id);
      try {
        await store.start(view.task.id);
      } finally {
        setBusyId(null);
      }
    },
    [store],
  );

  const handleStop = useCallback(
    async (taskId: number) => {
      setBusyId(taskId);
      try {
        await store.stop(taskId);
      } finally {
        setBusyId(null);
      }
    },
    [store],
  );

  const handleDelete = useCallback(
    async (view: TaskView) => {
      if (!window.confirm(t("task.deleteBody", { title: view.task.title }))) return;
      setBusyId(view.task.id);
      try {
        await store.remove(view.task.id);
      } finally {
        setBusyId(null);
      }
    },
    [store, t],
  );

  const toggleSteps = useCallback((taskId: number) => {
    setExpandedSteps((prev) => ({ ...prev, [taskId]: !prev[taskId] }));
  }, []);

  const toggleReport = useCallback((taskId: number) => {
    setExpandedReport((prev) => ({ ...prev, [taskId]: !prev[taskId] }));
  }, []);

  const doneCount = (view: TaskView) => view.steps.filter((s) => s.status === "completed").length;

  return (
    <div className="nex-term" role="group" aria-label={t("task.group")}>
      <header className="nex-vcs-header">
        <div className="nex-vcs-heading">
          <h2 className="nex-vcs-title">{t("task.title")}</h2>
          <p className="nex-vcs-subtitle">{t("task.subtitle")}</p>
        </div>
        <div className="nex-vcs-header-actions">
          <M3Button variant="quiet" onClick={openCreate}>
            {t("task.new")}
          </M3Button>
          <M3Button variant="quiet" onClick={onClose}>
            {t("common.backToConversations")}
          </M3Button>
        </div>
      </header>

      <div className="nex-term-body">
        {store.error && (
          <div className="nex-composer-error nex-fade-in" role="alert">
            {store.error}
          </div>
        )}

        {formOpen && (
          <section className="nex-agent-terminal" aria-label={editingId !== null ? t("task.editTitle") : t("task.new")}>
            <div className="nex-agent-terminal-header">
              <span className="nex-agent-terminal-command">
                {editingId !== null ? t("task.editTitle") : t("task.new")}
              </span>
            </div>
            <div className="nex-agent-terminal-body">
              <label className="nex-vcs-notice" htmlFor="nex-task-title">
                {t("task.titleLabel")}
              </label>
              <input
                id="nex-task-title"
                className="nex-input"
                type="text"
                value={form.title}
                placeholder={t("task.titlePh")}
                onChange={(event) => setForm((prev) => ({ ...prev, title: event.target.value }))}
              />
              <label className="nex-vcs-notice" htmlFor="nex-task-desc">
                {t("task.descLabel")}
              </label>
              <input
                id="nex-task-desc"
                className="nex-input"
                type="text"
                value={form.description}
                placeholder={t("task.descPh")}
                onChange={(event) => setForm((prev) => ({ ...prev, description: event.target.value }))}
              />
              {editingId === null && (
                <>
                  <label className="nex-vcs-notice" htmlFor="nex-task-steps">
                    {t("task.stepsLabel")}
                  </label>
                  <textarea
                    id="nex-task-steps"
                    className="nex-input"
                    rows={4}
                    value={form.stepsText}
                    placeholder={t("task.stepsPh")}
                    onChange={(event) => setForm((prev) => ({ ...prev, stepsText: event.target.value }))}
                  />
                  <label className="nex-vcs-notice" htmlFor="nex-task-provider">
                    {t("task.providerLabel")}
                  </label>
                  <input
                    id="nex-task-provider"
                    className="nex-input"
                    type="text"
                    value={form.provider}
                    placeholder={t("task.providerPh")}
                    onChange={(event) => setForm((prev) => ({ ...prev, provider: event.target.value }))}
                  />
                  <label className="nex-vcs-notice" htmlFor="nex-task-model">
                    {t("task.modelLabel")}
                  </label>
                  <input
                    id="nex-task-model"
                    className="nex-input"
                    type="text"
                    value={form.model}
                    placeholder={t("task.modelPh")}
                    onChange={(event) => setForm((prev) => ({ ...prev, model: event.target.value }))}
                  />
                  <label className="nex-vcs-notice" htmlFor="nex-task-cap">
                    {t("task.capLabel")}
                  </label>
                  <input
                    id="nex-task-cap"
                    className="nex-input"
                    type="number"
                    min={1}
                    max={25}
                    value={form.cap}
                    onChange={(event) => setForm((prev) => ({ ...prev, cap: event.target.value }))}
                  />
                </>
              )}
              <div className="nex-vcs-header-actions">
                <M3Button
                  variant="primary"
                  size="sm"
                  disabled={busyId !== null || form.title.trim() === ""}
                  onClick={() => void submitForm()}
                >
                  {editingId !== null ? t("task.save") : t("task.create")}
                </M3Button>
                <M3Button variant="quiet" size="sm" onClick={closeForm}>
                  {t("common.cancel")}
                </M3Button>
              </div>
            </div>
          </section>
        )}

        {store.loading && store.tasks.length === 0 ? (
          <M3LoadingIndicator label={t("common.loading")} />
        ) : store.tasks.length === 0 ? (
          <div className="nex-agent-empty">
            <p>{t("task.noTasksTitle")}</p>
            <p>{t("task.noTasksText")}</p>
          </div>
        ) : (
          store.tasks.map((view) => {
            const running = view.task.status === "running";
            const busy = busyId === view.task.id;
            const canRun =
              !running && view.task.provider !== null && view.task.model !== null && view.steps.length > 0;
            return (
              <article
                key={view.task.id}
                className="nex-agent-terminal"
                aria-label={`${view.task.title} — ${taskStatusLabel(view.task.status, t)}`}
              >
                <div className="nex-agent-terminal-header">
                  <span className="nex-agent-terminal-command">{view.task.title}</span>
                  <span className="nex-tag nex-tag-mono">{taskStatusLabel(view.task.status, t)}</span>
                  {view.steps.length > 0 && (
                    <span className="nex-tag nex-tag-mono">
                      {t("task.progress", { done: doneCount(view), total: view.steps.length })}
                    </span>
                  )}
                </div>
                <div className="nex-agent-terminal-body">
                  <div className="nex-vcs-header-actions">
                    {running ? (
                      <M3Button variant="destructive" size="sm" disabled={busy} onClick={() => void handleStop(view.task.id)}>
                        {t("task.stop")}
                      </M3Button>
                    ) : (
                      <M3Button
                        variant="primary"
                        size="sm"
                        disabled={busy || !canRun}
                        title={canRun ? undefined : t("task.needProvider")}
                        onClick={() => void handleRun(view)}
                      >
                        {t("task.run")}
                      </M3Button>
                    )}
                    <M3Button variant="quiet" size="sm" disabled={busy || running} onClick={() => openEdit(view)}>
                      {t("task.edit")}
                    </M3Button>
                    <M3Button variant="quiet" size="sm" disabled={busy || running} onClick={() => void handleDelete(view)}>
                      {t("task.delete")}
                    </M3Button>
                    {view.steps.length > 0 && (
                      <M3Button variant="quiet" size="sm" onClick={() => toggleSteps(view.task.id)}>
                        {expandedSteps[view.task.id] ? t("task.hideSteps") : t("task.showSteps")}
                      </M3Button>
                    )}
                    {view.task.report !== null && (
                      <M3Button variant="quiet" size="sm" onClick={() => toggleReport(view.task.id)}>
                        {expandedReport[view.task.id] ? t("task.hideReport") : t("task.showReport")}
                      </M3Button>
                    )}
                  </div>
                  {expandedSteps[view.task.id] === true && (
                    <ol className="nex-task-steps">
                      {view.steps.map((step) => (
                        <li key={step.id} className="nex-task-step">
                          <span className="nex-tag nex-tag-mono">{step.seq}</span>
                          <span>{step.title}</span>
                          <span className="nex-tag nex-tag-mono">{stepStatusLabel(step.status, t)}</span>
                          {step.result !== null && step.result !== "" && (
                            <pre className="nex-agent-terminal-stdout">{step.result}</pre>
                          )}
                        </li>
                      ))}
                    </ol>
                  )}
                  {expandedReport[view.task.id] === true && view.task.report !== null && (
                    <div>
                      <p className="nex-vcs-notice">{t("task.reportTitle")}</p>
                      <pre className="nex-agent-terminal-stdout">{view.task.report}</pre>
                    </div>
                  )}
                </div>
              </article>
            );
          })
        )}
      </div>
    </div>
  );
}
