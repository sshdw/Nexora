import { describe, expect, test, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, fireEvent, act } from '@testing-library/react'
import '@testing-library/jest-dom/vitest'

import HeroDemo from '@/components/HeroDemo'

const setMatchMedia = (matches: boolean) => {
  Object.defineProperty(window, 'matchMedia', {
    writable: true,
    value: vi.fn((query: string) => ({
      matches: query === '(prefers-reduced-motion: reduce)' ? matches : false,
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    })),
  })
}

describe('HeroDemo', () => {
  beforeEach(() => {
    setMatchMedia(false)
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  test('renders the quiet idle state with project, prompt, files, and command', () => {
    render(<HeroDemo />)

    expect(screen.getByText('example-app')).toBeInTheDocument()
    expect(screen.getByText('Fix the failing authentication test.')).toBeInTheDocument()
    expect(screen.getByText('src/auth.ts')).toBeInTheDocument()
    expect(screen.getByText('src/auth.test.ts')).toBeInTheDocument()
    expect(screen.getByText('npm test')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Allow' })).not.toBeInTheDocument()
    expect(screen.queryByText('Tests passed · 2 files changed')).not.toBeInTheDocument()
  })

  test('clicking the Approval spine step shows the Allow button', () => {
    render(<HeroDemo />)

    const approval = screen.getByRole('button', { name: /Approval, pending/ })
    fireEvent.click(approval)

    expect(screen.getByRole('button', { name: 'Allow' })).toBeInTheDocument()
    expect(screen.getByText('Allow npm test to run?')).toBeInTheDocument()
  })

  test('clicking Allow advances the walkthrough to editing', () => {
    render(<HeroDemo />)

    fireEvent.click(screen.getByRole('button', { name: /Approval, pending/ }))
    fireEvent.click(screen.getByRole('button', { name: 'Allow' }))

    expect(screen.getByText('Agent edits two files.')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Allow' })).not.toBeInTheDocument()
  })

  test('clicking Run tests reaches the passed result', () => {
    vi.useFakeTimers()
    render(<HeroDemo />)

    fireEvent.click(screen.getByRole('button', { name: /Review, pending/ }))
    expect(screen.getByText('Verification: npm test.')).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: 'Run tests' }))
    act(() => {
      vi.advanceTimersByTime(601)
    })
    expect(screen.getByText('Tests passed · 2 files changed')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Replay' })).toBeInTheDocument()
  })

  test('Replay resets the walkthrough to idle', () => {
    vi.useFakeTimers()
    render(<HeroDemo />)

    fireEvent.click(screen.getByRole('button', { name: /Approval, pending/ }))
    fireEvent.click(screen.getByRole('button', { name: 'Allow' }))
    act(() => { vi.advanceTimersByTime(351) })
    expect(screen.getByText('Diff ready. Review the change.')).toBeInTheDocument()
    act(() => { vi.advanceTimersByTime(351) })
    expect(screen.getByText('Verification: npm test.')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Run tests' }))
    act(() => { vi.advanceTimersByTime(601) })
    expect(screen.getByText('Tests passed · 2 files changed')).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: 'Replay' }))
    expect(screen.getByText('Agent is idle.')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Allow' })).not.toBeInTheDocument()
    expect(screen.queryByText('Tests passed · 2 files changed')).not.toBeInTheDocument()
  })

  const advanceToAwaitingApproval = () => {
    act(() => { vi.advanceTimersByTime(800) })
    expect(screen.getByText('Agent reads the project files.')).toBeInTheDocument()
    act(() => { vi.advanceTimersByTime(350) })
    expect(screen.getByText('Tool call: read_file(src/auth.ts)')).toBeInTheDocument()
    act(() => { vi.advanceTimersByTime(350) })
    expect(screen.getByText('Issue found.')).toBeInTheDocument()
    act(() => { vi.advanceTimersByTime(350) })
    expect(screen.getByText('Command request: npm test.')).toBeInTheDocument()
    act(() => { vi.advanceTimersByTime(350) })
    expect(screen.getByRole('button', { name: 'Allow' })).toBeInTheDocument()
  }

  test('autoplay plays the walkthrough once with fake timers', () => {
    vi.useFakeTimers()
    render(<HeroDemo />)

    advanceToAwaitingApproval()

    // gated state: autoplay simulates Allow after GATED_DELAY
    act(() => { vi.advanceTimersByTime(1001) })
    expect(screen.getByText('Agent edits two files.')).toBeInTheDocument()

    act(() => { vi.advanceTimersByTime(351) })
    expect(screen.getByText('Diff ready. Review the change.')).toBeInTheDocument()
    act(() => { vi.advanceTimersByTime(351) })
    expect(screen.getByText('Verification: npm test.')).toBeInTheDocument()

    act(() => { vi.advanceTimersByTime(1001) })
    expect(screen.getByText('Tests passed · 2 files changed')).toBeInTheDocument()
    act(() => { vi.advanceTimersByTime(601) })
    expect(screen.getByRole('button', { name: 'Replay' })).toBeInTheDocument()
  })

  test('user interaction switches to manual mode: gated states wait for the user', () => {
    vi.useFakeTimers()
    render(<HeroDemo />)

    advanceToAwaitingApproval()

    fireEvent.click(screen.getByRole('button', { name: 'Allow' }))
    expect(screen.getByText('Agent edits two files.')).toBeInTheDocument()

    act(() => { vi.advanceTimersByTime(351) })
    expect(screen.getByText('Diff ready. Review the change.')).toBeInTheDocument()
    act(() => { vi.advanceTimersByTime(351) })
    expect(screen.getByText('Verification: npm test.')).toBeInTheDocument()
    // Manual mode: verification waits for Run tests, no autopilot.
    expect(screen.getByRole('button', { name: 'Run tests' })).toBeInTheDocument()
    expect(screen.queryByText('Running tests…')).not.toBeInTheDocument()
    act(() => { vi.advanceTimersByTime(1001) })
    expect(screen.getByText('Verification: npm test.')).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: 'Run tests' }))
    act(() => { vi.advanceTimersByTime(601) })
    expect(screen.getByText('Tests passed · 2 files changed')).toBeInTheDocument()
  })

  test('prefers-reduced-motion shows a completed static state without autoplay', () => {
    vi.useFakeTimers()
    setMatchMedia(true)
    render(<HeroDemo />)

    expect(screen.getByText('Tests passed · 2 files changed')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Replay' })).toBeInTheDocument()
    expect(screen.queryByText('Agent is idle.')).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Allow' })).not.toBeInTheDocument()

    // No timers scheduled: advancing time changes nothing.
    act(() => {
      vi.advanceTimersByTime(10_000)
    })
    expect(screen.getByText('Tests passed · 2 files changed')).toBeInTheDocument()
  })
})
