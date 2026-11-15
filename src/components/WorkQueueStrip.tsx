//! Persistent FIFO work queue strip rendered above the composer (Stage 6 PR 6A).
//!
//! Displays queued follow-ups in FIFO order with reorder (move up/down), inline
//! edit, remove/cancel, and manual `Run next` recovery when the queue paused on
//! a failed/cancelled/interrupted run.

import { useState } from "react";

import type { WorkQueueItem } from "../lib/tauri";
import { useStrings } from "../lib/useLocale";
import {
  ArrowDownIcon,
  ArrowUpIcon,
  CheckIcon,
  CloseIcon,
  PencilIcon,
} from "./icons";
import { Button } from "./ui/button";

export interface WorkQueueStripProps {
  items: WorkQueueItem[];
  paused: boolean;
  hasActiveRun: boolean;
  onMove: (itemId: number, direction: "up" | "down") => Promise<void> | void;
  onEdit: (itemId: number, content: string) => Promise<void> | void;
  onCancel: (itemId: number) => Promise<void> | void;
  onRunNext: () => Promise<void> | void;
}

export default function WorkQueueStrip({
  items,
  paused,
  hasActiveRun,
  onMove,
  onEdit,
  onCancel,
  onRunNext,
}: WorkQueueStripProps) {
  const { t } = useStrings();
  const [editingId, setEditingId] = useState<number | null>(null);
  const [editDraft, setEditDraft] = useState("");
  const [busyId, setBusyId] = useState<number | null>(null);
  const [runningNext, setRunningNext] = useState(false);

  if (items.length === 0) return null;

  const startEdit = (item: WorkQueueItem) => {
    setEditingId(item.id);
    setEditDraft(item.content);
  };

  const commitEdit = async (itemId: number) => {
    const trimmed = editDraft.trim();
    if (!trimmed) return;
    setBusyId(itemId);
    try {
      await onEdit(itemId, trimmed);
      setEditingId(null);
      setEditDraft("");
    } finally {
      setBusyId(null);
    }
  };

  const handleRunNext = async () => {
    if (runningNext || hasActiveRun) return;
    setRunningNext(true);
    try {
      await onRunNext();
    } finally {
      setRunningNext(false);
    }
  };

  return (
    <section
      className="nex-queue-strip"
      aria-label={t("queue.title")}
      data-paused={paused ? "true" : "false"}
    >
      <div className="nex-queue-header">
        <div className="nex-queue-title-group">
          <span className="nex-queue-title">{t("queue.title")}</span>
          <span className="nex-queue-count">
            {t("queue.count", { count: items.length })}
          </span>
        </div>
        {paused && !hasActiveRun && (
          <Button
            type="button"
            size="sm"
            variant="outline"
            disabled={runningNext}
            onClick={() => void handleRunNext()}
          >
            {t("queue.runNext")}
          </Button>
        )}
      </div>

      {paused && (
        <p className="nex-queue-paused-banner" role="status">
          {t("queue.pausedBanner")}
        </p>
      )}

      <ol className="nex-queue-list">
        {items.map((item, idx) => {
          const isEditing = editingId === item.id;
          const isBusy = busyId === item.id;
          return (
            <li key={item.id} className="nex-queue-item">
              <span className="nex-queue-index">{idx + 1}.</span>
              {isEditing ? (
                <div className="nex-queue-edit-row">
                  <input
                    type="text"
                    className="nex-queue-edit-input"
                    value={editDraft}
                    disabled={isBusy}
                    aria-label={t("queue.edit")}
                    onChange={(e) => setEditDraft(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") {
                        e.preventDefault();
                        void commitEdit(item.id);
                      } else if (e.key === "Escape") {
                        e.preventDefault();
                        setEditingId(null);
                      }
                    }}
                  />
                  <Button
                    type="button"
                    size="xs"
                    variant="outline"
                    disabled={isBusy || editDraft.trim() === ""}
                    aria-label={t("queue.save")}
                    title={t("queue.save")}
                    onClick={() => void commitEdit(item.id)}
                  >
                    <CheckIcon size={16} />
                  </Button>
                  <Button
                    type="button"
                    size="xs"
                    variant="ghost"
                    disabled={isBusy}
                    aria-label={t("queue.cancelEdit")}
                    title={t("queue.cancelEdit")}
                    onClick={() => setEditingId(null)}
                  >
                    <CloseIcon size={16} />
                  </Button>
                </div>
              ) : (
                <>
                  <span className="nex-queue-content" title={item.content}>
                    {item.content}
                  </span>
                  <div className="nex-queue-item-actions">
                    <Button
                      type="button"
                      size="icon-xs"
                      variant="ghost"
                      disabled={idx === 0 || isBusy}
                      aria-label={t("queue.moveUp")}
                      title={t("queue.moveUp")}
                      onClick={() => void onMove(item.id, "up")}
                    >
                      <ArrowUpIcon size={16} />
                    </Button>
                    <Button
                      type="button"
                      size="icon-xs"
                      variant="ghost"
                      disabled={idx === items.length - 1 || isBusy}
                      aria-label={t("queue.moveDown")}
                      title={t("queue.moveDown")}
                      onClick={() => void onMove(item.id, "down")}
                    >
                      <ArrowDownIcon size={16} />
                    </Button>
                    <Button
                      type="button"
                      size="icon-xs"
                      variant="ghost"
                      disabled={isBusy}
                      aria-label={t("queue.edit")}
                      title={t("queue.edit")}
                      onClick={() => startEdit(item)}
                    >
                      <PencilIcon size={16} />
                    </Button>
                    <Button
                      type="button"
                      size="icon-xs"
                      variant="ghost"
                      disabled={isBusy}
                      aria-label={t("queue.remove")}
                      title={t("queue.remove")}
                      onClick={() => void onCancel(item.id)}
                    >
                      <CloseIcon size={16} />
                    </Button>
                  </div>
                </>
              )}
            </li>
          );
        })}
      </ol>
    </section>
  );
}
