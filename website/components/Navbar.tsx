'use client'

import Link from 'next/link'
import { useCallback, useState } from 'react'

import ThemeToggle from './ThemeToggle'

const GITHUB_URL = 'https://github.com/sshdw/Nexora'

const primaryCta = 'whitespace-nowrap rounded-md border border-transparent bg-primary-cta-bg text-primary-cta-text px-4 py-2 text-sm font-medium hover:bg-primary-cta-bg/90 transition-colors duration-200'

const ghostLink = 'text-sm font-medium text-muted hover:text-accent transition-colors duration-200'

export default function Navbar() {
  const [menuOpen, setMenuOpen] = useState(false)

  const toggleMenu = useCallback(() => setMenuOpen((open) => !open), [])

  const closeMenu = useCallback(() => setMenuOpen(false), [])

  return (
    <header className="sticky top-0 z-40 h-[58px] shrink-0 bg-canvas border-b border-border">
      <div className="mx-auto max-w-[1240px] px-5 md:px-6 lg:px-8 flex h-full items-center justify-between gap-3">
        <Link href="/" className="flex items-center gap-2 font-semibold text-text">
          Nexora
        </Link>

        <nav className="hidden md:flex items-center gap-6" aria-label="Primary">
          <Link href="/" className={ghostLink}>
            Product
          </Link>
          <Link href="/security" className={ghostLink}>
            Security
          </Link>
          <Link href="/" className={ghostLink}>
            Open Source
          </Link>
          <Link href="/docs" className={ghostLink}>
            Docs
          </Link>
        </nav>

        <div className="hidden md:flex items-center gap-3">
          <a
            href={GITHUB_URL}
            target="_blank"
            rel="noopener"
            className={ghostLink}
          >
            GitHub
          </a>
          <Link href="/download" className={primaryCta}>
            Download
          </Link>
        </div>

        <ThemeToggle />

        <button
          type="button"
          className="md:hidden flex items-center gap-2 p-2 text-muted hover:text-text"
          aria-expanded={menuOpen}
          aria-controls="mobile-menu"
          onClick={toggleMenu}
        >
          <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <path d="M4 6h12M4 10h12M4 14h12" />
          </svg>
          <span className="text-xs font-medium">Menu</span>
        </button>

        {menuOpen && (
          <div
            id="mobile-menu"
            className="md:hidden fixed inset-0 z-50 bg-canvas flex flex-col gap-4 p-6 text-sm"
          >
            <nav className="flex flex-col gap-3" onClick={closeMenu}>
              <Link href="/" className={ghostLink} onClick={closeMenu}>
                Product
              </Link>
              <Link href="/security" className={ghostLink} onClick={closeMenu}>
                Security
              </Link>
              <Link href="/" className={ghostLink} onClick={closeMenu}>
                Open Source
              </Link>
              <Link href="/docs" className={ghostLink} onClick={closeMenu}>
                Docs
              </Link>
            </nav>
            <div className="mt-auto flex flex-col gap-3 pt-6 border-t border-border">
              <a
                href={GITHUB_URL}
                target="_blank"
                rel="noopener"
                className={ghostLink}
                onClick={closeMenu}
              >
                GitHub
              </a>
              <Link href="/download" className={primaryCta} onClick={closeMenu}>
                Download
              </Link>
            </div>
          </div>
        )}
      </div>
    </header>
  )
}
