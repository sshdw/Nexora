# Nexora

**A local-first AI desktop workspace — multi-provider chat and supervised agent runs, with everything you write stored on your own machine.**

[![CI](https://github.com/sshdw/Nexora/actions/workflows/ci.yml/badge.svg)](https://github.com/sshdw/Nexora/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/sshdw/Nexora)](https://github.com/sshdw/Nexora/releases/latest)
[![License](https://img.shields.io/github/license/sshdw/Nexora)](LICENSE)

Nexora is a desktop app for people who work with AI all day and do not want their work living on someone else's server. It runs on Tauri v2 with a React 19 frontend and a Rust backend; every conversation, prompt, attachment, and setting lives in a single SQLite file on your disk. There are no accounts, no sync service, and no telemetry — the network is touched only when you send a request to a provider you configured yourself.

Latest release: **v1.5.0** — see [releases](https://github.com/sshdw/Nexora/releases/latest) and the [changelog](CHANGELOG.md). Version 2.0 (visual rebuild) is in development on `main`.

---

## Why local-first

- **Your data is one file you own.** Conversations, prompts, and settings live in a single portable SQLite database (`nexora.db`) in your per-user application-data directory. Copy it, back it up, delete it.
- **It opens and works offline.** Browsing history, searching, editing prompts, and changing settings need no connection. Startup never blocks on the network.
- **API keys never touch your database.** Credentials live in the operating system keyring. They are never written to SQLite, logs, or source, and errors surfaced in the UI carry no secret material.
- **No accounts, no workspaces, no background sync.** Nothing is uploaded until you press send.

## Features

**Chat that keeps its history**

- Unlimited conversations — rename, archive, restore, or permanently delete.
- Full history persists across restarts in chronological order; a failed request shows an error instead of corrupting what came before.
- Offline full-text search across conversation titles, message content, and prompts (SQLite FTS5).
- Export a conversation to JSON and import it back; malformed files are rejected and rolled back atomically.

**Agent runs with real controls**

- One agent run per dialog, streamed as explicit steps you can follow.
- Tools: file read/write, command execution, directory listing.
- Three autonomy levels — `supervised`, `semi_autonomous` (default), `full_autonomous` — plus approval gates that park anything needing a human decision.
- Run controls: pause, resume, cancel, extend the iteration budget, and a per-run spend guard in micro-USD.
- Six agent roles on a plan → act → review pipeline, with run snapshots, checkpoints and rollback, an append-only audit trail, a self-audit gate, a read-only run inspector, a cost dashboard, and a task manager for autonomous mode.

**Providers and models**

- OpenAI, Anthropic, Google Gemini, xKiro, OpenRouter, NVIDIA NIM, and OpenCode Zen, each with a maintained shortlist of supported models — or point Nexora at your own OpenAI-compatible endpoint.
- Choose the provider and model per request; Settings accepts a custom model ID beyond the curated shortlist.
- Provider failures are classified, not swallowed: rate limit (with a Retry-After hint), outage/5xx, network/timeout, rejected credential, invalid request, unexpected response — the same taxonomy for chat and for agent runs.
- A local-only provider health probe (healthy / degraded / unreachable / unknown).

**Prompts and documents**

- A reusable prompt library you can create, edit, delete, and insert into any conversation.
- Attach local files to a request, review them before sending, remove them beforehand.

**Workspace and interface**

- Token-driven component library and app shell: a full token set, reusable primitives, tabs, split view, and a distraction-free zen mode; a Ctrl+K command palette; a hotkeys reference.
- Integrated git (status, log, diff, timeline, explain-diff, risk signals, stage/commit/push) and a terminal panel with error intelligence for failed commands.
- Activity feed, health view, cost dashboard, and simplified onboarding.
- English and Russian, switchable in-app.

## Providers

| Provider | Endpoint | Notes |
|---|---|---|
| OpenAI | native | Curated model shortlist |
| Anthropic | native | Curated model shortlist |
| Google Gemini | native | Live-verified context windows, tool-capability gated |
| xKiro | OpenAI-compatible | Curated model shortlist |
| OpenRouter | OpenAI-compatible | Curated model shortlist |
| NVIDIA NIM | OpenAI-compatible | Curated model shortlist |
| OpenCode Zen | OpenAI-compatible | Curated model shortlist |
| Custom | OpenAI-compatible | Model ID comes from your endpoint configuration |

Supported models live in the backend (`src-tauri/src/infrastructure/providers/`).

## Download

**Prebuilt installers** — [github.com/sshdw/Nexora/releases/latest](https://github.com/sshdw/Nexora/releases/latest).
The latest release ships Windows installers (NSIS `*-setup.exe` plus MSI); other platforms — build from source.

**Build from source**

Prerequisites: a stable Rust toolchain (pinned by `src-tauri/rust-toolchain.toml`), Node.js and npm, and the [Tauri v2 system dependencies](https://tauri.app/start/prerequisites/) for your platform.

```bash
npm install
npm run tauri dev      # dev build; Vite serves on port 1420 (strict port)
```

```bash
npm run tauri build    # production bundle
```

## Tech stack

| Layer | Technology |
|---|---|
| Desktop shell | Tauri v2 (`com.nexora.app`) |
| Frontend | React 19, TypeScript (strict), Vite 6, Tailwind CSS 4 |
| UI system | Token-driven components, Radix primitives, GSAP motion |
| Backend | Rust (2021), `unsafe_code = "forbid"`, clippy `pedantic` clean |
| IPC | Tauri commands — thin translation only, business rules live in services |
| Storage | SQLite via `rusqlite` (bundled, FTS5), forward-only migrations |
| Credentials | OS keyring (`keyring` crate), resolved in the backend |
| Integrations | `reqwest` (rustls) for providers, `git2` for the git panel |

## Docs

Approved specification documents live in [`docs/`](docs/):

- [SRS.md](docs/SRS.md) — requirements specification
- [ARCHITECTURE.md](docs/ARCHITECTURE.md) — layered technical architecture
- [DATABASE.md](docs/DATABASE.md) — schema and migration strategy
- [CONTEXT-WINDOWS.md](docs/CONTEXT-WINDOWS.md) — enforced context windows, compaction, capability fallback
- [NEXORA-DESIGN-BRIEF-2.0-FINAL.md](docs/NEXORA-DESIGN-BRIEF-2.0-FINAL.md) — desktop design direction
- [NEXORA-CLI-DESIGN-BRIEF.md](docs/NEXORA-CLI-DESIGN-BRIEF.md) — CLI design brief (2.1)
- [ROADMAP.md](docs/ROADMAP.md) — historical MVP phase record
- [AGENT-E2E.md](docs/AGENT-E2E.md) — manual UI checklist

And the project history:

- [CHANGELOG.md](CHANGELOG.md) — release history

## Roadmap

- **2.0 — visual rebuild: in progress on `main`.** Tracked in the [2.0 milestone](https://github.com/sshdw/Nexora/milestone/1).
- **2.1 — CLI: next.** A command-line companion in the same repository. Tracked in the [2.1 milestone](https://github.com/sshdw/Nexora/milestone/2); design brief in [docs/NEXORA-CLI-DESIGN-BRIEF.md](docs/NEXORA-CLI-DESIGN-BRIEF.md).

Out of scope by design: cloud sync, user accounts, collaboration, mobile/web clients, and a plugin system.

## Contributing

Issues and pull requests are welcome. Before opening a PR, run the gates in this order:

```bash
npm test                # frontend tests (vitest)
npx tsc                 # frontend typecheck
npx eslint .            # frontend lint
cd src-tauri
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test              # backend tests, in-memory SQLite
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full workflow.

## Security

Please report vulnerabilities privately via **GitHub → Security → Report a vulnerability** rather than in a public issue — see [SECURITY.md](SECURITY.md).

Security properties of the app itself: `unsafe_code = "forbid"` in the Rust backend; a restrictive content-security policy on the webview; API keys confined to the OS keyring; and IPC errors that are scrubbed of credentials, SQL, and message content.

## License

[MIT](LICENSE) © 2026 sshdw
