// Vitest global setup: jest-dom matchers (toBeInTheDocument, ...).
import "@testing-library/jest-dom/vitest";

// jsdom provides no `matchMedia`, but `Modal.tsx` reads it once at module
// scope for the dialog-exit timing — stub it globally (never matches, so
// tests observe the full timing path) before any component module loads.
if (
  typeof window !== "undefined" &&
  typeof window.matchMedia !== "function"
) {
  window.matchMedia = ((query: string): MediaQueryList => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}
