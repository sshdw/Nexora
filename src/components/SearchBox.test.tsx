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

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
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

describe("SearchBox loading / panel-open contract (#228)", () => {
  it("keeps the panel closed while loading with no prior results, spinner inline", () => {
    const { input } = renderSearchBox();

    fireEvent.change(input, { target: { value: "abc" } });

    // Loading-only: the panel must not open, and the wait signal must be
    // the inline adornment, not a floating status over the history.
    expect(resultsPanel()).toBeNull();
    const spinner = screen.getByRole("status");
    expect(spinner).toHaveTextContent("Searching");
    expect(spinner).toHaveClass("nex-search-spinner");
    expect(input).toHaveClass("nex-search-input-loading");
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
    expect(input).not.toHaveClass("nex-search-input-loading");
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
