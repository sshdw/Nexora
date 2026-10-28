//! SearchBox debounce / loading / panel-open contract (#228).
//!
//! The sidebar search must never open its results panel for a
//! loading-only state: the spinner lives inline in the input, and the
//! panel opens only when results or an error actually arrive. These pins
//! drive the real component over the real `search` IPC wrapper (mocked at
//! `@tauri-apps/api/core`, like the sibling suites) so the debounce path
//! is exercised, not a synchronous stub.
//!
//! No timing assertions (policy): the debounced call is awaited with
//! `waitFor`, never against a clock.

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { LocaleProvider } from "../lib/useLocale";
import SearchBox from "./SearchBox";
import type { Conversation, SearchResults } from "../lib/tauri";

const { mockInvoke } = vi.hoisted(() => {
  const mockInvoke = vi.fn();
  return { mockInvoke };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

afterEach(() => {
  cleanup();
});

beforeEach(() => {
  mockInvoke.mockReset();
  // Default: a settled empty payload, so a stray debounce never rejects
  // with a non-promise. Individual tests override with their own result.
  mockInvoke.mockResolvedValue(emptyResults());
});

function emptyResults(): SearchResults {
  return { conversations: [], message_matches: [], prompts: [] };
}

function conversation(id: number, title: string): Conversation {
  return {
    id,
    title,
    status: "active",
    created_at: 0,
    updated_at: 0,
    workspace_root: null,
  };
}

function renderSearchBox() {
  const onSelectResult = vi.fn();
  render(
    <LocaleProvider>
      <SearchBox conversations={[]} onSelectResult={onSelectResult} />
    </LocaleProvider>,
  );
  return { onSelectResult, input: screen.getByRole("searchbox") };
}

const resultsPanel = () =>
  screen.queryByRole("group", { name: "Search results" });

/** Manually settled promise, so the in-flight window is deterministic. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("SearchBox loading / panel-open contract (#228)", () => {
  it("keeps the panel closed while loading with no prior results, spinner inline", () => {
    const { input } = renderSearchBox();

    fireEvent.change(input, { target: { value: "abc" } });

    // Loading-only: the panel must not open, and the wait signal must be
    // the inline adornment, not a floating status over the history.
    expect(resultsPanel()).toBeNull();
    const spinner = screen.getByRole("status");
    expect(spinner).toHaveTextContent("Searching");
    // #230 Phase 1 replaced the two class pins with the STRUCTURAL
    // contract they stood for, which is what actually keeps the spinner
    // inside the field: the status node is a child of the same wrapper as
    // the input, so it is painted IN the field and not over the list. (The
    // field's trailing padding is now unconditional, so no "-loading"
    // class exists to toggle: the adornment cannot shift the text as it
    // appears.)
    const field = input.parentElement;
    expect(field).not.toBeNull();
    expect(field!.contains(spinner)).toBe(true);
    expect(field!.querySelector('[data-slot="search-adornment"]')).toBe(spinner);
  });

  it("opens the panel once results arrive", async () => {
    mockInvoke.mockResolvedValueOnce({
      conversations: [conversation(1, "Roadmap notes")],
      message_matches: [],
      prompts: [],
    });
    const { input } = renderSearchBox();

    fireEvent.change(input, { target: { value: "roadmap" } });
    expect(resultsPanel()).toBeNull(); // still loading, no prior results

    await waitFor(() => expect(resultsPanel()).not.toBeNull());
    expect(screen.getByText("Roadmap notes")).toBeInTheDocument();
    // The inline spinner is loading-only; it is gone once data lands.
    expect(screen.queryByRole("status")).toBeNull();
    // …and the panel is the floating layer it was before: it renders the
    // result rows, never a bare status line.
    expect(screen.getByRole("group", { name: "Search results" })).toHaveAttribute(
      "data-slot",
      "search-results",
    );
  });

  it("drops the previous results when a new query starts", async () => {
    mockInvoke.mockResolvedValueOnce({
      conversations: [conversation(1, "Roadmap notes")],
      message_matches: [],
      prompts: [],
    });
    const { input } = renderSearchBox();

    fireEvent.change(input, { target: { value: "roadmap" } });
    await waitFor(() => expect(resultsPanel()).not.toBeNull());

    // Refining the query: stale hits must not linger over the list while
    // the new one settles (#228).
    mockInvoke.mockResolvedValueOnce(emptyResults());
    fireEvent.change(input, { target: { value: "roadmap x" } });
    expect(resultsPanel()).toBeNull();
    expect(screen.queryByText("Roadmap notes")).toBeNull();
    expect(screen.getByRole("status")).toBeInTheDocument();
  });

  it("opens the panel with an error when the search fails", async () => {
    mockInvoke.mockRejectedValueOnce({
      kind: "unknown",
      message: "search backend unreachable",
    });
    const { input } = renderSearchBox();

    fireEvent.change(input, { target: { value: "abc" } });
    expect(resultsPanel()).toBeNull();

    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(
        "search backend unreachable",
      ),
    );
    expect(resultsPanel()).not.toBeNull();
  });
});

// MAP row 2: empty-with-query is the `empty.md` pattern AND it must
// quote the query back. A bare "No results" line would leave the user
// guessing which of several near-identical queries came back empty.
describe("SearchBox empty-with-query (#230 Phase 1)", () => {
  it("answers with the exact query the user typed", async () => {
    const { input } = renderSearchBox();

    fireEvent.change(input, { target: { value: "  разговор  " } });

    await waitFor(() => expect(resultsPanel()).not.toBeNull());
    const empty = document.querySelector('[data-slot="search-empty"]');
    expect(empty).not.toBeNull();
    // Trimmed (the query the backend was actually asked), quoted back.
    expect(empty).toHaveTextContent("No results for “разговор”");
  });
});

// #233: the debounce cleanup only cancelled the timer, so a request that had
// already fired could still resolve and write state for a query the field no
// longer holds. Both races are driven by a manually settled `invoke` promise.
describe("SearchBox in-flight identity guard (#233)", () => {
  it("does not let a response for a cleared query open the panel", async () => {
    const pending = deferred<SearchResults>();
    mockInvoke.mockReturnValueOnce(pending.promise);
    const { input } = renderSearchBox();

    fireEvent.change(input, { target: { value: "abc" } });
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledTimes(1));

    // Clear the field while `abc` is still in flight.
    fireEvent.change(input, { target: { value: "" } });
    expect(resultsPanel()).toBeNull();

    await act(async () => {
      pending.resolve({
        conversations: [conversation(1, "Stale hit")],
        message_matches: [],
        prompts: [],
      });
      await pending.promise;
    });

    // The cleared field must stay closed: no stale panel, no stale hits.
    expect(resultsPanel()).toBeNull();
    expect(screen.queryByText("Stale hit")).toBeNull();
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("never applies an old response after the query was retyped", async () => {
    const first = deferred<SearchResults>();
    const second = deferred<SearchResults>();
    mockInvoke
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    const { input } = renderSearchBox();

    fireEvent.change(input, { target: { value: "abc" } });
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledTimes(1));

    // Retype before the first request resolves, so its response is superseded.
    fireEvent.change(input, { target: { value: "abd" } });
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledTimes(2));

    await act(async () => {
      first.resolve({
        conversations: [conversation(1, "Old hit")],
        message_matches: [],
        prompts: [],
      });
      await first.promise;
    });

    // The old response must not land, not even briefly: the second query is
    // still loading, and loading-only never opens the panel (#228).
    expect(resultsPanel()).toBeNull();
    expect(screen.queryByText("Old hit")).toBeNull();

    await act(async () => {
      second.resolve({
        conversations: [conversation(2, "New hit")],
        message_matches: [],
        prompts: [],
      });
      await second.promise;
    });

    await waitFor(() => expect(screen.getByText("New hit")).toBeInTheDocument());
    expect(screen.queryByText("Old hit")).toBeNull();
    expect(screen.queryByRole("status")).toBeNull();
  });
});
