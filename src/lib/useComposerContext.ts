import { useCallback, useEffect, useRef, useState } from "react";
import {
  listPromptSkills,
  type ContextSelector,
  type PromptContextView,
  type PromptReferences,
  type PromptSkill,
} from "./tauri";

export type PromptSheet =
  | { kind: "skills" }
  | { kind: "skill"; skill: PromptSkill }
  | { kind: "context"; selector: ContextSelector["kind"] }
  | { kind: "review" };

type IncludedContext = Pick<PromptContextView, "reference" | "label">;

export interface ComposerPromptDraft {
  workspaceRoot: string | null;
  explicit: PromptSkill[];
  contexts: IncludedContext[];
  excluded: string[];
}

export interface ComposerPromptDraftStore {
  value?: ComposerPromptDraft;
  update: (edit: (current?: ComposerPromptDraft) => ComposerPromptDraft) => void;
}

const emptyDraft = (workspaceRoot: string | null): ComposerPromptDraft =>
  ({ workspaceRoot, explicit: [], contexts: [], excluded: [] });

export function useComposerContext(
  conversationId: number,
  workspaceRoot: string | null,
  draft: string,
  live: boolean,
  store?: ComposerPromptDraftStore
) {
  const [skills, setSkills] = useState<PromptSkill[]>([]);
  const [skillError, setSkillError] = useState(false);
  const [local, setLocal] = useState<ComposerPromptDraft>(() => emptyDraft(workspaceRoot));
  const saved = store ? store.value : local;
  const { explicit, contexts, excluded } = saved?.workspaceRoot === workspaceRoot
    ? saved
    : emptyDraft(workspaceRoot);
  const updateDraft = (edit: (current: ComposerPromptDraft) => ComposerPromptDraft) => {
    const update = (current?: ComposerPromptDraft) => edit(current?.workspaceRoot === workspaceRoot
      ? current : emptyDraft(workspaceRoot));
    if (store) store.update(update);
    else setLocal(update);
  };
  const refreshRevision = useRef(0);
  const [automatic, setAutomatic] = useState<{ draft: string; skills: PromptSkill[] } | null>(null);
  const [sheet, setSheet] = useState<PromptSheet | null>(null);

  const refresh = useCallback(async () => {
    const revision = ++refreshRevision.current;
    try {
      const items = await listPromptSkills(conversationId);
      if (revision !== refreshRevision.current) return;
      setSkills(Array.isArray(items) ? items : []);
      setSkillError(false);
    } catch {
      if (revision !== refreshRevision.current) return;
      setSkills([]);
      setSkillError(true);
    }
  }, [conversationId]);

  useEffect(() => {
    setSheet(null);
    setSkills([]);
    setAutomatic(null);
    void refresh();
    return () => { refreshRevision.current += 1; };
  }, [refresh, workspaceRoot]);

  const automaticEnabled = skills.some((item) => item.auto_select);
  const needsMatch = !live && automaticEnabled && draft.trim() !== "" && !draft.startsWith("/");
  useEffect(() => {
    if (!needsMatch) return;
    let cancelled = false;
    const timer = window.setTimeout(() => {
      void listPromptSkills(conversationId, draft)
        .then((items) => {
          if (!cancelled) setAutomatic({ draft, skills: Array.isArray(items) ? items : [] });
        })
        .catch(() => {
          // Auto selection is voluntary. A failed lookup sends no extra skill.
          if (!cancelled) setAutomatic({ draft, skills: [] });
        });
    }, 140);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [conversationId, draft, needsMatch, workspaceRoot, skills]);

  const suggested = needsMatch && automatic?.draft === draft
    ? automatic.skills.filter((item) =>
        !excluded.includes(item.reference.source_id) &&
        !explicit.some((selected) => selected.reference.source_id === item.reference.source_id) &&
        skills.some((current) => current.auto_select && current.reference.revision === item.reference.revision)
      )
    : [];
  const selected = [...explicit, ...suggested];
  const active = selected.length + contexts.length > 0;
  const pending = needsMatch && automatic?.draft !== draft;
  const references: PromptReferences | undefined = active
    ? { skills: selected.map((item) => item.reference), contexts: contexts.map((item) => item.reference) }
    : undefined;

  function updateSkill(item: PromptSkill) {
    setSkills((current) => current.map((skill) =>
      skill.reference.source_id === item.reference.source_id ? item : skill
    ));
    if (!item.enabled || !item.trusted) removeSkill(item);
  }

  function includeSkill(item: PromptSkill) {
    updateDraft((current) => ({
      ...current,
      explicit: [
        ...current.explicit.filter((skill) => skill.reference.source_id !== item.reference.source_id),
        item,
      ],
    }));
    setSheet(null);
  }

  function removeSkill(item: PromptSkill) {
    const id = item.reference.source_id;
    updateDraft((current) => ({
      ...current,
      explicit: current.explicit.filter((skill) => skill.reference.source_id !== id),
      excluded: [...new Set([...current.excluded, id])],
    }));
  }

  function includeContext(item: PromptContextView) {
    const selector = JSON.stringify(item.reference.selector);
    updateDraft((current) => ({
      ...current,
      contexts: [
        ...current.contexts.filter((view) => JSON.stringify(view.reference.selector) !== selector),
        { reference: item.reference, label: item.label },
      ],
    }));
    setSheet(null);
  }

  function removeContext(item: IncludedContext) {
    updateDraft((current) => ({
      ...current,
      contexts: current.contexts.filter((view) => view.reference.revision !== item.reference.revision),
    }));
  }

  function clear() {
    updateDraft(() => emptyDraft(workspaceRoot));
  }
  return {
    skills, skillError, refresh, selected, suggested, contexts, active, pending, references,
    sheet, setSheet, updateSkill, includeSkill, removeSkill, includeContext, clear,
    removeContext,
  };
}

export type ComposerContextState = ReturnType<typeof useComposerContext>;
