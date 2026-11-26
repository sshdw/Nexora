import {
  act,
  cleanup,
  fireEvent,
  render,
  renderHook,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
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
afterEach(cleanup);
beforeEach(() => {
  values.clear();
  fail.read = false;
  fail.write = false;
  invoke.mockClear();
});
describe("shared access profile", () => {
  it.each([
    [null, null, "standard"],
    ["read_only", null, "supervised"],
    ["standard", null, "standard"],
    ["supervised", null, "supervised"],
    ["full_access", null, "standard"],
    ["full_access", "false", "standard"],
    ["full_access", "true", "full_access"],
    ["invalid", null, "supervised"],
  ] as const)(
    "resolves profile=%s ack=%s as %s",
    async (raw, ack, expected) => {
      values.set("agent.access_profile", raw);
      if (ack !== null) values.set("agent.full_access_acknowledged", ack);
      const { result } = renderHook(() => useAccessProfile());
      await waitFor(() => expect(result.current.loading).toBe(false));
      expect(result.current.profile).toBe(expected);
    }
  );

  it("fails closed to supervised on an unreadable preference", async () => {
    fail.read = true;
    const { result } = renderHook(() => useAccessProfile());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.profile).toBe("supervised");
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
        screen.getAllByRole("radio", { name: "Auto-approve safe" })[0]
      ).toBeEnabled()
    );
    const groups = screen.getAllByRole("radiogroup", {
      name: "Access profile",
    });
    const selected = within(groups[0]).getByRole("radio", {
      name: "Auto-approve safe",
    });
    selected.focus();
    fireEvent.keyDown(selected, { key: "ArrowLeft" });
    await waitFor(() =>
      expect(values.get("agent.access_profile")).toBe("supervised")
    );
    for (const group of groups)
      expect(
        within(group).getByRole("radio", { name: "Ask permissions" })
      ).toHaveAttribute("aria-checked", "true");
    expect(
      within(groups[0]).getByRole("radio", { name: "Ask permissions" })
    ).toHaveFocus();
    fail.write = true;
    await act(async () => {
      fireEvent.click(
        within(groups[1]).getByRole("radio", { name: "Auto-approve safe" })
      );
    });
    for (const group of groups)
      expect(
        within(group).getByRole("radio", { name: "Ask permissions" })
      ).toHaveAttribute("aria-checked", "true");
    expect(screen.getAllByRole("alert")).toHaveLength(2);
  });

  it("opens explicit opt-in dialog before enabling Full access and persists acknowledgment on confirm", async () => {
    function Harness() {
      const store = useAccessProfile();
      return (
        <LocaleProvider>
          <AccessProfileControl store={store} variant="list" />
        </LocaleProvider>
      );
    }
    render(<Harness />);
    await waitFor(() =>
      expect(
        screen.getByRole("radio", { name: "Auto-approve safe" })
      ).toBeEnabled()
    );

    // Clicking Full access opens the confirmation dialog without persisting yet.
    fireEvent.click(screen.getByRole("radio", { name: "Full access" }));
    expect(
      screen.getByRole("dialog", { name: "Enable Full access?" })
    ).toBeInTheDocument();
    expect(values.get("agent.access_profile")).toBeUndefined();
    expect(values.get("agent.full_access_acknowledged")).toBeUndefined();

    // Cancelling closes the dialog and leaves mode unchanged.
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(
      screen.queryByRole("dialog", { name: "Enable Full access?" })
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("radio", { name: "Auto-approve safe" })
    ).toHaveAttribute("aria-checked", "true");

    // Re-opening and confirming persists acknowledgment first, then full_access.
    fireEvent.click(screen.getByRole("radio", { name: "Full access" }));
    fireEvent.click(screen.getByRole("button", { name: "Enable Full Access" }));
    await waitFor(() =>
      expect(values.get("agent.access_profile")).toBe("full_access")
    );
    expect(values.get("agent.full_access_acknowledged")).toBe("true");
    expect(
      screen.getByRole("radio", { name: "Full access" })
    ).toHaveAttribute("aria-checked", "true");
    expect(
      screen.queryByRole("dialog", { name: "Enable Full access?" })
    ).not.toBeInTheDocument();
  });
});
