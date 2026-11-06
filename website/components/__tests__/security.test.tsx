import { describe, expect, test } from 'vitest'
import { render, screen } from '@testing-library/react'
import '@testing-library/jest-dom/vitest'

import SecurityPage from '@/app/security/page'

describe('/security', () => {
  test('renders the H1 and intro', () => {
    render(<SecurityPage />)

    expect(screen.getByRole('heading', { name: 'Security model' })).toBeInTheDocument()
    expect(
      screen.getByText(
        /Your workspace stays local\. Requests go to the providers you choose\./,
      ),
    ).toBeInTheDocument()
  })

  test('renders all required sections in order', () => {
    render(<SecurityPage />)

    const headings = [
      'Security model',
      'Technical diagram',
      'Security overview',
      'What stays local',
      'What leaves your device',
      'Credentials',
      'Agent permissions & approvals',
      'Shell execution',
      'Prompt injection / untrusted content',
      'Audit & rollback',
      'Application hardening',
      'Telemetry & diagnostics',
      'Known limitations',
      'Threat model',
      'Reporting vulnerabilities',
    ]

    for (const heading of headings) {
      expect(screen.getByRole('heading', { name: heading })).toBeInTheDocument()
    }
  })

  test('renders the technical diagram', () => {
    render(<SecurityPage />)

    expect(screen.getByText('Workspace')).toBeInTheDocument()
    expect(screen.getByText('local SQLite')).toBeInTheDocument()
    expect(screen.getAllByText('Credentials')).toHaveLength(2)
    expect(screen.getByText('OS keyring')).toBeInTheDocument()
    expect(screen.getByText('Requests')).toBeInTheDocument()
    expect(screen.getByText('provider you choose')).toBeInTheDocument()
  })

  test('renders all known limitations', () => {
    render(<SecurityPage />)

    expect(screen.getByText('Shell execution is not a sandbox.')).toBeInTheDocument()
    expect(
      screen.getByText("Nexora can't guarantee the security or privacy of third-party providers."),
    ).toBeInTheDocument()
    expect(
      screen.getByText(
        'Open source does not equal an independent security audit.',
      ),
    ).toBeInTheDocument()
  })
})
