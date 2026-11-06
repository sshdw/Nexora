import { describe, expect, test } from 'vitest'
import { render, screen } from '@testing-library/react'
import '@testing-library/jest-dom/vitest'

import Navbar from '@/components/Navbar'
import Footer from '@/components/Footer'
import ThemeToggle from '@/components/ThemeToggle'

describe('shell', () => {
  test('renders navbar and footer with primary navigation', () => {
    render(
      <>
        <Navbar />
        <Footer />
      </>
    )

    expect(screen.getAllByRole('link', { name: 'Nexora' })).toHaveLength(2)
    expect(screen.getAllByRole('link', { name: 'Product' })).toHaveLength(2)
    expect(screen.getAllByRole('link', { name: 'Security' })).toHaveLength(2)
    expect(screen.getByRole('link', { name: 'Open Source' })).toBeInTheDocument()
    expect(screen.getAllByRole('link', { name: 'Docs' })).toHaveLength(2)
    expect(screen.getAllByRole('link', { name: 'Download' })).toHaveLength(2)
    expect(screen.getByRole('link', { name: 'Changelog' })).toBeInTheDocument()

    const github = screen.getAllByRole('link', { name: /^GitHub/ })
    expect(github.length).toBeGreaterThanOrEqual(1)
    expect(github[0]).toHaveAttribute('target', '_blank')

    expect(screen.getByText(/MIT License/)).toBeInTheDocument()
    expect(screen.getByText(/© 2026 Nexora/)).toBeInTheDocument()
  })

  test('renders a theme toggle that switches the dark class', () => {
    render(<ThemeToggle />)

    expect(screen.getByRole('button', { name: 'Light' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Dark' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'System' })).toBeInTheDocument()

    const darkButton = screen.getByRole('button', { name: 'Dark' })
    darkButton.click()

    expect(document.documentElement).toHaveClass('dark')
    expect(document.documentElement).toHaveAttribute('data-theme', 'dark')

    const lightButton = screen.getByRole('button', { name: 'Light' })
    lightButton.click()

    expect(document.documentElement).not.toHaveClass('dark')
    expect(document.documentElement).not.toHaveAttribute('data-theme', 'dark')
  })
})
