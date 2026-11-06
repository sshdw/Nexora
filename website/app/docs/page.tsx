export const metadata = {
  title: 'Docs',
  description:
    'User-facing docs for setting up and using Nexora: local-first workspace, providers, agent runs, and where to find the specification documents.',
  openGraph: {
    url: '/docs',
  },
}

const GITHUB_URL = 'https://github.com/sshdw/Nexora'
const DOCS_DIR = `${GITHUB_URL}/tree/main/docs`

export default function Page() {
  return (
    <main className="min-h-[100dvh] bg-canvas text-text font-sans">
      <div className="mx-auto max-w-[1240px] px-5 md:px-6 lg:px-8">
        <section className="py-12 md:py-20">
          <div className="max-w-[760px]">
            <h1 className="text-[42px] md:text-[54px] lg:text-[68px] leading-none tracking-tight">
              Docs
            </h1>
            <p className="mt-6 text-base leading-relaxed text-muted">
              Nexora is a local-first AI desktop workspace. These docs explain
              how to set it up, connect providers, run agents, and where to find
              the specification documents in the repository.
            </p>
          </div>
        </section>

        <section className="py-12 md:py-16 border-t border-border pt-[calc(1rem+1px)]">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">What these docs cover</h2>
            <ul className="mt-4 list-disc ml-6 space-y-3 text-base leading-relaxed">
              <li>
                Setting up Nexora: prerequisites, dev build, and production
                bundle.
              </li>
              <li>
                Connecting AI providers: OpenAI, Anthropic, Google Gemini,
                xKiro, OpenRouter, NVIDIA NIM, OpenCode Zen, or your own
                OpenAI-compatible endpoint.
              </li>
              <li>
                Agent runs: autonomy levels, approval gates, and run controls.
              </li>
              <li>
                Where your data lives: single SQLite file on your disk, OS
                keyring for credentials.
              </li>
              <li>
                Links to the specification documents and the changelog.
              </li>
            </ul>
          </div>
        </section>

        <section className="py-12 md:py-16">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">Audience</h2>
            <p className="mt-4 text-base leading-relaxed text-muted">
              These docs are for people running Nexora on their own machine.
              There is no cloud account, no sync service, and no multi-user
              portal. If you are looking for the backend API reference, Nexora
              is a Tauri desktop app with local IPC commands - it does not
              expose a public HTTP API.
            </p>
          </div>
        </section>

        <section className="py-12 md:py-16">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">
              Specification documents
            </h2>
            <p className="mt-3 text-base leading-relaxed text-muted">
              Approved specification documents live in the
              <code className="font-mono text-sm">docs/</code> directory of the
              repository:
            </p>
            <ul className="mt-4 list-disc ml-6 space-y-2 text-base leading-relaxed">
              <li>
                <a
                  href={`${DOCS_DIR}/SRS.md`}
                  target="_blank"
                  rel="noopener"
                  className="text-accent hover:text-accent-hover transition-colors duration-200"
                >
                  SRS.md - requirements specification
                </a>
              </li>
              <li>
                <a
                  href={`${DOCS_DIR}/ARCHITECTURE.md`}
                  target="_blank"
                  rel="noopener"
                  className="text-accent hover:text-accent-hover transition-colors duration-200"
                >
                  ARCHITECTURE.md - layered technical architecture
                </a>
              </li>
              <li>
                <a
                  href={`${DOCS_DIR}/DATABASE.md`}
                  target="_blank"
                  rel="noopener"
                  className="text-accent hover:text-accent-hover transition-colors duration-200"
                >
                  DATABASE.md - schema and migration strategy
                </a>
              </li>
              <li>
                <a
                  href={`${DOCS_DIR}/CONTEXT-WINDOWS.md`}
                  target="_blank"
                  rel="noopener"
                  className="text-accent hover:text-accent-hover transition-colors duration-200"
                >
                  CONTEXT-WINDOWS.md - enforced context windows, compaction,
                  capability fallback
                </a>
              </li>
              <li>
                <a
                  href={`${DOCS_DIR}/ROADMAP.md`}
                  target="_blank"
                  rel="noopener"
                  className="text-accent hover:text-accent-hover transition-colors duration-200"
                >
                  ROADMAP.md - historical MVP phase record
                </a>
              </li>
            </ul>
          </div>
        </section>

        <section className="py-12 md:py-16">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">Provider setup</h2>
            <p className="mt-3 text-base leading-relaxed text-muted">
              Nexora connects to the providers you configure yourself. Credentials
              are stored in the OS keyring and are never written to SQLite,
              logs, or error text. Supported providers:
            </p>
            <ul className="mt-4 list-disc ml-6 space-y-2 text-base leading-relaxed">
              <li>OpenAI</li>
              <li>Anthropic</li>
              <li>Google Gemini</li>
              <li>xKiro</li>
              <li>OpenRouter</li>
              <li>NVIDIA NIM</li>
              <li>OpenCode Zen</li>
              <li>Custom OpenAI-compatible endpoint</li>
            </ul>
          </div>
        </section>

        <section className="py-12 md:py-16 border-b border-border pb-[calc(1rem+1px)]">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">Related pages</h2>
            <div className="mt-4 grid grid-cols-1 sm:grid-cols-2 gap-4">
              <a
                href="/changelog"
                className="rounded-md border border-border bg-surface p-4 text-left hover:bg-accent/05 transition-colors duration-200"
              >
                <p className="font-medium text-text">Changelog</p>
                <p className="text-sm text-muted">Release notes for each Nexora version.</p>
              </a>
              <a
                href="/security"
                className="rounded-md border border-border bg-surface p-4 text-left hover:bg-accent/05 transition-colors duration-200"
              >
                <p className="font-medium text-text">Security model</p>
                <p className="text-sm text-muted">Local-first workspace, OS keyring, approval gates, known limitations.</p>
              </a>
              <a
                href="/privacy"
                className="rounded-md border border-border bg-surface p-4 text-left hover:bg-accent/05 transition-colors duration-200"
              >
                <p className="font-medium text-text">Privacy</p>
                <p className="text-sm text-muted">What data is collected, what is not collected, retention.</p>
              </a>
              <a
                href="/download"
                className="rounded-md border border-border bg-surface p-4 text-left hover:bg-accent/05 transition-colors duration-200"
              >
                <p className="font-medium text-text">Download</p>
                <p className="text-sm text-muted">Windows installer, build from source, GitHub releases.</p>
              </a>
            </div>
          </div>
        </section>
      </div>
    </main>
  )
}
