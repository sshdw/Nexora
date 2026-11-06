import { describe, expect, test } from 'vitest'
import { render, screen } from '@testing-library/react'
import '@testing-library/jest-dom/vitest'

import OsDetection, { detectOs, downloadLabel } from '@/components/OsDetection'

describe('detectOs', () => {
  test('detects Windows', () => {
    expect(
      detectOs('Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36'),
    ).toBe('windows')
  })

  test('detects macOS', () => {
    expect(
      detectOs(
        'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15',
      ),
    ).toBe('macos')
  })

  test('detects Linux but not Android', () => {
    expect(detectOs('Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36')).toBe(
      'linux',
    )
    expect(
      detectOs('Mozilla/5.0 (Linux; Android 10; Pixel 4) AppleWebKit/537.36'),
    ).toBe('unknown')
  })

  test('falls back to unknown', () => {
    expect(detectOs('Mozilla/5.0 (generic)')).toBe('unknown')
  })
})

describe('downloadLabel', () => {
  test('returns the right label per platform', () => {
    expect(downloadLabel('windows')).toBe('Download for Windows')
    expect(downloadLabel('macos')).toBe('Download for macOS')
    expect(downloadLabel('linux')).toBe('Build from source')
    expect(downloadLabel('unknown')).toBe('Download Nexora')
  })
})

describe('OsDetection', () => {
  test('detects a valid platform after mount', () => {
    render(
      <OsDetection>
        {(platform) => <span data-testid="platform">{platform}</span>}
      </OsDetection>,
    )

    const platform = screen.getByTestId('platform').textContent
    expect(['windows', 'macos', 'linux', 'unknown']).toContain(platform)
  })

  test('detects the platform after mount', () => {
    render(
      <OsDetection>
        {(platform) => <span data-testid="platform">{platform}</span>}
      </OsDetection>,
    )

    const platform = screen.getByTestId('platform').textContent
    expect(['windows', 'macos', 'linux', 'unknown']).toContain(platform)
  })
})
