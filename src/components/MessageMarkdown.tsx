//! Markdown bodies for ASSISTANT messages (ADD-01, review spec
//! 05-ADD-FOR-2.0.0.md §3.1 + hardening policy 02-SECURITY-FRONTEND-IPC.md
//! §4.2). User messages stay plain escaped text in ConversationView — only
//! model-authored bodies are markdown-interpreted, because model text is the
//! only text that carries formatting intent.
//!
//! Security posture (the whole point of this component, §4.2):
//!   1. Library class — `react-markdown` + `remark-gfm` emit REACT ELEMENTS.
//!      There is no HTML-string pipeline here: no `rehype-raw`, no
//!      `dangerouslySetInnerHTML`, so the app's zero-sink posture and the
//!      strict CSP (`script-src 'self'`) survive verbatim. react-markdown
//!      converts raw-HTML nodes to plain text nodes by default
//!      (node_modules/react-markdown/lib/index.js:360-369), which is exactly
//!      §4.2 req 2: a model must not be able to author markup, and
//!      HTML-looking input renders visibly as text.
//!   2. Links are COPY-ONLY for 2.0.0 (owner default; §4.2 req 3): the `a`
//!      override never renders an `<a>` element, so a model-authored URL can
//!      never reach a navigation sink — it renders as styled text next to the
//!      destination shown verbatim, plus a copy affordance that reuses the
//!      app's clipboard pattern (`navigator.clipboard.writeText` + copied
//!      state + `common.copyFailed` fallback, as in AuditPanel.tsx:299-310).
//!      `urlTransform` passes `href` through UNTOUCHED only because the only
//!      consumer is `CopyOnlyLink`, which receives it as a prop and renders
//!      it as text; it is never placed on a DOM attribute (pinned in tests).
//!   3. Images are app-source-only (§4.2 req 5): `urlTransform` returns null
//!      for every `src` that is not an app-generated `blob:`/`asset:` URL,
//!      and the `img` override re-checks and otherwise renders the alt text
//!      in muted style. Remote `http(s)` and `data:` images never load —
//!      CSP `img-src` does not include them and would block them anyway, but
//!      `data:` IS in `img-src` (`tauri.conf.json`), so this hook is the
//!      only `data:` enforcement point.
//!   4. Fences render as text children of `<pre><code>` (existing
//!      `.nex-message-body pre/code` token styling) and are CAPPED at the
//!      backend's 32,768-char persisted-text bound
//!      (`src-tauri/src/application/agent/persistence.rs:69`, §4.2 req 4),
//!      so a hostile multi-megabyte fence cannot stall the webview.
//!   5. Styling is token classes only, no renderer style attributes
//!      (§4.2 req 6); no animation is added, so `prefers-reduced-motion`
//!      has nothing to honor here.
//!
//! Streaming: assistant content updates per `agent-run-event` frame; the
//! component is memoized on `(messageId, content)` (§4.2 req 7 / §3.1) so
//! unrelated renders never re-parse. An unterminated fence mid-stream parses
//! as a code block (CommonMark), pinned in tests.

import { isValidElement, memo, useState } from "react";
import type {
  AnchorHTMLAttributes,
  HTMLAttributes,
  ImgHTMLAttributes,
  ReactNode,
} from "react";
import ReactMarkdown, { defaultUrlTransform } from "react-markdown";
import type { Components, ExtraProps } from "react-markdown";
import remarkGfm from "remark-gfm";

import { useStrings } from "../lib/useLocale";

/** Render cap for one fenced block — the backend's persisted-text bound
 *  (`application/agent/persistence.rs:69`); the UI never renders materially
 *  more than the backend persists (02-SECURITY §4.2 req 4). */
const FENCE_RENDER_CAP = 32_768;

/** The only image sources that survive: app-generated attachment URLs.
 *  Everything else — `http(s)`, `data:`, `javascript:`, relative paths —
 *  renders as its alt text (02-SECURITY §4.2 req 5). */
function isAppImageSource(value: string): boolean {
  return value.startsWith("blob:") || value.startsWith("asset:");
}

/** Flatten React children to their text content. Used for the fence cap and
 *  for deduplicating autolink labels (a GFM autolink's label IS its URL). */
function textOf(node: ReactNode): string {
  if (node === null || node === undefined || typeof node === "boolean") return "";
  if (typeof node === "string") return node;
  if (typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textOf).join("");
  if (isValidElement(node)) {
    return textOf((node.props as { children?: ReactNode }).children);
  }
  return "";
}

interface CopyOnlyLinkProps
  extends AnchorHTMLAttributes<HTMLAnchorElement>,
    ExtraProps {
  /** Catalog label for the copy affordance. */
  copyLabel: string;
  /** Catalog label shown after a successful copy. */
  copiedLabel: string;
}

/** §4.2 req 3: a model-authored link NEVER becomes an anchor. The
 *  destination renders as visible muted text (the app-wide precedent for
 *  URLs — IssuesPanel.tsx:317-319 renders `html_url` as text) and copies
 *  verbatim on demand. If the label already is the destination (GFM
 *  autolink, bare URL) it is shown once. */
function CopyOnlyLink({ children, href, copyLabel, copiedLabel }: CopyOnlyLinkProps) {
  const { t } = useStrings();
  const [copied, setCopied] = useState(false);
  const [failed, setFailed] = useState(false);
  const target = typeof href === "string" && href.length > 0 ? href : null;
  const label = textOf(children);
  const visibleLabel = label.length > 0 ? label : (target ?? "");

  const copy = async () => {
    if (target === null) return;
    try {
      await navigator.clipboard.writeText(target);
      setCopied(true);
    } catch {
      setFailed(true);
    }
  };

  return (
    <span className="nex-md-link">
      <span className="nex-md-link-text">{visibleLabel}</span>
      {target !== null && label !== target && (
        <span className="nex-md-link-target">{target}</span>
      )}
      {target !== null &&
        (failed ? (
          <span className="nex-md-copy-failed">{t("common.copyFailed")}</span>
        ) : (
          <button type="button" className="nex-md-copy" onClick={() => void copy()}>
            {copied ? copiedLabel : copyLabel}
          </button>
        ))}
    </span>
  );
}

/** §4.2 req 5: images from non-app sources render as their alt text in
 *  muted style. The `urlTransform` hook has already stripped the `src` by
 *  the time this runs — the hook IS the enforcement point the spec names —
 *  so there is no destination to fall back to; an image with no alt text
 *  renders nothing at all. The `src` re-check is the second lock on the
 *  same door for app-generated values. */
function MdImage({
  alt,
  src,
}: ImgHTMLAttributes<HTMLImageElement> & ExtraProps) {
  if (typeof src === "string" && isAppImageSource(src)) {
    return (
      <img
        src={src}
        alt={typeof alt === "string" ? alt : ""}
        className="nex-md-img"
      />
    );
  }
  if (typeof alt !== "string" || alt.trim().length === 0) return null;
  return <span className="nex-md-img-alt">{alt}</span>;
}

interface MdFenceProps extends HTMLAttributes<HTMLPreElement>, ExtraProps {
  /** Catalog note shown when the block exceeds the render cap. */
  truncatedNote: string;
}

/** §4.2 req 4: fenced code stays a text child of `<pre><code>` (the
 *  `nex-agent-terminal-stdout` pattern) and is capped at the backend's
 *  persisted-text bound so a hostile multi-megabyte fence cannot stall the
 *  webview. Overflow renders a visible note instead of the silent tail. */
function MdFence({ children, truncatedNote }: MdFenceProps) {
  const raw = textOf(children);
  const capped =
    raw.length > FENCE_RENDER_CAP ? raw.slice(0, FENCE_RENDER_CAP) : raw;
  return (
    <pre>
      <code>{capped}</code>
      {raw.length > FENCE_RENDER_CAP && (
        <span className="nex-md-code-note">{truncatedNote}</span>
      )}
    </pre>
  );
}

/** URL hook (§4.2 req 5's "imgSrc-style allowlist"): `src` survives only for
 *  app-generated `blob:`/`asset:` values — `data:` included in the rejection
 *  because CSP `img-src` does allow `data:` and this is the only hook that
 *  sees it. `href` passes through raw because CopyOnlyLink renders it as
 *  TEXT only; the moment anything ever wants clickable links again that is a
 *  separate spec (02-SECURITY §4.2 req 3), not a default here. Every other
 *  URL attribute falls back to the library's safe default. */
function transformUrl(value: string, key: string): string | null {
  if (key === "src") return isAppImageSource(value) ? value : null;
  if (key === "href") return value;
  return defaultUrlTransform(value);
}

export interface MessageMarkdownProps {
  /** Persisted message id — memo key so unrelated thread renders never
   *  re-parse one body (§3.1 streaming note). */
  messageId: number;
  /** Model-authored markdown source. */
  content: string;
}

function MessageMarkdown({ content }: MessageMarkdownProps) {
  const { t, locale } = useStrings();
  const components: Components = {
    a: (props) => (
      <CopyOnlyLink
        {...props}
        copyLabel={t("md.copyLink")}
        copiedLabel={t("md.linkCopied")}
      />
    ),
    img: MdImage,
    pre: (props) => (
      <MdFence
        {...props}
        truncatedNote={t("md.codeTruncated", {
          limit: FENCE_RENDER_CAP.toLocaleString(locale),
        })}
      />
    ),
  };
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      urlTransform={transformUrl}
      components={components}
    >
      {content}
    </ReactMarkdown>
  );
}

export default memo(
  MessageMarkdown,
  (prev, next) => prev.messageId === next.messageId && prev.content === next.content,
);
