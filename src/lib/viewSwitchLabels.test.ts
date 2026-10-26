//! The two switches that both said "Chat" (Defect B, round-4 visual QA).
//!
//! QA, on a real Tauri frame: the pane's header chip said "Chat"
//! (selected) and the composer's mode chip right below it said "Agent"
//! (selected) — two different switches in the same chip grammar, one
//! saying the same word as the other's counterpart. The stage's VIEW
//! switch and the composer's MODE switch were both "Chat".
//!
//! The fix renames the view switch only:
//!
//!   view  (ConversationView.tsx → conv.chatTab / conv.contextTab)
//!         EN Conversation | Context      RU Диалог | Контекст
//!   mode  (Composer.tsx → conv.modeChat / conv.agentToggle)
//!         EN Chat | Agent                RU Чат | Агент   ← unchanged
//!
//! "Byte-identical" is meant literally below: the composer's pair is
//! asserted against hard-coded words, so a future sweep that renames
//! both switches again fails here instead of shipping a second
//! contradiction.
//!
//! `conv.viewLabel` is the group's own accessible name. It moved off
//! "Conversation view" for the same reason in the a11y tree that the
//! visual defect had: announced with a "Conversation" tab inside it, the
//! group name reads like the option ("Conversation view, tab list,
//! Conversation, selected"). It now names what the group switches: the
//! pane's content.

import { describe, expect, it } from "vitest";

import { readRepoFile } from "../test/readRepoFile";

import { tr } from "./strings";

const VIEW_SOURCE = readRepoFile("src/components/ConversationView.tsx");
const COMPOSER_SOURCE = readRepoFile("src/components/Composer.tsx");

describe("view switch — the stage's Conversation | Context (Defect B)", () => {
  it("names both views, in both locales", () => {
    expect(tr("en", "conv.chatTab")).toBe("Conversation");
    expect(tr("en", "conv.contextTab")).toBe("Context");
    expect(tr("ru", "conv.chatTab")).toBe("Диалог");
    expect(tr("ru", "conv.contextTab")).toBe("Контекст");
  });

  it("keeps the group's own name distinct from both options", () => {
    for (const locale of ["en", "ru"] as const) {
      const label = tr(locale, "conv.viewLabel");
      const options = [tr(locale, "conv.chatTab"), tr(locale, "conv.contextTab")];
      // A group named after one of its own options is the a11y twin of
      // the visual contradiction QA reported.
      expect(options).not.toContain(label);
      expect(label.length).toBeGreaterThan(0);
    }
    expect(tr("en", "conv.viewLabel")).toBe("Pane content");
    expect(tr("ru", "conv.viewLabel")).toBe("Содержимое панели");
  });

  it("leaves the composer's mode pair byte-identical", () => {
    // Hard-coded, not read back from the catalog: a rename that touched
    // the composer too would keep this file green if it compared one
    // key to another.
    expect(tr("en", "conv.modeChat")).toBe("Chat");
    expect(tr("en", "conv.agentToggle")).toBe("Agent");
    expect(tr("ru", "conv.modeChat")).toBe("Чат");
    expect(tr("ru", "conv.agentToggle")).toBe("Агент");
  });

  it("never uses one word for both switches, in either locale", () => {
    for (const locale of ["en", "ru"] as const) {
      const view = [tr(locale, "conv.chatTab"), tr(locale, "conv.contextTab")];
      const mode = [tr(locale, "conv.modeChat"), tr(locale, "conv.agentToggle")];
      for (const word of view) expect(mode).not.toContain(word);
      for (const word of mode) expect(view).not.toContain(word);
    }
  });

  it("wires each switch to its own keys, and only those keys", () => {
    // The views live in ConversationView, the modes in Composer. This is
    // the pin that catches the two being swapped (or one being wired to
    // the other's key) without touching either component in the fix.
    expect(VIEW_SOURCE).toContain('t("conv.chatTab")');
    expect(VIEW_SOURCE).toContain('t("conv.contextTab")');
    expect(VIEW_SOURCE).toContain('label={t("conv.viewLabel")}');
    expect(VIEW_SOURCE).not.toContain('t("conv.modeChat")');
    expect(VIEW_SOURCE).not.toContain('t("conv.agentToggle")');

    expect(COMPOSER_SOURCE).toContain('t("conv.modeChat")');
    expect(COMPOSER_SOURCE).toContain('t("conv.agentToggle")');
    expect(COMPOSER_SOURCE).not.toContain('t("conv.chatTab")');
    expect(COMPOSER_SOURCE).not.toContain('t("conv.contextTab")');
  });

  it("bounds the option labels, so neither can run away in a split pane", () => {
    // Honest scope: this is a LENGTH bound, not a layout measurement —
    // vitest/jsdom has no layout. The switch sits in `.nex-context-tabs`
    // (context.css), a full-bleed flex band inside a pane; the segmented
    // group is an inline-flex child whose options are `whitespace-nowrap`.
    // EN goes from 4/7 characters to 12/7 and RU is shorter than EN, so
    // the row's widest case grows by roughly one 60px label — the reason
    // the check is here at all is that a rename to something like
    // "Conversation thread" (19) would have no abbreviation to fall back
    // on and would have to become a finding, not a shortened word.
    for (const locale of ["en", "ru"] as const) {
      for (const key of ["conv.chatTab", "conv.contextTab"] as const) {
        expect(tr(locale, key).length).toBeLessThanOrEqual(12);
      }
    }
  });
});
