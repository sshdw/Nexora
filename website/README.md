# Nexora website — Phase 1 foundation

Static marketing website for Nexora. Lives under `/website` of the `sshdw/Nexora` repository; the desktop app, Rust backend, and provider code are untouched.

## Stack

- Next.js 15 — App Router + TypeScript, static-first (`output: 'export'`)
- Tailwind CSS v4 (`@tailwindcss/postcss`) with CSS custom property design tokens (light + dark)
- Geist + Geist Mono, self-hosted via `@fontsource-variable/geist` and `@fontsource-variable/geist-mono`
- Vitest + `@testing-library/react` for unit tests
- Playwright + `@axe-core/playwright` for browser and accessibility smoke tests in CI

## Node

Node >= 26 (Active LTS). See `.nvmrc` and `engines` in `package.json`. CI runs on Node 26.

## Commands (run inside `/website`)

```
npm ci            install dependencies
npm run dev       dev server (http://localhost:3000)
npm run build     static production build → out/
npm run start     serve the exported build locally
npm run typecheck tsc --noEmit
npm run lint      eslint
npm test          vitest run
```

## Files created (Phase 1)

- `package.json`, `.nvmrc`, `tsconfig.json`, `next.config.js`, `postcss.config.mjs`, `tailwind.css`, `next-env.d.ts` — tooling and config
- `app/layout.tsx`, `app/page.tsx`, `app/globals.css` — App Router shell, metadata, design tokens
- `components/Navbar.tsx`, `components/Footer.tsx`, `components/ThemeToggle.tsx` — sticky navbar, footer, light/dark/system theme toggle
- `lib/theme.ts` — theme persistence (localStorage), system preference, return-to-system
- `components/__tests__/shell.test.tsx` — shell render smoke test
- `vitest.config.ts`, `vitest.setup.ts`, `eslint.config.mjs`, `playwright.config.ts`, `tests/e2e-smoke.spec.ts` — test setup
- `public/robots.txt`, `public/sitemap.xml`, `public/favicon.svg` — metadata infrastructure
- `.github/workflows/website-ci.yml` — CI: npm ci, lint, typecheck, unit tests, production build, browser smoke, accessibility smoke
- `README.md` — this file

## Notes

- No marketing copy beyond placeholder navbar/footer text (homepage copy and hero are Phase 2).
- Shadcn/generic SaaS component kits are not used; only small custom primitives.
- Dark theme uses the Tailwind `dark:` variant with CSS custom properties as the token source.