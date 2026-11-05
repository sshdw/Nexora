//! Thread boundary (ADD-05) — the conversation subtree is the shell's most
//! dynamic surface (per-frame agent-run renders, markdown bodies), so a
//! frame-shaped runtime error must degrade to the boundary strip INSIDE the
//! pane; the pane header and the shell around it stay up. The crashing child
//! is the real seam (ConversationView) mocked at module level to throw on
//! render — jsdom proves the boundary wiring, not the pane's internals.

import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { tr } from "../lib/strings";
import type { AutonomyStore } from "../lib/useAutonomy";
import type { WorkspaceStore } from "../lib/useWorkspace";
import { LocaleProvider } from "../lib/useLocale";
import ConversationPane from "./ConversationPane";

// The mocked ConversationView throws during render — the exact fault class
// the boundary exists for (an uncaught render error in the thread subtree).
vi.mock("./ConversationView", () => ({
  default: function CrashingConversationView(): never {
    throw new Error("thread boom");
  },
}));

function autonomyStub(): AutonomyStore {
  return {
    mode: "semi_autonomous",
    loading: false,
    error: null,
    reload: vi.fn(async () => {}),
    setMode: vi.fn(async () => null),
  };
}

function workspaceStub(): WorkspaceStore {
  return {
    root: null,
    recent: [],
    roots: [],
    loading: false,
    saving: false,
    error: null,
    reload: vi.fn(async () => {}),
    pickFolder: vi.fn(async () => null),
    selectRecent: vi.fn(async () => null),
    addRootFolder: vi.fn(async () => null),
    addRoot: vi.fn(async () => null),
    removeRoot: vi.fn(async () => null),
  };
}

function pane() {
  return (
    <LocaleProvider>
      <ConversationPane
        conversation={{
          id: 1,
          title: "Ops",
          status: "active",
          created_at: 1_700_000_000,
          updated_at: 1_700_000_000,
          workspace_root: null,
        }}
        selectedProvider={null}
        selectedModel={null}
        models={[]}
        onSelectModel={() => {}}
        modelError={null}
        autonomy={autonomyStub()}
        draft=""
        setDraft={() => {}}
        onOpenSettings={() => {}}
        onMessageSent={() => {}}
        onExport={() => {}}
        workspace={workspaceStub()}
        secondary={false}
      />
    </LocaleProvider>
  );
}

afterEach(cleanup);

describe("thread boundary (ADD-05)", () => {
  it("degrades a crashing conversation subtree to the strip inside the pane, header intact", () => {
    // The boundary and React report the crash on console.error; the spy
    // keeps the output clean and pins that the crash is logged.
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});

    render(pane());

    // The thread slot degraded to the boundary strip with its Retry action.
    expect(screen.getByRole("alert")).toHaveTextContent(tr("en", "err.boundaryBody"));
    expect(screen.getByRole("button", { name: tr("en", "common.retry") })).toBeInTheDocument();

    // The pane shell survived: title header and the export action are still
    // mounted — a thread crash is contained to the pane body.
    expect(screen.getByRole("heading", { name: "Ops" })).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: tr("en", "app.exportConversation") }),
    ).toBeInTheDocument();
    expect(consoleError).toHaveBeenCalled();
  });
});
