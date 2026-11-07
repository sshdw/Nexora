//! Error boundaries (ADD-05) — component behaviour + the root-level pin.
//!
//! The boundary has three wiring levels (review 05-ADD §3.5a). The two
//! slot levels are proven by real renders in DockPanel.test.tsx and
//! ConversationPane.test.tsx; the ROOT level is pinned here as source,
//! because importing src/main.tsx mounts the whole app (side-effectful
//! bootstrap), and what the root level owns is exactly one fact: the bare
//! `render(<App/>)` is gone. The repository already pins contracts as
//! source this way (theme.test.ts "reads src/index.css and src/main.tsx as
//! SOURCE"; classCoverage.test.ts).

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ReactNode } from "react";

import { tr } from "../lib/strings";
import { LocaleProvider } from "../lib/useLocale";
import { readRepoFile } from "../test/readRepoFile";
import ErrorBoundary from "./ErrorBoundary";

const BODY = tr("en", "err.boundaryBody");
const RETRY = tr("en", "common.retry");

/** A child that throws for as long as the "environment" is broken. The flag
 *  (not a per-attempt counter) is deliberate: React 19 renders concurrently,
 *  and a pass that throws is once retried synchronously from the root — a
 *  throw-once child would be consumed by that internal retry and the
 *  fallback would never show. Staying broken across both passes is also the
 *  real-world shape: the fault persists until something outside the tree
 *  changes, the user clicks Retry, the subtree re-renders and recovers. */
let broken = true;
function FlakyChild() {
  if (broken) throw new Error("boom");
  return <p>recovered</p>;
}

/** A permanently crashing child — the boundary must contain it. */
function Boom(): never {
  throw new Error("boom");
}

function underBoundary(children: ReactNode, root = false) {
  return render(
    <LocaleProvider>
      <ErrorBoundary root={root}>{children}</ErrorBoundary>
    </LocaleProvider>
  );
}

afterEach(() => {
  cleanup();
  broken = true;
  vi.restoreAllMocks();
});

describe("error boundary — component level", () => {
  it("renders children untouched when nothing throws", () => {
    underBoundary(<p>fine</p>);
    expect(screen.getByText("fine")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("renders the muted fallback for a throwing child and recovers on Retry", () => {
    // The boundary and React both report the crash on console.error; the
    // spy keeps the test output clean and pins that the crash is at least
    // logged, never swallowed silently.
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => {});

    underBoundary(<FlakyChild />);

    // Fallback: the one-sentence muted surface, announced (role=alert),
    // with the catalog Retry action.
    expect(screen.getByRole("alert")).toHaveTextContent(BODY);
    expect(screen.queryByText("recovered")).toBeNull();

    // The transient fault clears (outside the tree — the boundary cannot
    // know); Retry resets the boundary state and re-renders the same
    // children, which now mount.
    broken = false;
    fireEvent.click(screen.getByRole("button", { name: RETRY }));

    expect(screen.getByText("recovered")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(consoleError).toHaveBeenCalled();
  });

  it("contains a permanently crashing child at the fallback instead of unmounting", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    underBoundary(
      <>
        <p>before</p>
        <Boom />
      </>
    );
    // The healthy sibling goes down with the slot — a boundary replaces its
    // WHOLE children slot — but the shell above the boundary survives, which
    // is exactly what the per-slot wiring relies on.
    expect(screen.queryByText("before")).toBeNull();
    expect(screen.getByRole("alert")).toHaveTextContent(BODY);
  });

  it("centers the card on the window surface in the root variant", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    const { container } = underBoundary(<Boom />, true);
    const root = container.querySelector(".nex-error-boundary-root");
    expect(root).not.toBeNull();
    expect(root?.querySelector(".nex-error-boundary")).not.toBeNull();
  });
});

describe("error boundary — root-level wiring pin (ADD-05, main.tsx)", () => {
  it("main.tsx wraps <App/> in the root boundary — the render is no longer bare", () => {
    const source = readRepoFile("src/main.tsx");
    expect(source).toMatch(
      /import ErrorBoundary from "\.\/components\/ErrorBoundary"/
    );
    // The exact finding (main.tsx:40 bare render) cannot regress silently:
    // the boundary must sit between StrictMode and the app.
    expect(source).toMatch(
      /<ErrorBoundary root>\s*<App \/>\s*<\/ErrorBoundary>/
    );
    // And the root variant is requested, so the last-resort screen centres
    // on the window instead of flowing into an unstyled document body.
    expect(source).toMatch(/<ErrorBoundary root>/);
  });
});
