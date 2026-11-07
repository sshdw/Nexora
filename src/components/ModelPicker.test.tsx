//! Direct component pins for `ModelPicker` (AC-211 minimum set, review 03
//! §6.3 item 4). The composer-integrated path (open, filter, arrow keys,
//! Enter/Escape/click pick, error and settings footer) is already covered in
//! `Composer.test.tsx` through the real `Composer`; this file pins the
//! picker's own contract where only a direct render reaches it: the active
//! row initializing on the current selection when it survives the filter
//! (first row otherwise), hover moving the active row, the search query
//! resetting on close so a reopen shows the unfiltered list, and the
//! empty-filter Escape path closing without a pick.

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { LocaleProvider } from "../lib/useLocale";
import ModelPicker from "./ModelPicker";

afterEach(() => {
  cleanup();
});

type PickerProps = Parameters<typeof ModelPicker>[0];

function renderPicker(overrides: Partial<PickerProps> = {}) {
  const props: PickerProps = {
    selectedModel: "gpt-5.2",
    models: ["gpt-5.2", "gpt-5.2-mini", "claude-sonnet-4"],
    onSelectModel: () => {},
    onOpenSettings: () => {},
    ...overrides,
  };
  return render(
    <LocaleProvider>
      <ModelPicker {...props} />
    </LocaleProvider>
  );
}

const trigger = () =>
  screen.getByRole("button", { name: "Agent mode and model" });
const openPicker = () => fireEvent.click(trigger());
const list = () => screen.getByRole("listbox", { name: "Model" });
const search = () => screen.getByRole("searchbox", { name: "Search models" });

describe("ModelPicker — active row and query lifecycle", () => {
  it("lands the active row on the current selection when it survives the filter", () => {
    renderPicker({ selectedModel: "claude-sonnet-4" });
    openPicker();
    const options = within(list()).getAllByRole("option");
    // The selection is the third row: the active descendant names it, not
    // the first row.
    expect(search()).toHaveAttribute("aria-activedescendant", options[2]!.id);
    expect(options[2]).toHaveAttribute("data-active", "true");
  });

  it("falls back to the first row when the selection is not in the list", () => {
    renderPicker({ selectedModel: null });
    openPicker();
    const options = within(list()).getAllByRole("option");
    expect(search()).toHaveAttribute("aria-activedescendant", options[0]!.id);
  });

  it("moves the active row on hover without picking", () => {
    const onSelectModel = vi.fn();
    renderPicker({ onSelectModel });
    openPicker();
    const options = within(list()).getAllByRole("option");
    fireEvent.mouseEnter(options[1]!);
    expect(search()).toHaveAttribute("aria-activedescendant", options[1]!.id);
    expect(options[1]).toHaveAttribute("data-active", "true");
    expect(onSelectModel).not.toHaveBeenCalled();
  });

  it("picks the hovered row on click and re-renders from the prop only", async () => {
    const onSelectModel = vi.fn();
    const { rerender } = renderPicker({ onSelectModel });
    openPicker();
    fireEvent.click(screen.getByRole("option", { name: "claude-sonnet-4" }));
    expect(onSelectModel).toHaveBeenCalledWith("claude-sonnet-4");
    // The picker keeps no copy of the selection: the trigger still shows the
    // prop value until the provider store answers.
    expect(trigger()).toHaveTextContent("gpt-5.2");
    await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());

    // The store answering (new prop) is what moves the trigger label.
    rerender(
      <LocaleProvider>
        <ModelPicker
          selectedModel="claude-sonnet-4"
          models={["gpt-5.2", "gpt-5.2-mini", "claude-sonnet-4"]}
          onSelectModel={onSelectModel}
          onOpenSettings={() => {}}
        />
      </LocaleProvider>
    );
    expect(trigger()).toHaveTextContent("claude-sonnet-4");
  });

  it("resets the search query on close so a reopen shows the full list", async () => {
    renderPicker();
    openPicker();
    fireEvent.change(search(), { target: { value: "mini" } });
    expect(within(list()).getAllByRole("option")).toHaveLength(1);

    fireEvent.keyDown(search(), { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());

    openPicker();
    expect(within(list()).getAllByRole("option")).toHaveLength(3);
    expect(search()).toHaveValue("");
  });

  it("closes with Escape when the filter matches nothing, without picking", async () => {
    const onSelectModel = vi.fn();
    renderPicker({ onSelectModel });
    openPicker();
    fireEvent.change(search(), { target: { value: "no such model" } });
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(screen.getByText("No models match your search")).toBeInTheDocument();

    fireEvent.keyDown(search(), { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("searchbox")).toBeNull());
    expect(onSelectModel).not.toHaveBeenCalled();
  });
});
