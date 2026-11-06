import { describe, expect, test } from 'vitest'
import { render, screen } from '@testing-library/react'
import '@testing-library/jest-dom/vitest'

import PrivacyPage from '@/app/privacy/page'

describe('/privacy', () => {
  test('renders the H1 and intro', () => {
    render(<PrivacyPage />)

    expect(screen.getByRole('heading', { name: 'Privacy' })).toBeInTheDocument()
    expect(
      screen.getByText(/Nexora collects nothing from you by default\./),
    ).toBeInTheDocument()
  })

  test('renders all key sections', () => {
    render(<PrivacyPage />)

    const headings = [
      'Privacy',
      'Collected data',
      'Data not collected',
      'Analytics',
      'Server logs',
      'Retention',
      'External services',
      'GitHub and download links',
      'No cookie banner',
    ]

    for (const heading of headings) {
      expect(screen.getByRole('heading', { name: heading })).toBeInTheDocument()
    }
  })

  test('renders privacy specifics', () => {
    render(<PrivacyPage />)

    expect(screen.getByText('No telemetry upload anywhere.')).toBeInTheDocument()
    expect(
      screen.getByText(/Analytics on this website is a disabled no-op adapter\./),
    ).toBeInTheDocument()
    expect(
      screen.getByText(
        'This website does not use tracking cookies, so no cookie banner is shown.',
      ),
    ).toBeInTheDocument()
  })
})
