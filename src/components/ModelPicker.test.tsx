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
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { LocaleProvider } from "../lib/useLocale";
import ModelPicker from "./ModelPicker";

beforeAll(() => {
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    }
  );
});

afterAll(() => {
  vi.unstubAllGlobals();
});

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

describe("ModelPicker — connection grouping and effort capabilities", () => {
  it("groups models by connection_id and dispatches connection-aware selection", async () => {
    const onSelectConnectionModel = vi.fn();
    renderPicker({
      selectedConnectionId: "openai",
      selectedModel: "gpt-5.6-terra",
      models: ["gpt-5.6-terra", "claude-sonnet-5"],
      connectionGroups: [
        {
          connectionId: "openai",
          connectionName: "OpenAI",
          models: ["gpt-5.6-terra"],
        },
        {
          connectionId: "anthropic",
          connectionName: "Anthropic",
          models: ["claude-sonnet-5"],
        },
      ],
      onSelectConnectionModel,
    });
    openPicker();
    expect(screen.getByText("OpenAI")).toBeInTheDocument();
    expect(screen.getByText("Anthropic")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("option", { name: "claude-sonnet-5" }));
    expect(onSelectConnectionModel).toHaveBeenCalledWith(
      "anthropic",
      "claude-sonnet-5"
    );
    await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());
  });

  it("renders only the supported effort levels for the active model", () => {
    const onSelectEffort = vi.fn();
    renderPicker({
      selectedConnectionId: "gemini",
      selectedModel: "gemini-3.6-flash",
      capabilities: {
        connection_id: "gemini",
        model_id: "gemini-3.6-flash",
        context_window: 1000000,
        supported_efforts: ["low", "high"],
        supports_thinking: true,
        supports_attachments: true,
        supports_tools: true,
        provenance: "built_in_catalog",
        has_manual_override: false,
      },
      selectedEffort: "low",
      onSelectEffort,
    });
    openPicker();

    const effortGroup = screen.getByRole("group", { name: "Effort" });
    expect(
      within(effortGroup).getByRole("button", { name: "Low" })
    ).toBeInTheDocument();
    expect(
      within(effortGroup).getByRole("button", { name: "High" })
    ).toBeInTheDocument();
    expect(
      within(effortGroup).queryByRole("button", { name: "Medium" })
    ).toBeNull();
    expect(
      within(effortGroup).queryByRole("button", { name: "Max" })
    ).toBeNull();

    expect(screen.getByRole("slider", { name: "Effort" })).toHaveAttribute(
      "aria-valuemax",
      "1"
    );

    fireEvent.click(within(effortGroup).getByRole("button", { name: "High" }));
    expect(onSelectEffort).toHaveBeenCalledWith("high");
  });

  it("hides effort controls with an honest unsupported notice when empty", () => {
    renderPicker({
      selectedConnectionId: "gemini",
      selectedModel: "gemini-3.1-flash-lite",
      capabilities: {
        connection_id: "gemini",
        model_id: "gemini-3.1-flash-lite",
        context_window: 1000000,
        supported_efforts: [],
        supports_thinking: false,
        supports_attachments: true,
        supports_tools: true,
        provenance: "built_in_catalog",
        has_manual_override: false,
      },
      onSelectEffort: vi.fn(),
    });
    openPicker();

    expect(screen.getByTestId("effort-unsupported")).toHaveTextContent(
      "Effort levels not supported for this model"
    );
    expect(screen.queryByRole("slider", { name: "Effort" })).toBeNull();
    expect(screen.queryByRole("group", { name: "Effort" })).toBeNull();
  });

  it("reflects manual capability overrides over unknown or unsupported defaults", () => {
    const onSelectEffort = vi.fn();
    renderPicker({
      selectedConnectionId: "openai_compat",
      selectedModel: "local-reasoner",
      capabilities: {
        connection_id: "openai_compat",
        model_id: "local-reasoner",
        context_window: 128000,
        supported_efforts: ["medium", "xhigh"],
        supports_thinking: true,
        supports_attachments: false,
        supports_tools: true,
        provenance: "manual_override",
        has_manual_override: true,
      },
      selectedEffort: "xhigh",
      onSelectEffort,
    });
    openPicker();

    expect(screen.getByText("Manual")).toBeInTheDocument();
    expect(screen.getByText("Manual override")).toBeInTheDocument();
    const effortGroup = screen.getByRole("group", { name: "Effort" });
    expect(
      within(effortGroup).getByRole("button", { name: "Medium" })
    ).toBeInTheDocument();
    expect(
      within(effortGroup).getByRole("button", { name: "Max" })
    ).toBeInTheDocument();
    expect(
      within(effortGroup).queryByRole("button", { name: "Low" })
    ).toBeNull();
  });

  it("groups models across multiple new preset connections and switches Effort per model", () => {
    const onSelectConnectionModel = vi.fn();
    renderPicker({
      selectedConnectionId: "deepseek",
      selectedModel: "deepseek-reasoner",
      connectionGroups: [
        {
          connectionId: "deepseek",
          connectionName: "DeepSeek",
          models: ["deepseek-chat", "deepseek-reasoner"],
        },
        {
          connectionId: "xai",
          connectionName: "xAI",
          models: ["grok-3", "grok-3-mini"],
        },
        {
          connectionId: "mistral",
          connectionName: "Mistral",
          models: ["mistral-large-latest", "codestral-latest"],
        },
      ],
      capabilities: {
        connection_id: "deepseek",
        model_id: "deepseek-reasoner",
        context_window: 128000,
        supported_efforts: ["low", "medium", "high"],
        supports_thinking: true,
        supports_attachments: false,
        supports_tools: true,
        provenance: "built_in_catalog",
        has_manual_override: false,
      },
      selectedEffort: "high",
      onSelectEffort: vi.fn(),
      onSelectConnectionModel,
    });
    openPicker();

    expect(screen.getByText("DeepSeek")).toBeInTheDocument();
    expect(screen.getByText("xAI")).toBeInTheDocument();
    expect(screen.getByText("Mistral")).toBeInTheDocument();

    const effortGroup = screen.getByRole("group", { name: "Effort" });
    expect(within(effortGroup).getByRole("button", { name: "Low" })).toBeInTheDocument();
    expect(within(effortGroup).getByRole("button", { name: "High" })).toBeInTheDocument();
    expect(within(effortGroup).queryByRole("button", { name: "Max" })).toBeNull();

    fireEvent.click(screen.getByRole("option", { name: "grok-3-mini" }));
    expect(onSelectConnectionModel).toHaveBeenCalledWith("xai", "grok-3-mini");
  });

  it("renders returned display names in trigger and options while dispatching the underlying model slug", () => {
    const onSelectConnectionModel = vi.fn();
    renderPicker({
      selectedModel: "gpt-5.4-plan",
      selectedConnectionId: "chatgpt_subscription",
      models: ["gpt-5.4-plan", "gpt-5.4-mini-plan"],
      connectionGroups: [
        {
          connectionId: "chatgpt_subscription",
          connectionName: "ChatGPT subscription",
          models: ["gpt-5.4-plan", "gpt-5.4-mini-plan"],
          modelLabels: {
            "gpt-5.4-plan": "GPT-5.4",
            "gpt-5.4-mini-plan": "GPT-5.4 Mini",
          },
        },
      ],
      onSelectConnectionModel,
    });

    const trigger = screen.getByRole("button", { name: "Agent mode and model" });
    expect(trigger).toHaveTextContent("GPT-5.4");

    openPicker();
    expect(screen.getByRole("option", { name: "GPT-5.4" })).toBeInTheDocument();
    const miniOption = screen.getByRole("option", { name: "GPT-5.4 Mini" });
    fireEvent.click(miniOption);
    expect(onSelectConnectionModel).toHaveBeenCalledWith(
      "chatgpt_subscription",
      "gpt-5.4-mini-plan"
    );
  });
});
