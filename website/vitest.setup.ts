import { vi } from 'vitest'

import '@testing-library/jest-dom/vitest'

// jsdom does not implement matchMedia; provide a minimal stub for theme logic.
Object.defineProperty(window, 'matchMedia', {
  writable: true,
  value: vi.fn((query: string) => ({
    matches: query === '(prefers-color-scheme: dark)',
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  })),
})
