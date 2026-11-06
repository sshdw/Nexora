import { test, expect } from '@playwright/test'
import { AxeBuilder } from '@axe-core/playwright'

test('homepage shell loads and shows navbar and footer', async ({ page }) => {
  await page.goto('/')

  await expect(page.locator('h1')).toContainText('Code with agents')

  // Links that must be visible on every viewport (navbar + footer).
  for (const name of ['Nexora', 'Download', 'Security', 'Docs', 'GitHub', 'Changelog']) {
    await expect(
      page.getByRole('link', { name }).filter({ visible: true }).first()
    ).toBeVisible()
  }

  await expect(page.getByText(/© 2026 Nexora/)).toBeVisible()
})

test('theme toggle switches dark mode', async ({ page }) => {
  await page.goto('/')

  await expect(page.locator('html')).not.toHaveClass('dark')

  await page.getByRole('button', { name: 'Dark' }).click()
  await expect(page.locator('html')).toHaveClass('dark')
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')

  await page.getByRole('button', { name: 'Light' }).click()
  await expect(page.locator('html')).not.toHaveClass('dark')
})

test('accessibility smoke test: no WCAG 2.2 AA violations', async ({ page }) => {
  await page.goto('/')

  const results = await new AxeBuilder({ page })
    .include('header, main, footer')
    .analyze()

  expect(results.violations).toEqual([])
})
