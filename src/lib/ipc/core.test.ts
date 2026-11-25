import { describe, expect, it } from "vitest";
import { formatIpcError } from "./core";

describe("formatIpcError", () => {
  it("extracts message from Tauri CommandError plain object", () => {
    expect(formatIpcError({ kind: "request", message: "Provider timed out" })).toBe(
      "Provider timed out"
    );
  });

  it("extracts message from JS Error", () => {
    expect(formatIpcError(new Error("Network down"))).toBe("Network down");
  });

  it("returns non-empty string errors directly", () => {
    expect(formatIpcError("Rate limited")).toBe("Rate limited");
  });

  it("never returns [object Object] for plain objects without message", () => {
    expect(formatIpcError({})).toBe("An unexpected error occurred");
    expect(formatIpcError({ code: 500 }, "Fallback")).toBe("Fallback");
    expect(formatIpcError({ message: "   " }, "Fallback")).toBe("Fallback");
  });

  it("returns fallback for null, undefined, and primitives", () => {
    expect(formatIpcError(null, "Fallback")).toBe("Fallback");
    expect(formatIpcError(undefined, "Fallback")).toBe("Fallback");
    expect(formatIpcError(42, "Fallback")).toBe("Fallback");
  });
});
