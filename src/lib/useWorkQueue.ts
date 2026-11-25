//! Persistent per-conversation FIFO work queue hook (Stage 6 PR 6A).
//!
//! Subscribes to `work-queue-updated` events for `conversationId`, loads initial
//! queued items from SQLite, and exposes reorder/edit/cancel/run-next controls.

import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import {
  type WorkQueueItem,
  cancelWorkQueueItem,
  enqueueWorkItem,
  formatIpcError,
  listWorkQueue,
  reorderWorkQueue,
  runNextWorkItem,
  updateWorkQueueItem,
} from "./tauri";

export interface WorkQueueStore {
  items: WorkQueueItem[];
  loading: boolean;
  error: string | null;
  reload: () => Promise<void>;
  enqueue: (
    content: string,
    provider: string,
    model: string,
    effort?: string | null
  ) => Promise<WorkQueueItem>;
  updateItem: (itemId: number, content: string) => Promise<WorkQueueItem>;
  moveItem: (itemId: number, direction: "up" | "down") => Promise<void>;
  cancelItem: (itemId: number) => Promise<void>;
  runNext: () => Promise<number | null>;
}

interface WorkQueueUpdatedPayload {
  conversation_id: number;
}

export function useWorkQueue(conversationId: number): WorkQueueStore {
  const [items, setItems] = useState<WorkQueueItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const requestRef = useRef(0);
  const convRef = useRef(conversationId);
  convRef.current = conversationId;

  const reload = useCallback(async () => {
    const req = ++requestRef.current;
    const targetConv = conversationId;
    setLoading(true);
    try {
      const next = await listWorkQueue(targetConv);
      if (convRef.current !== targetConv || requestRef.current !== req) return;
      setItems(next);
      setError(null);
    } catch (err) {
      if (convRef.current !== targetConv || requestRef.current !== req) return;
      setError(formatIpcError(err));
    } finally {
      if (convRef.current === targetConv && requestRef.current === req) {
        setLoading(false);
      }
    }
  }, [conversationId]);

  useEffect(() => {
    setItems([]);
    setError(null);
    void reload();

    let cancelled = false;
    let unlisten: (() => void) | undefined;
    listen<WorkQueueUpdatedPayload>("work-queue-updated", (event) => {
      if (cancelled) return;
      if (event.payload?.conversation_id === conversationId) {
        void reload();
      }
    })
      .then((dispose) => {
        if (cancelled) {
          dispose();
          return;
        }
        unlisten = dispose;
      })
      .catch(() => {
        /* Test environments without event mock still refresh via mutations. */
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [conversationId, reload]);

  const enqueue = useCallback(
    async (
      content: string,
      provider: string,
      model: string,
      effort?: string | null
    ): Promise<WorkQueueItem> => {
      const created = await enqueueWorkItem(
        conversationId,
        content,
        provider,
        model,
        effort
      );
      await reload();
      return created;
    },
    [conversationId, reload]
  );

  const updateItem = useCallback(
    async (itemId: number, content: string): Promise<WorkQueueItem> => {
      const updated = await updateWorkQueueItem(itemId, content);
      setItems((prev) =>
        prev.map((item) => (item.id === itemId ? updated : item))
      );
      return updated;
    },
    []
  );

  const moveItem = useCallback(
    async (itemId: number, direction: "up" | "down"): Promise<void> => {
      const idx = items.findIndex((item) => item.id === itemId);
      if (idx < 0) return;
      const swapWith = direction === "up" ? idx - 1 : idx + 1;
      if (swapWith < 0 || swapWith >= items.length) return;
      const reordered = [...items];
      const [moved] = reordered.splice(idx, 1);
      reordered.splice(swapWith, 0, moved);
      setItems(reordered);
      const serverOrdered = await reorderWorkQueue(
        conversationId,
        reordered.map((item) => item.id)
      );
      setItems(serverOrdered);
    },
    [conversationId, items]
  );

  const cancelItem = useCallback(
    async (itemId: number): Promise<void> => {
      await cancelWorkQueueItem(itemId);
      setItems((prev) => prev.filter((item) => item.id !== itemId));
    },
    []
  );

  const runNext = useCallback(async (): Promise<number | null> => {
    const res = await runNextWorkItem(conversationId);
    await reload();
    return res?.run_id ?? null;
  }, [conversationId, reload]);

  return {
    items,
    loading,
    error,
    reload,
    enqueue,
    updateItem,
    moveItem,
    cancelItem,
    runNext,
  };
}
