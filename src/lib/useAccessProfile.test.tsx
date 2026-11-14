import {
  act,
  fireEvent,
  render,
  renderHook,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useAccessProfile } from "./useAccessProfile";
import AccessProfileControl from "../components/AccessProfileControl";
import { LocaleProvider } from "./useLocale";
const { values, fail, invoke } = vi.hoisted(() => {
  const values = new Map<string, string | null>();
  const fail = { write: false, read: false };
  const invoke = vi.fn(
    async (name: string, args: { key: string; value?: string }) => {
      if (name === "get_setting") {
        if (fail.read) throw Error("read");
        return values.get(args.key) ?? null;
      }
      if (name === "set_setting") {
        if (fail.write) throw Error("write");
        values.set(args.key, args.value ?? null);
        return null;
      }
      throw Error(name);
    }
  );
  return { values, fail, invoke };
});
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
beforeEach(() => {
  values.clear();
  fail.read = false;
  fail.write = false;
  invoke.mockClear();
});
describe("shared access profile", () => {
  it.each([
    [null, "standard"],
    ["read_only", "read_only"],
    ["standard", "standard"],
    ["supervised", "supervised"],
    ["invalid", "read_only"],
  ] as const)("resolves %s as %s", async (raw, expected) => {
    values.set("agent.access_profile", raw);
    const { result } = renderHook(() => useAccessProfile());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.profile).toBe(expected);
  });
  it("fails closed on an unreadable preference", async () => {
    fail.read = true;
    const { result } = renderHook(() => useAccessProfile());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.profile).toBe("read_only");
  });
  it("keeps both selectors shared, persists keyboard changes, and rolls both back", async () => {
    function Harness() {
      const store = useAccessProfile();
      return (
        <LocaleProvider>
          <AccessProfileControl store={store} />
          <AccessProfileControl store={store} />
        </LocaleProvider>
      );
    }
    render(<Harness />);
    await waitFor(() =>
      expect(
        screen.getAllByRole("radio", { name: "Standard" })[0]
      ).toBeEnabled()
    );
    const groups = screen.getAllByRole("radiogroup", {
      name: "Access profile",
    });
    const selected = within(groups[0]).getByRole("radio", { name: "Standard" });
    selected.focus();
    fireEvent.keyDown(selected, { key: "ArrowRight" });
    await waitFor(() =>
      expect(values.get("agent.access_profile")).toBe("supervised")
    );
    for (const group of groups)
      expect(
        within(group).getByRole("radio", { name: "Supervised" })
      ).toHaveAttribute("aria-checked", "true");
    expect(
      within(groups[0]).getByRole("radio", { name: "Supervised" })
    ).toHaveFocus();
    fail.write = true;
    await act(async () => {
      fireEvent.click(
        within(groups[1]).getByRole("radio", { name: "Read only" })
      );
    });
    for (const group of groups)
      expect(
        within(group).getByRole("radio", { name: "Supervised" })
      ).toHaveAttribute("aria-checked", "true");
    expect(screen.getAllByRole("alert")).toHaveLength(2);
  });
});
