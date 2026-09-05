# Nexora

Nexora is a local-first AI desktop application for working with multiple AI providers while keeping all user data in a single local SQLite database. Nexora is a desktop AI agent (one agent run per dialog) on Tauri v2. Conversations, prompts, attachments, and settings live entirely on your machine: there are no accounts and no cloud synchronization, and the network is contacted only when you send a request to an AI provider.

## Features

Nexora implements the approved MVP scope — functional requirements FR-001 through FR-015 of the [SRS](docs/SRS.md) — plus the shipped agent stack:

- **AI conversations** — create unlimited conversations, exchange messages with an AI, rename, archive/restore, and permanently delete them (FR-002, FR-006).
- **Persistent history** — complete conversation history survives restarts and remains in chronological order; failed requests show an error instead of corrupting history (FR-003, FR-005).
- **Multiple AI providers** — connect OpenAI, Anthropic, Google Gemini, xKiro, OpenRouter, NVIDIA NIM, or OpenCode Zen (the last four as OpenAI-compatible endpoints), each with a maintained list of supported models; select provider and model before sending a request (FR-004).
- **Prompt library** — create, edit, and delete reusable prompts and insert them into any conversation (FR-007).
- **Document attachments** — attach local files to a request, review them before sending, and remove them beforehand (FR-008).
- **Local search** — offline full-text search across conversation titles, message content, and prompts, powered by SQLite FTS5 (FR-009).
- **Import & export** — export individual conversations to JSON and import them back; invalid files are rejected and roll back atomically (FR-010, FR-011).
- **Settings** — persisted appearance (dark/light theme) and default provider/model selection; invalid values are rejected (FR-012).
- **Data management** — permanently remove conversations or prompts, or clear all application data behind an explicit typed confirmation (FR-013).
- **Credential management** — add, update, and remove provider API keys; missing credentials are detected before a request is sent (FR-014).
- **Offline access** — browsing history, searching, editing prompts, and changing settings all work without internet access; startup never requires a connection (FR-001, FR-015).
- **Agent runs with streaming steps** — one run per dialog.
- **Tools** — file read/write, command execution, directory listing.
- **Autonomy modes** — `supervised` / `semi_autonomous` (default) / `full_autonomous`; approval parking (parked approvals never auto-resolved).
- **Run controls** — pause / resume / cancel; iteration budget with extend; financial spend guard (per-run micro-USD budget).
- **Terminal and diff viewers**.
- **Classified provider errors (v1.0.2)** — rate limit with Retry-After hint, outage/5xx, network/timeout, rejected credential, invalid request, unexpected response — agent runs and chat alike.

## Tech Stack

| Layer | Technology |
|---|---|
| Desktop framework | Tauri v2 |
| Frontend | React 19, TypeScript (strict), Vite |
| Backend | Rust |
| Database | SQLite via `rusqlite` (bundled, with FTS5) |

Supported providers — OpenAI, Anthropic, Google Gemini, xKiro, OpenRouter, NVIDIA NIM, and OpenCode Zen (the last four as OpenAI-compatible endpoints) — and their supported models are defined in the backend (`src-tauri/src/infrastructure/providers/`).

## Architecture

Nexora follows a layered architecture; full details are in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

```
React UI (src/)
      ↓
Tauri IPC commands (thin translation layer)
      ↓
Application services (business rules)
      ↓
Infrastructure: SQLite repositories · AI provider clients · OS keyring
```

- The React frontend (`src/`) contains no business logic; components access data only through typed wrappers around Tauri commands in `src/lib/`.
- The Rust backend (`src-tauri/src/`) is organized into command handlers, application services, domain entities, and infrastructure.
- All persistent data lives in one portable SQLite file (`nexora.db`) in the operating system's per-user application-data directory. The schema is created exclusively by forward-only migrations embedded in the backend.
- AI providers are external services reached only through the backend, behind a provider-independent execution boundary that handles credential lookup, request execution, and error classification.

## Privacy & Security

- **Local-first data ownership** — every conversation, prompt, attachment record, and setting resides locally in one self-contained database file. There are no user accounts, no shared workspaces, and no background synchronization.
- **Offline availability** — core functionality works without internet access. Data leaves the machine only when you initiate an AI request with a configured provider.
- **Protected credentials** — API keys are stored exclusively in the operating system's secure keyring. They are never written to SQLite, log output, or source code, and errors returned to the UI carry no secret material.

## Install

Users download Nexora from GitHub Releases (latest): [https://github.com/sshdw/Nexora/releases/latest](https://github.com/sshdw/Nexora/releases/latest). The recommended installer is the NSIS `Nexora_*_x64-setup.exe`; the MSI `Nexora_*_x64_en-US.msi` is also published.

## Getting Started

### Prerequisites

- Latest stable Rust
- Node.js and npm
- Tauri v2 system dependencies for your platform ([official prerequisites](https://tauri.app/start/prerequisites/))

### Run in development

```bash
npm install
npm run tauri dev
```

The dev server binds to port 1420 (strict); the port must be free before starting.

### Production build

```bash
npm run tauri build
```

## Development

There is intentionally no JavaScript test runner. Automated coverage lives entirely in the Rust backend.

| Task | Command |
|---|---|
| Frontend typecheck | `npx tsc` |
| Frontend lint | `npx eslint .` |
| Backend unit tests | `cargo test` inside `src-tauri/` |
| Backend lint | `cargo clippy` inside `src-tauri/` |
| Backend formatting | `cargo fmt` inside `src-tauri/` |

Backend tests run against in-memory SQLite instances using the same migrations as production.

## Project Status

**Nexora 1.1.0** — MVP + agent era shipped; see CHANGELOG.

Capabilities outside the approved MVP (cloud sync, user accounts, collaboration, mobile/web apps, plugins) are explicitly out of scope — see [docs/SRS.md](docs/SRS.md).

## Documentation

Approved specification documents live in [`docs/`](docs/):

- [SRS.md](docs/SRS.md) — requirements specification (FR-001–FR-015)
- [ARCHITECTURE.md](docs/ARCHITECTURE.md) — technical architecture
- [DATABASE.md](docs/DATABASE.md) — database design and migration strategy
- [ROADMAP.md](docs/ROADMAP.md) — implementation phases (historical MVP phases)
- [AGENT-E2E.md](docs/AGENT-E2E.md) — manual UI checklist
- [AGENT-4.3-DESIGN.md](docs/AGENT-4.3-DESIGN.md) and [AGENT-5.1-DESIGN.md](docs/AGENT-5.1-DESIGN.md) — historical design records
- [CHANGELOG.md](CHANGELOG.md)
