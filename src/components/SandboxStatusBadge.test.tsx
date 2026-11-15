import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { getSandboxStatus, type SandboxStatus } from "../lib/tauri";
import SandboxStatusBadge from "./SandboxStatusBadge";

vi.mock("../lib/tauri", () => ({ getSandboxStatus: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });
const verified: SandboxStatus = { workspace_root: "/fixture/a", state: "verified", posture: "Verified", mechanism: "native-test", reason: null };

it("announces verified server posture with the actual shell limitation", async () => {
  vi.mocked(getSandboxStatus).mockResolvedValue(verified);
  render(<SandboxStatusBadge workspaceRoot="/fixture/a" locale="en" />);
  await waitFor(() => expect(screen.getByRole("status").textContent).toContain("Isolation verified"));
  expect(screen.getByRole("status").textContent).toContain("creating, deleting or renaming");
});

it("shows fallback when the server status is unavailable", async () => {
  vi.mocked(getSandboxStatus).mockRejectedValue(new Error("unavailable"));
  render(<SandboxStatusBadge workspaceRoot="/fixture/a" locale="ru" />);
  await waitFor(() => expect(screen.getByRole("status").textContent).toContain("Согласие на каждую команду"));
  expect(screen.getByRole("status").textContent).toContain("недоступен");
});

it("cannot show a previous workspace response as verified after a switch", async () => {
  let finish: ((value: SandboxStatus) => void) | undefined;
  vi.mocked(getSandboxStatus).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; })).mockResolvedValue(verified);
  const view = render(<SandboxStatusBadge workspaceRoot="/fixture/a" locale="en" />);
  view.rerender(<SandboxStatusBadge workspaceRoot="/fixture/b" locale="en" />);
  await waitFor(() => expect(screen.getByRole("status").textContent).toContain("Per-command consent"));
  finish?.(verified);
  await waitFor(() => expect(screen.getByRole("status").textContent).not.toContain("Isolation verified"));
});

it("matches Windows workspace paths including verbatim prefix and trailing separator", async () => {
  vi.mocked(getSandboxStatus).mockResolvedValue({
    ...verified,
    workspace_root: "\\\\?\\D:\\projects\\nx",
  });
  const view = render(<SandboxStatusBadge workspaceRoot={"D:\\projects\\nx\\"} locale="en" />);
  await waitFor(() => expect(screen.getByRole("status").textContent).toContain("Isolation verified"));

  vi.mocked(getSandboxStatus).mockResolvedValue({
    ...verified,
    workspace_root: "D:\\projects\\other",
  });
  view.rerender(<SandboxStatusBadge workspaceRoot={"D:\\projects\\nx"} locale="en" />);
  await waitFor(() => expect(screen.getByRole("status").textContent).toContain("Per-command consent"));
});
