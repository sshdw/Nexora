export const metadata = {
  title: 'Privacy',
  description:
    'Nexora privacy: what data is collected, what is not collected, analytics, server logs, retention, external services, and GitHub download links.',
  openGraph: {
    url: '/privacy',
  },
}

export default function Page() {
  return (
    <main className="min-h-[100dvh] bg-canvas text-text font-sans">
      <div className="mx-auto max-w-[1240px] px-5 md:px-6 lg:px-8">
        <section className="py-12 md:py-20">
          <div className="max-w-[760px]">
            <h1 className="text-[42px] md:text-[54px] lg:text-[68px] leading-none tracking-tight">
              Privacy
            </h1>
            <p className="mt-6 text-base leading-relaxed text-muted">
              Nexora is a local-first desktop app. Your conversations, prompts,
              attachments, and settings live in a single SQLite file on your
              disk. There is no Nexora account, no cloud sync, and no telemetry
              upload. This page explains what data is collected, what is not
              collected, and how the website and the app handle your
              information.
            </p>
          </div>
        </section>

        <section className="py-12 md:py-16">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">Collected data</h2>
            <p className="mt-3 text-base leading-relaxed">
              Nexora collects nothing from you by default. Your workspace stays
              on your device. Nothing leaves unless you send a request to a
              provider you configured yourself.
            </p>
          </div>
        </section>

        <section className="py-12 md:py-16">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">Data not collected</h2>
            <ul className="mt-4 list-disc ml-6 space-y-2 text-base leading-relaxed">
              <li>
                No Nexora account data - there are no accounts.
              </li>
              <li>
                No cloud copies of your workspace - there is no sync service.
              </li>
              <li>No prompts or source code sent to Nexora.</li>
              <li>No telemetry upload anywhere.</li>
              <li>No remote crash reports.</li>
              <li>No fingerprints or tracking profiles.</li>
            </ul>
          </div>
        </section>

        <section className="py-12 md:py-16">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">Analytics</h2>
            <p className="mt-3 text-base leading-relaxed">
              Analytics on this website is a disabled no-op adapter. It is not
              yet implemented, and no visitor data is sent to any analytics
              service. If analytics is added later, it will be self-hosted and
              privacy-friendly, with an event vocabulary that never sends
              prompts, source code, filenames, keys, or search query contents.
            </p>
          </div>
        </section>

        <section className="py-12 md:py-16">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">Server logs</h2>
            <p className="mt-3 text-base leading-relaxed">
              This website is served as static files. Server access logs may
              record standard request metadata, such as an IP address and
              requested path, for operation and security. They are not used for
              tracking and are retained only as needed for operations. The
              Nexora desktop app sends no logs to any server.
            </p>
          </div>
        </section>

        <section className="py-12 md:py-16">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">Retention</h2>
            <p className="mt-3 text-base leading-relaxed">
              Your data stays on your device for as long as you keep it. Delete
              the SQLite file to remove your workspace. There is no Nexora
              server retention because Nexora does not keep a copy of your
              data.
            </p>
          </div>
        </section>

        <section className="py-12 md:py-16">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">External services</h2>
            <p className="mt-3 text-base leading-relaxed">
              Only the services you choose:
            </p>
            <ul className="mt-4 list-disc ml-6 space-y-2 text-base leading-relaxed">
              <li>
                AI providers you configure (OpenAI, Anthropic, Gemini,
                OpenRouter, NVIDIA NIM, OpenCode Zen, or your own
                OpenAI-compatible endpoint) receive only the requests you send
                to them.
              </li>
              <li>
                GitHub hosts the source code and releases. Download links point
                to GitHub Releases.
              </li>
              <li>No other third-party services are contacted by default.</li>
            </ul>
          </div>
        </section>

        <section className="py-12 md:py-16">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">
              GitHub and download links
            </h2>
            <p className="mt-3 text-base leading-relaxed">
              Source code: github.com/sshdw/Nexora (MIT licensed). Downloads:
              GitHub Releases. Windows installers are available when a real
              release artifact exists; other platforms build from source.
              Checksums or signatures are included only when actually
              published.
            </p>
          </div>
        </section>

        <section className="py-12 md:py-16">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">No cookie banner</h2>
            <p className="mt-3 text-base leading-relaxed">
              This website does not use tracking cookies, so no cookie banner
              is shown.
            </p>
          </div>
        </section>
      </div>
    </main>
  )
}
