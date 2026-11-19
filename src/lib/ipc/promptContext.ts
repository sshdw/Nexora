import { invoke } from "./core";

export interface SkillReference {
  source_id: string;
  revision: string;
}

export interface PromptSkill {
  reference: SkillReference;
  name: string;
  description: string;
  scope: "personal" | "project";
  path: string;
  commit: string | null;
  sha256: string;
  trusted: boolean;
  enabled: boolean;
  auto_select: boolean;
}

export interface PromptSkillView {
  summary: PromptSkill;
  text: string;
}

export type SkillAction =
  | "trust"
  | "disable"
  | "enable"
  | "enable_auto_selection"
  | "disable_auto_selection";

export type ContextSelector =
  | { kind: "file" | "folder"; path: string }
  | { kind: "symbol"; path: string; symbol: string }
  | { kind: "diff"; path: string | null };

export interface ContextReference {
  selector: ContextSelector;
  revision: string;
}

export interface PromptContextView {
  reference: ContextReference;
  label: string;
  text: string;
  omitted: boolean;
}

export interface PromptReferences {
  skills?: SkillReference[];
  automatic_skills?: SkillReference[];
  contexts?: ContextReference[];
  review?: boolean;
}

export function listPromptSkills(conversationId: number, draft?: string): Promise<PromptSkill[]> {
  return invoke("list_prompt_skills", { conversationId, draft: draft ?? null });
}

export function viewPromptSkill(conversationId: number, reference: SkillReference): Promise<PromptSkillView> {
  return invoke("view_prompt_skill", { conversationId, reference });
}

export function updatePromptSkill(
  conversationId: number, reference: SkillReference, action: SkillAction, locale: string
): Promise<PromptSkill> {
  return invoke("update_prompt_skill", { conversationId, reference, action, locale });
}

export function previewPromptContext(
  conversationId: number, selector: ContextSelector
): Promise<PromptContextView> {
  return invoke("preview_prompt_context", { conversationId, selector });
}
