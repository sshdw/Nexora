import { act, cleanup, fireEvent, render, renderHook, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import type { PromptContextView, PromptSkill } from "../lib/tauri";
import { useComposerContext, type PromptSheet } from "../lib/useComposerContext";
import { LocaleProvider } from "../lib/useLocale";
import ComposerPromptSheet from "./ComposerPromptSheet";

const mocks = vi.hoisted(() => ({
  listPromptSkills: vi.fn(),
  previewPromptContext: vi.fn(),
  updatePromptSkill: vi.fn(),
  viewPromptSkill: vi.fn(),
}));
vi.mock("../lib/tauri", () => mocks);

const skill: PromptSkill = {
  reference: { source_id: "synthetic-source", revision: "synthetic-revision" },
  name: "audit",
  description: "Review the given material",
  scope: "project",
  path: "audit/SKILL.md",
  commit: null,
  sha256: "synthetic-hash",
  trusted: false,
  enabled: true,
  auto_select: false,
};

function renderSheet(sheet: PromptSheet) {
  const onReview = vi.fn(async () => true);
  function Harness() {
    const state = useComposerContext(7, "workspace", "", false);
    return (
      <LocaleProvider>
        <ComposerPromptSheet conversationId={7} state={state} sheet={sheet}
          onReview={onReview} onUseSkill={() => {}} onIncludeContext={() => {}} />
        <output>{JSON.stringify(state.references ?? null)}</output>
      </LocaleProvider>
    );
  }
  return { ...render(<Harness />), onReview };
}

beforeEach(() => {
  vi.resetAllMocks();
  mocks.listPromptSkills.mockResolvedValue([skill]);
});
afterEach(cleanup);

it("shows skill content as plaintext and includes only the trusted revision", async () => {
  const hostileMarkup = '<img src="https://example.invalid" onerror="alert(1)">';
  mocks.viewPromptSkill.mockResolvedValue({ summary: skill, text: hostileMarkup });
  mocks.updatePromptSkill.mockResolvedValue({ ...skill, trusted: true });
  const { container } = renderSheet({ kind: "skill", skill });
  expect(await screen.findByText(hostileMarkup)).toBeInTheDocument();
  expect(container.querySelector("img")).toBeNull();
  const include = screen.getByRole("button", { name: "Include in message" });
  expect(include).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "Trust this version" }));
  await waitFor(() => expect(include).toBeEnabled());
  expect(mocks.updatePromptSkill).toHaveBeenCalledWith(7, skill.reference, "trust", "en");
  fireEvent.click(include);
  expect(container.querySelector("output")?.textContent).toContain(skill.reference.revision);
  expect(container.querySelector("output")?.textContent).not.toContain(hostileMarkup);
});

it("previews a read-only Git review and waits for explicit send", async () => {
  const preview: PromptContextView = {
    reference: { selector: { kind: "diff", path: null }, revision: "git-revision" },
    label: "HEAD → working tree",
    text: "-old\n+new",
    omitted: true,
  };
  mocks.previewPromptContext.mockResolvedValue(preview);
  const { onReview } = renderSheet({ kind: "review" });
  expect(await screen.findByText("HEAD → working tree")).toBeInTheDocument();
  expect(screen.getByText("This preview is partial. Review sends only the changes shown.")).toBeInTheDocument();
  expect(mocks.previewPromptContext).toHaveBeenCalledWith(7, { kind: "diff", path: null });
  expect(onReview).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Send for review" }));
  await waitFor(() => expect(onReview).toHaveBeenCalledWith(preview));
});

it("offers tracked-file review after the working tree exceeds its limits", async () => {
  const preview: PromptContextView = {
    reference: { selector: { kind: "diff", path: "changed.txt" }, revision: "file-revision" },
    label: "changed.txt", text: "-old\n+new", omitted: false,
  };
  mocks.previewPromptContext
    .mockRejectedValueOnce({ message: "The working-tree diff exceeds the review limits. Choose a tracked file to review." })
    .mockResolvedValueOnce(preview);
  const { onReview } = renderSheet({ kind: "review" });
  expect(await screen.findByRole("alert")).toHaveTextContent("exceeds the review limits");
  expect(screen.queryByRole("button", { name: "Send for review" })).not.toBeInTheDocument();
  const path = screen.getByLabelText("Workspace-relative file, or leave empty for the working tree");
  fireEvent.change(path, { target: { value: "changed.txt" } });
  fireEvent.click(screen.getByRole("button", { name: "Preview" }));
  expect(await screen.findByText("changed.txt")).toBeInTheDocument();
  expect(mocks.previewPromptContext).toHaveBeenLastCalledWith(7, { kind: "diff", path: "changed.txt" });
  fireEvent.click(screen.getByRole("button", { name: "Send for review" }));
  await waitFor(() => expect(onReview).toHaveBeenCalledWith(preview));
});

it("requests automatic matches only for opted-in skills and suppresses them during a live run", async () => {
  const optedIn = { ...skill, trusted: true, auto_select: true };
  mocks.listPromptSkills.mockResolvedValue([optedIn]);
  const { result, rerender } = renderHook(
    ({ live }) => useComposerContext(7, "workspace", "Please audit this", live),
    { initialProps: { live: false } }
  );
  await waitFor(() => expect(result.current.references?.automatic_skills).toEqual([skill.reference]));
  expect(result.current.references?.skills).toEqual([]);
  expect(mocks.listPromptSkills).toHaveBeenCalledWith(7, "Please audit this");
  rerender({ live: true });
  expect(result.current.references).toBeUndefined();
  act(() => result.current.includeSkill(optedIn));
  expect(result.current.references?.skills).toEqual([skill.reference]);
  expect(result.current.references?.automatic_skills).toEqual([]);
});
