//! Markdown bodies for assistant messages (ADD-01) — the security pins.
//!
//! The fixture set follows 02-SECURITY-FRONTEND-IPC.md §4.2 req 8: classic
//! payloads must render inert. Everything here renders against the REAL
//! react-markdown pipeline (no mocks): what is pinned is what the library
//! actually does with hostile input, not what a mock pretends it does.
//!
//! jsdom computes no cascade: what is asserted is element presence, text
//! equality and attribute absence — which is what decides whether a
//! model-authored payload can reach a sink (script, anchor href, img src).

import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { LocaleProvider } from "../lib/useLocale";
import MessageMarkdown from "./MessageMarkdown";

function md(content: string) {
  return render(
    <LocaleProvider>
      <MessageMarkdown messageId={1} content={content} />
    </LocaleProvider>,
  );
}

/** jsdom ships no clipboard; the app's copy pattern writes through
 *  `navigator.clipboard.writeText` (AuditPanel.tsx:299-310), so the stub
 *  replaces exactly that seam. */
function stubClipboard(): ReturnType<typeof vi.fn> {
  const writeText = vi.fn<(text: string) => Promise<void>>(() => Promise.resolve());
  Object.defineProperty(window.navigator, "clipboard", {
    value: { writeText },
    configurable: true,
  });
  return writeText;
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  // Drop the stubbed clipboard so a leaking defineProperty cannot grant a
  // clipboard to a file that expects jsdom's absent one.
  delete (window.navigator as { clipboard?: unknown }).clipboard;
});

describe("markdown structure (GFM)", () => {
  it("renders lists, ordered lists and task lists", () => {
    const { container } = md(
      [
        "- alpha",
        "- beta",
        "",
        "1. first",
        "2. second",
        "",
        "- [x] done",
        "- [ ] todo",
      ].join("\n"),
    );

    const lists = screen.getAllByRole("list");
    expect(lists.length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText("alpha")).toBeInTheDocument();
    expect(screen.getByText("first")).toBeInTheDocument();

    // Task-list checkboxes render as real disabled inputs — inert, not
    // interactive form fields (mdast-util-to-hast list-item.js:46).
    const checked = screen.getByRole("checkbox", { checked: true });
    const unchecked = screen.getByRole("checkbox", { checked: false });
    expect(checked).toBeDisabled();
    expect(unchecked).toBeDisabled();
    expect(container.querySelector("li.task-list-item")).not.toBeNull();
  });

  it("renders a GFM table with header and body cells", () => {
    md("| key | value |\n| --- | --- |\n| mode | chat |");

    const table = screen.getByRole("table");
    const rows = within(table).getAllByRole("row");
    expect(rows).toHaveLength(2);
    expect(within(rows[1]!).getByRole("cell", { name: "mode" })).toBeInTheDocument();
    expect(within(rows[1]!).getByRole("cell", { name: "chat" })).toBeInTheDocument();
  });

  it("renders strikethrough, blockquote and a thematic break", () => {
    const { container } = md("~~gone~~\n\n> quoted line\n\n---");
    expect(container.querySelector("del")?.textContent).toBe("gone");
    expect(container.querySelector("blockquote")?.textContent).toContain("quoted line");
    expect(container.querySelector("hr")).not.toBeNull();
  });

  it("renders a fenced code block as text inside pre>code with the language dropped", () => {
    const { container } = md("```ts\nconst x = 1;\n```");
    const pre = container.querySelector("pre");
    expect(pre).not.toBeNull();
    const code = pre?.querySelector("code");
    expect(code?.textContent).toBe("const x = 1;\n");
    // No highlighting dependency in 2.0.0 (05-ADD §3.1): no language class
    // is emitted, and the code fence is a plain text child — never markup.
    expect(code?.querySelector("*")).toBeNull();
  });

  it("renders an UNTERMINATED fence (mid-stream reply) as a code block, not prose", () => {
    // §3.1 streaming pin: a half-streamed fence must never flash as prose.
    const { container } = md("```ts\nconst x = 1;");
    expect(container.querySelector("pre code")?.textContent).toContain("const x = 1;");
    expect(container.querySelector("p")).toBeNull();
  });

  it("caps a fence at the backend's 32,768-char persisted-text bound, with a visible note", () => {
    // §4.2 req 4: the UI must not render materially more than the backend
    // persists (application/agent/persistence.rs:69); overflow is honest.
    const oversized = `${"a".repeat(32_769)}\nconst tail = "hidden";`;
    const { container } = md(`\`\`\`\n${oversized}\n\`\`\``);

    const code = container.querySelector("pre code");
    expect(code?.textContent?.length).toBe(32_768);
    expect(code?.textContent).not.toContain("hidden");
    expect(container.querySelector(".nex-md-code-note")?.textContent).toContain("32,768");
  });

  it("re-renders when the message changes and memoizes on (messageId, content)", () => {
    // Direct pin on the memo comparator (§3.1): same id+content skips,
    // either changing re-renders. The export is a memo object, so its
    // comparator is callable as data.
    const compare = (MessageMarkdown as unknown as { compare?: (a: unknown, b: unknown) => boolean })
      .compare;
    expect(typeof compare).toBe("function");
    const a = { messageId: 7, content: "x" };
    expect(compare?.(a, { messageId: 7, content: "x" })).toBe(true);
    expect(compare?.(a, { messageId: 7, content: "y" })).toBe(false);
    expect(compare?.(a, { messageId: 8, content: "x" })).toBe(false);
  });
});

describe("raw HTML in model output is text (§4.2 req 2)", () => {
  it("renders a <script> payload as inert visible text — no script element, no execution", () => {
    const { container } = md("<script>alert(1)</script>");

    expect(container.querySelector("script")).toBeNull();
    // Visible as text, not silently dropped: what the model sent is what
    // the user sees (escaped by React, unparseable by the DOM).
    expect(container.textContent).toContain("<script>alert(1)</script>");
  });

  it("renders <img src=x onerror=alert(1)> as text — no img, no handler attribute", () => {
    const { container } = md("<img src=x onerror=alert(1)>");

    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("[onerror]")).toBeNull();
    expect(container.textContent).toContain("<img src=x onerror=alert(1)>");
  });

  it("passes no markup through for iframe/embed/object payloads", () => {
    const { container } = md(
      "<iframe src=\"https://evil.example\"></iframe><object data=\"x\"></object><embed src=\"x\">",
    );
    expect(container.querySelector("iframe, object, embed")).toBeNull();
    expect(container.querySelector("[src]")).toBeNull();
    expect(container.querySelector("[data]")).toBeNull();
  });
});

describe("links are copy-only (§4.2 req 3, owner default for 2.0.0)", () => {
  it("renders [x](javascript:alert(1)) as text + copy affordance, never an anchor with that href", async () => {
    const writeText = stubClipboard();
    const { container } = md("[x](javascript:alert(1))");

    // The sink assertions: NO anchor element and NO href attribute anywhere
    // in the rendered tree — the raw destination exists only as text.
    expect(container.querySelector("a")).toBeNull();
    expect(container.querySelector("[href]")).toBeNull();
    expect(container.textContent).toContain("javascript:alert(1)");

    // The copy affordance copies the destination verbatim — the user sees
    // and controls exactly what goes to the clipboard.
    fireEvent.click(screen.getByRole("button", { name: "Copy link" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("javascript:alert(1)"));
    expect(await screen.findByRole("button", { name: "Copied" })).toBeInTheDocument();
  });

  it("renders [x](data:text/html,...) copy-only with the scheme visible", () => {
    const { container } = md("[x](data:text/html;base64,PHNjcmlwdD4)");
    expect(container.querySelector("a")).toBeNull();
    expect(container.querySelector("[href]")).toBeNull();
    expect(container.textContent).toContain("data:text/html;base64,PHNjcmlwdD4");
  });

  it("renders an https link copy-only: label emphasized, destination visible, copy works", async () => {
    const writeText = stubClipboard();
    const { container } = md("read the [docs](https://example.com/a?b=1) first");

    expect(container.querySelector("a")).toBeNull();
    expect(container.querySelector("[href]")).toBeNull();
    expect(container.textContent).toContain("docs");
    expect(container.textContent).toContain("https://example.com/a?b=1");

    fireEvent.click(screen.getByRole("button", { name: "Copy link" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("https://example.com/a?b=1"));
  });

  it("shows a GFM autolink once (label IS the destination) and still copy-only", () => {
    const { container } = md("see https://example.com today");
    expect(container.querySelector("a")).toBeNull();
    // The URL appears exactly once — no duplicated label/destination pair.
    expect(container.textContent?.split("https://example.com").length - 1).toBe(1);
    expect(screen.getByRole("button", { name: "Copy link" })).toBeInTheDocument();
  });

  it("falls back to the catalog copy-failure string when the clipboard rejects", async () => {
    stubClipboard().mockRejectedValue(new Error("denied"));
    md("[x](https://example.com)");
    fireEvent.click(screen.getByRole("button", { name: "Copy link" }));
    expect(await screen.findByText("Copy failed — select the text manually.")).toBeInTheDocument();
  });
});

describe("images are app-source-only (§4.2 req 5)", () => {
  it("strips a remote image and renders its alt text instead", () => {
    const { container } = md("![screenshot](https://cdn.example.com/pic.png)");
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("[src]")).toBeNull();
    expect(container.textContent).toContain("screenshot");
  });

  it("rejects a data: image — the scheme CSP alone will not block", () => {
    // img-src DOES include data: (tauri.conf.json), so the renderer hook is
    // the only enforcement point for data: payloads (§3.1 note, §4.2 req 5).
    const { container } = md("![logo](data:image/svg+xml;base64,AAAA)");
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("[src]")).toBeNull();
    expect(container.textContent).toContain("logo");
  });

  it("renders an app-generated blob: source as a real image", () => {
    const { container } = md("![shot](blob:nexora/abc-123)");
    const img = container.querySelector("img");
    expect(img).not.toBeNull();
    expect(img).toHaveAttribute("src", "blob:nexora/abc-123");
    expect(img).toHaveAttribute("alt", "shot");
  });

  it("an image with no alt renders nothing once stripped (the hook removed the src first)", () => {
    // The urlTransform hook is the enforcement point (§4.2 req 5): by the
    // time the img override runs there is no destination left to show, so
    // an unlabelled model image drops out entirely.
    const { container } = md("![](https://cdn.example.com/pic.png)");
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("[src]")).toBeNull();
    expect(container.textContent).not.toContain("https://cdn.example.com/pic.png");
  });
});

describe("locale (EN+RU catalog)", () => {
  it("renders the copy affordance in the active locale", () => {
    window.localStorage.setItem("nexora.locale", "ru");
    md("[x](https://example.com)");
    expect(screen.getByRole("button", { name: "Скопировать ссылку" })).toBeInTheDocument();
  });
});
