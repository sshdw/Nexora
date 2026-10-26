//! Tab strip + pane header contract tests (#230 Phase 2, MAP rows 4-6).
//!
//! These are BEHAVIOUR pins, not snapshots of class names. What they
//! hold down:
//!
//!   1. ONE Split control (SYSTEM §4). It lived in the tab strip AND
//!      in every pane header, so a split offered four identical
//!      buttons for one action. The strip owns it; the pane header
//!      renders none.
//!   2. The `+` new-tab button sits at the END of the row (MAP row 4).
//!   3. The active tab scrolls itself into view when the strip
//!      overflows — jsdom has no layout, so `scrollIntoView` is
//!      stubbed and the CALL is the assertion.
//!   4. Arrow/Home/End move and activate (the tablist contract), and
//!      each tab keeps its own close control.
//!   5. The workspace chip shows the folder NAME and keeps the full
//!      path in `title=` only, and carries no glyph of its own: the
//!      emoji it used to hold was not an icon from `icons.tsx`, the
//!      sole icon source (SYSTEM §1).

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { LocaleProvider } from "../lib/useLocale";
import ConversationPane from "./ConversationPane";
import ConversationTabs, { type TabEntry } from "./ConversationTabs";
import WorkspaceChip from "./WorkspaceChip";
import type { AutonomyStore } from "../lib/useAutonomy";
import type { Conversation } from "../lib/tauri";

// The pane's content is not under test here (thread/composer are P3/P4
// surfaces) — stubbing it keeps this suite on the header and the strip.
vi.mock("./ConversationView", () => ({
  default: () => <div data-testid="conversation-view" />,
}));

const TABS: TabEntry[] = [
  { id: 1, title: "Refactor the parser", archived: false },
  { id: 2, title: "Обзор миграции схемы", archived: false },
];

function tabStrip(overrides: Partial<Parameters<typeof ConversationTabs>[0]> = {}) {
  const props = {
    tabs: TABS,
    activeId: TABS[0]!.id,
    splitId: null,
    onActivate: vi.fn(),
    onClose: vi.fn(),
    onNewConversation: vi.fn(),
    creating: false,
    zen: false,
    onToggleZen: vi.fn(),
    splitOpen: false,
    onToggleSplit: vi.fn(),
    ...overrides,
  };
  const view = render(
    <LocaleProvider>
      <ConversationTabs {...props} />
    </LocaleProvider>,
  );
  return { props, view };
}

function conversation(overrides: Partial<Conversation> = {}): Conversation {
  return {
    id: 1,
    title: "Refactor the parser",
    status: "active",
    created_at: 0,
    updated_at: 0,
    workspace_root: null,
    ...overrides,
  };
}

const autonomy = {
  mode: "suggest",
  setMode: vi.fn(),
  budgetUsd: 1,
  setBudgetUsd: vi.fn(),
  dailyUsd: 0,
} as unknown as AutonomyStore;

function pane(overrides: Partial<Parameters<typeof ConversationPane>[0]> = {}) {
  const props = {
    conversation: conversation(),
    selectedProvider: null,
    selectedModel: null,
    models: [],
    onSelectModel: vi.fn(),
    modelError: null,
    autonomy,
    draft: "",
    setDraft: vi.fn(),
    onOpenSettings: vi.fn(),
    onMessageSent: vi.fn(),
    onExport: vi.fn(),
    workspaceRoot: null,
    workspaceLoading: false,
    secondary: false,
    ...overrides,
  };
  render(
    <LocaleProvider>
      <ConversationPane {...props} />
    </LocaleProvider>,
  );
  return props;
}

afterEach(() => {
  cleanup();
});

// jsdom implements no scrolling at all — `scrollIntoView` is simply
// absent from Element.prototype, and the strip calls it on every
// active-tab change. Stubbed for the whole file (one test asserts on
// the call itself), restored on teardown.
const realScrollIntoView = Element.prototype.scrollIntoView;
beforeEach(() => {
  Element.prototype.scrollIntoView = vi.fn();
});
afterEach(() => {
  Element.prototype.scrollIntoView = realScrollIntoView;
});

describe("exactly one Split control (SYSTEM §4, MAP row 5)", () => {
  it("keeps the opener in the tab strip and flips its label with the state", () => {
    tabStrip();
    const closedControl = screen.getAllByRole("button", { name: /split/i });
    expect(closedControl).toHaveLength(1);
    expect(closedControl[0]).toHaveTextContent("Split");

    cleanup();

    const open = tabStrip({ splitOpen: true });
    const openControl = screen.getAllByRole("button", { name: /split/i });
    expect(openControl).toHaveLength(1);
    expect(openControl[0]).toHaveTextContent("Close split");
    fireEvent.click(openControl[0]!);
    expect(open.props.onToggleSplit).toHaveBeenCalledTimes(1);
  });

  it("renders no Split control in the pane header", () => {
    // The header owns no split props at all — the prop chain a header
    // Split button needed was removed with the control (SYSTEM §4), so
    // the default pane is the whole surface under test.
    pane();
    expect(screen.queryAllByRole("button", { name: /split/i })).toHaveLength(0);
    // The header keeps what MAP row 5 gives it: the export action.
    expect(screen.getByRole("button", { name: /export/i })).toBeInTheDocument();
  });
});

describe("tab strip grammar (MAP row 4)", () => {
  it("puts the new-tab button last, with the name the text button had", () => {
    const { props } = tabStrip();
    const newTab = screen.getByRole("button", { name: "New conversation" });
    // Last in DOM order: creation never sits between two open tabs.
    const focusable = document.querySelectorAll(
      '.nex-tabstrip button, .nex-tabstrip a[href]',
    );
    expect(focusable[focusable.length - 1]).toBe(newTab);

    fireEvent.click(newTab);
    expect(props.onNewConversation).toHaveBeenCalledTimes(1);
  });

  it("marks the active tab with aria-selected and nothing else", () => {
    tabStrip();
    const selected = screen.getAllByRole("tab").filter(
      (tab) => tab.getAttribute("aria-selected") === "true",
    );
    expect(selected).toHaveLength(1);
    expect(selected[0]).toHaveAccessibleName("Refactor the parser");
  });

  it("scrolls the active tab into view when it changes", () => {
    // jsdom implements no scrolling; the CONTRACT is that the strip
    // asks the browser to bring the active tab into view.
    const scroll = vi.mocked(Element.prototype.scrollIntoView);
    const { view } = tabStrip();
    scroll.mockClear();
    view.rerender(
      <LocaleProvider>
        <ConversationTabs
          tabs={TABS}
          activeId={TABS[1]!.id}
          splitId={null}
          onActivate={vi.fn()}
          onClose={vi.fn()}
          onNewConversation={vi.fn()}
          creating={false}
          zen={false}
          onToggleZen={vi.fn()}
          splitOpen={false}
          onToggleSplit={vi.fn()}
        />
      </LocaleProvider>,
    );
    expect(scroll).toHaveBeenCalledWith({ block: "nearest", inline: "nearest" });
  });

  it("moves and activates with the arrow keys, and closes per tab", () => {
    const { props } = tabStrip();
    const [first, second] = screen.getAllByRole("tab");

    fireEvent.keyDown(first!, { key: "ArrowRight" });
    expect(second).toHaveFocus();
    expect(props.onActivate).toHaveBeenCalledWith(2);

    fireEvent.keyDown(second!, { key: "Home" });
    expect(first).toHaveFocus();
    // Arriving at the tab that is ALREADY primary moves focus and
    // nothing else — no redundant activation.
    expect(props.onActivate).toHaveBeenCalledTimes(1);

    fireEvent.keyDown(first!, { key: "End" });
    expect(second).toHaveFocus();

    fireEvent.click(screen.getByRole("button", { name: "Close Обзор миграции схемы" }));
    expect(props.onClose).toHaveBeenCalledWith(2);
  });

  it("annotates the tab shown in the split pane", () => {
    tabStrip({ splitId: 2 });
    const splitTab = screen.getAllByRole("tab")[1]!;
    expect(splitTab).toHaveAccessibleName("Обзор миграции схемы (open in split pane)");
  });
});

describe("quiet control tone — no control takes the UA's `buttontext` (Defect A)", () => {
  /** One strip/pane-header control: ghost at rest, brightening through
   *  the variant's own utility.
   *
   *  What this pins, and why it is the contract rather than a class-name
   *  snapshot: a `<button>` with no colour of its own takes the UA's
   *  `color: buttontext`, and with no `color-scheme` on the themed roots
   *  that resolved to BLACK — the tab close ×, the `+`, Split, Zen and
   *  the pane header's export glyph were invisible on the strip's
   *  elevated bar (measured on real Tauri frames). `ghost` is the variant
   *  all five use (M3Button `quiet` → ghost, M3IconButton → ghost), and
   *  it declares NO rest `text-*` — that absence is the bug's mechanism.
   *  Their rest tone now comes from the one base-layer fallback in
   *  src/index.css; the hover brighten from the utility asserted below.
   *  theme.test.ts pins the fallback's layer and its non-`!important`
   *  shape, which is what keeps a variant's own `text-*` on top. */
  function expectQuietControl(control: HTMLElement) {
    expect(control).toHaveAttribute("data-variant", "ghost");
    const classes = control.className.split(/\s+/).filter(Boolean);
    // The brighten rides the existing variant utility, not new CSS.
    expect(classes).toContain("hover:text-foreground");
    // No REST colour utility. (`text-sm` on the base string is the type
    // scale, not a colour — it is filtered by name below.) If a colour
    // ever lands here, the control has stopped using the fallback and the
    // reasoning above stops holding.
    const restColours = classes.filter((token) =>
      /^text-(?:foreground|background|card-foreground|popover-foreground|primary|primary-foreground|secondary-foreground|muted-foreground|accent-foreground|destructive|tick|ring|border|input)$/.test(
        token,
      ),
    );
    expect(restColours).toEqual([]);
  }

  it("covers the tab close, the new-tab `+`, Split and Zen", () => {
    tabStrip();
    expectQuietControl(
      screen.getByRole("button", { name: "Close Refactor the parser" }),
    );
    expectQuietControl(screen.getByRole("button", { name: "New conversation" }));
    expectQuietControl(screen.getByRole("button", { name: "Split" }));
    expectQuietControl(screen.getByRole("button", { name: "Zen" }));
  });

  it("covers the pane header's export control", () => {
    pane();
    expectQuietControl(screen.getByRole("button", { name: /export/i }));
  });

  it("leaves the tab label a plain button on its own token, not a ghost", () => {
    // The tab row was never UA-coloured: `.nex-tab-label` declares
    // `color: inherit` and `.nex-tab` owns the token (a class rule, so
    // it out-ranks the base `button` type selector). Asserted so the
    // new fallback cannot be read as "every button in the strip is a
    // ghost now" — the tabs keep their own two-signal grammar.
    tabStrip();
    const tab = screen.getAllByRole("tab")[0]!;
    expect(tab).not.toHaveAttribute("data-variant");
    expect(tab.className).toBe("nex-tab-label");
  });
});

describe("workspace chip (MAP row 5, SYSTEM §4)", () => {
  function chip(props: Parameters<typeof WorkspaceChip>[0]) {
    const { container } = render(
      <LocaleProvider>
        <WorkspaceChip {...props} />
      </LocaleProvider>,
    );
    return container;
  }

  it("shows the folder NAME and keeps the path in the tooltip", () => {
    const container = chip({ root: "D:\\projects\\nx", loading: false });
    const el = container.querySelector(".nex-workspace-chip")!;
    expect(el).toHaveTextContent("nx");
    expect(el).not.toHaveTextContent("projects");
    expect(el.getAttribute("title")).toBe("D:\\projects\\nx");
    expect(el.getAttribute("aria-label")).toContain("D:\\projects\\nx");
  });

  it("names a POSIX root the same way", () => {
    const container = chip({ root: "/home/gogog/projects/nx/", loading: false });
    expect(container.querySelector(".nex-workspace-chip")).toHaveTextContent("nx");
  });

  it("carries no glyph: no aria-hidden slot and no pictographic character", () => {
    const container = chip({ root: "D:\\projects\\nx", loading: false });
    // The emoji this chip used to render lived in exactly such a slot.
    expect(container.querySelector('[aria-hidden="true"]')).toBeNull();
    expect(container.textContent ?? "").not.toMatch(/\p{Extended_Pictographic}/u);
  });

  it("shows the loading and unset states whole", () => {
    expect(chip({ root: null, loading: true })).toHaveTextContent("Workspace…");
    expect(chip({ root: null, loading: false })).toHaveTextContent("Workspace unset");
  });
});