import { useEffect, useState } from "react";
import { previewPromptContext, updatePromptSkill, viewPromptSkill } from "../lib/tauri";
import type {
  ContextSelector, PromptContextView, PromptSkillView, SkillAction,
} from "../lib/tauri";
import type { ComposerContextState, PromptSheet } from "../lib/useComposerContext";
import { useStrings } from "../lib/useLocale";
import M3Button from "./M3Button";
import ModalShell from "./Modal";

function errorMessage(error: unknown, fallback: string) {
  if (typeof error === "object" && error !== null && "message" in error
      && typeof error.message === "string") return error.message;
  return fallback;
}

interface Props {
  conversationId: number;
  state: ComposerContextState;
  sheet: PromptSheet;
  onUseSkill: (name: string) => void;
  onIncludeContext: () => void;
  onReview: (view: PromptContextView) => Promise<boolean>;
}

export default function ComposerPromptSheet({
  conversationId, state, sheet, onUseSkill, onIncludeContext, onReview,
}: Props) {
  const { locale, t } = useStrings();
  const [path, setPath] = useState("");
  const [symbol, setSymbol] = useState("");
  const [preview, setPreview] = useState<PromptContextView | null>(null);
  const [skill, setSkill] = useState<PromptSkillView | null>(null);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const skillRef = sheet.kind === "skill" ? sheet.skill.reference : null;
  const review = sheet.kind === "review";
  const selectorKind = sheet.kind === "context" ? sheet.selector : "diff";
  const contextMode = sheet.kind === "context" || review;
  const close = () => state.setSheet(null);

  useEffect(() => {
    if (!skillRef && !review) return;
    let cancelled = false;
    setLoading(true);
    const work = skillRef
      ? viewPromptSkill(conversationId, skillRef).then((result) => {
          if (!cancelled) setSkill(result);
        })
      : previewPromptContext(conversationId, { kind: "diff", path: null }).then((result) => {
          if (!cancelled) setPreview(result);
        });
    void work.catch((cause) => {
      if (!cancelled) setError(errorMessage(cause, t("prompt.unavailable")));
    }).finally(() => {
      if (!cancelled) setLoading(false);
    });
    return () => { cancelled = true; };
  }, [conversationId, review, skillRef, t]);

  async function changeSkill(action: SkillAction) {
    if (!skill || busy) return;
    setBusy(true);
    setError(null);
    try {
      const summary = await updatePromptSkill(conversationId, skill.summary.reference, action, locale);
      setSkill({ ...skill, summary });
      state.updateSkill(summary);
    } catch (cause) {
      setError(errorMessage(cause, t("prompt.unavailable")));
    } finally {
      setBusy(false);
    }
  }

  async function previewContext() {
    if (!contextMode || busy) return;
    const value = path.trim();
    let selector: ContextSelector;
    if (selectorKind === "symbol") {
      selector = { kind: "symbol", path: value, symbol: symbol.trim() };
    } else if (selectorKind === "diff") {
      selector = { kind: "diff", path: value || null };
    } else {
      selector = { kind: selectorKind, path: value };
    }
    setBusy(true);
    setError(null);
    setPreview(null);
    try {
      setPreview(await previewPromptContext(conversationId, selector));
    } catch (cause) {
      setError(errorMessage(cause, t("prompt.unavailable")));
    } finally {
      setBusy(false);
    }
  }

  const title = sheet.kind === "skill"
    ? `/${sheet.skill.name}`
    : sheet.kind === "skills" ? t("prompt.skills")
      : review ? t("prompt.reviewTitle") : `@${selectorKind}`;
  const summary = skill?.summary;

  return (
    <ModalShell title={title} busy={busy} onClose={close} sheet>
      <div className="nex-prompt-sheet">
        {loading && <p role="status">{t("common.loading")}</p>}
        {error && <p role="alert">{error}</p>}
        {sheet.kind === "skills" && (
          <>
            {state.skillError && <p role="alert">{t("prompt.unavailable")}</p>}
            {!state.skillError && state.skills.length === 0 && <p>{t("prompt.noSkills")}</p>}
            <ul className="nex-prompt-skills">
              {state.skills.map((item) => (
                <li key={item.reference.source_id}>
                  <M3Button variant="quiet" onClick={() => state.setSheet({ kind: "skill", skill: item })}>
                    <code>/{item.name}</code>
                  </M3Button>
                  <span>{t(item.scope === "personal" ? "prompt.personal" : "prompt.project")}</span>
                </li>
              ))}
            </ul>
          </>
        )}
        {summary && skill && (
          <>
            <p>{summary.description}</p>
            <dl className="nex-prompt-pin">
              <dt>{t(summary.scope === "personal" ? "prompt.personal" : "prompt.project")}</dt>
              <dd><code>{summary.path}</code></dd>
              {summary.commit && <><dt>Git</dt><dd><code>{summary.commit}</code></dd></>}
              <dt>SHA-256</dt>
              <dd><code>{summary.sha256}</code></dd>
            </dl>
            <pre className="nex-prompt-preview" tabIndex={0}>{skill.text}</pre>
            <div className="nex-prompt-actions">
              {!summary.trusted && <M3Button variant="secondary" loading={busy}
                onClick={() => void changeSkill("trust")}>{t("prompt.trust")}</M3Button>}
              <M3Button disabled={busy} onClick={() => void changeSkill(summary.enabled ? "disable" : "enable")}>
                {t(summary.enabled ? "prompt.disable" : "prompt.enable")}
              </M3Button>
              {summary.enabled && summary.trusted && (
                <label className="nex-prompt-toggle">
                  <input type="checkbox" checked={summary.auto_select} disabled={busy}
                    onChange={(event) => void changeSkill(event.currentTarget.checked
                      ? "enable_auto_selection" : "disable_auto_selection")} />
                  {t("prompt.autoSelect")}
                </label>
              )}
              <M3Button variant="primary" disabled={busy || !summary.enabled || !summary.trusted}
                onClick={() => { state.includeSkill(summary); onUseSkill(summary.name); }}>
                {t("prompt.include")}
              </M3Button>
            </div>
          </>
        )}
        {contextMode && !review && (
          <form className="nex-prompt-form" onSubmit={(event) => {
            event.preventDefault();
            void previewContext();
          }}>
            <label>
              {t(selectorKind === "diff" ? "prompt.diffPath" : "prompt.path")}
              <input className="nex-prompt-field" value={path} disabled={busy}
                maxLength={1024}
                onChange={(event) => { setPath(event.currentTarget.value); setPreview(null); }} />
            </label>
            {selectorKind === "symbol" && (
              <label>
                {t("prompt.symbol")}
                <input className="nex-prompt-field" value={symbol} disabled={busy}
                  maxLength={96}
                  onChange={(event) => { setSymbol(event.currentTarget.value); setPreview(null); }} />
              </label>
            )}
            <M3Button type="submit" variant="secondary" loading={busy}
              disabled={(selectorKind !== "diff" && !path.trim()) || (selectorKind === "symbol" && !symbol.trim())}>
              {t("prompt.preview")}
            </M3Button>
          </form>
        )}
        {review && <p>{t("prompt.reviewScope")}</p>}
        {preview && (
          <>
            <p><code>{preview.label}</code></p>
            {preview.omitted && <p role="status">{t("prompt.omitted")}</p>}
            <pre className="nex-prompt-preview" tabIndex={0}>{preview.text}</pre>
            <div className="nex-prompt-actions">
              <M3Button variant="primary" loading={busy} onClick={() => {
                if (!review) {
                  state.includeContext(preview);
                  onIncludeContext();
                  return;
                }
                setBusy(true);
                void onReview(preview).then((sent) => {
                  if (sent) close();
                  else setError(t("prompt.reviewFailed"));
                }).finally(() => setBusy(false));
              }}>{t(review ? "prompt.sendReview" : "prompt.include")}</M3Button>
            </div>
          </>
        )}
        <div className="nex-prompt-actions">
          {sheet.kind === "skill" && (
            <M3Button disabled={busy} onClick={() => state.setSheet({ kind: "skills" })}>
              {t("prompt.skills")}
            </M3Button>
          )}
          <M3Button disabled={busy} onClick={close}>{t("common.close")}</M3Button>
        </div>
      </div>
    </ModalShell>
  );
}
