export type Theme = 'light' | 'dark' | 'system'

export const STORAGE_KEY = 'nexora-theme'

export function getSavedTheme(): Theme | null {
  if (typeof window === 'undefined' || typeof localStorage === 'undefined') {
    return null
  }
  const value = localStorage.getItem(STORAGE_KEY)
  if (value === 'light' || value === 'dark' || value === 'system') {
    return value
  }
  return null
}

export function saveTheme(value: Theme): void {
  if (typeof localStorage === 'undefined') {
    return
  }
  localStorage.setItem(STORAGE_KEY, value)
}

export function resolveTheme(saved: Theme | null, prefersDark: boolean): 'light' | 'dark' {
  if (saved === null || saved === 'system') {
    return prefersDark ? 'dark' : 'light'
  }
  return saved
}

export function applyTheme(mode: 'light' | 'dark'): void {
  if (typeof document === 'undefined') {
    return
  }
  const html = document.documentElement
  html.dataset.theme = mode
  if (mode === 'dark') {
    html.classList.add('dark')
  } else {
    html.classList.remove('dark')
  }
}
