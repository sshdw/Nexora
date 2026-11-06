'use client'

import { useCallback, useEffect, useState } from 'react'

import type { Theme } from '@/lib/theme'
import { applyTheme, getSavedTheme, resolveTheme, saveTheme } from '@/lib/theme'

function prefersDark(): boolean {
  return typeof window !== 'undefined' && window.matchMedia('(prefers-color-scheme: dark)').matches
}

function toggleClassName(active: boolean): string {
  return (
    'relative inline-flex items-center justify-center whitespace-nowrap rounded-md border border-border px-3 py-1.5 text-xs font-medium transition-colors duration-200 ' +
    (active
      ? 'bg-primary-cta-bg text-primary-cta-text hover:bg-primary-cta-bg/90'
      : 'bg-surface text-muted hover:text-accent hover:border-accent')
  )
}

export default function ThemeToggle() {
  const [mode, setMode] = useState<'light' | 'dark'>('light')
  const [preference, setPreference] = useState<Theme>('system')
  const [mounted, setMounted] = useState(false)

  const sync = useCallback(() => {
    const saved = getSavedTheme()
    const resolved = resolveTheme(saved, prefersDark())
    applyTheme(resolved)
    setMode(resolved)
    setPreference(saved ?? 'system')
  }, [])

  useEffect(() => {
    setMounted(true)
    sync()

    const media = window.matchMedia('(prefers-color-scheme: dark)')
    const handler = () => sync()
    media.addEventListener('change', handler)
    window.addEventListener('storage', handler)
    return () => {
      media.removeEventListener('change', handler)
      window.removeEventListener('storage', handler)
    }
  }, [sync])

  const setTheme = useCallback(
    (value: Theme) => {
      saveTheme(value)
      const resolved = resolveTheme(value, prefersDark())
      applyTheme(resolved)
      setMode(resolved)
      setPreference(value)
    },
    []
  )

  if (!mounted) {
    return null
  }

  const isSystem = preference === 'system'

  return (
    <div className="flex items-center gap-1" role="group" aria-label="Appearance">
      <button
        type="button"
        aria-pressed={!isSystem && mode === 'light'}
        data-active={!isSystem && mode === 'light' ? 'true' : undefined}
        onClick={() => setTheme('light')}
        className={toggleClassName(!isSystem && mode === 'light')}
      >
        Light
      </button>
      <button
        type="button"
        aria-pressed={!isSystem && mode === 'dark'}
        data-active={!isSystem && mode === 'dark' ? 'true' : undefined}
        onClick={() => setTheme('dark')}
        className={toggleClassName(!isSystem && mode === 'dark')}
      >
        Dark
      </button>
      <button
        type="button"
        aria-pressed={isSystem}
        data-active={isSystem ? 'true' : undefined}
        onClick={() => setTheme('system')}
        className={toggleClassName(isSystem)}
      >
        System
      </button>
    </div>
  )
}
