export const metadata = {
  title: 'Download Nexora',
  description:
    'Download Nexora: prebuilt Windows installer, or build from source. All releases are on GitHub Releases.',
  openGraph: {
    url: '/download',
  },
}

const GITHUB_URL = 'https://github.com/sshdw/Nexora'
const RELEASES_URL = `${GITHUB_URL}/releases`
const LATEST_RELEASE = `${GITHUB_URL}/releases/latest`

export default function Page() {
  return (
    <main className="min-h-[100dvh] bg-canvas text-text font-sans">
      <div className="mx-auto max-w-[1240px] px-5 md:px-6 lg:px-8">
        <section className="py-12 md:py-20">
          <div className="max-w-[760px]">
            <h1 className="text-[42px] md:text-[54px] lg:text-[68px] leading-none tracking-tight">
              Download Nexora
            </h1>
            <p className="mt-6 text-base leading-relaxed text-muted">
              Get the prebuilt Windows installer, or build Nexora from source.
              All releases are published on GitHub Releases.
            </p>
          </div>
        </section>

        <section className="py-12 md:py-16 border-t border-border pt-[calc(1rem+1px)]">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">Prebuilt installers</h2>
            <p className="mt-3 text-base leading-relaxed text-muted">
              The latest release ships Windows installers. Other platforms build
              from source.
            </p>
            <div className="mt-6 flex flex-col sm:flex-row gap-4">
              <a
                href={LATEST_RELEASE}
                target="_blank"
                rel="noopener"
                className="rounded-md bg-primary-cta-bg text-primary-cta-text px-5 py-3 text-sm font-medium whitespace-nowrap hover:bg-primary-cta-bg/90 transition-colors duration-200"
              >
                Download for Windows
              </a>
              <a
                href={RELEASES_URL}
                target="_blank"
                rel="noopener"
                className="rounded-md border border-border bg-surface text-text px-5 py-3 text-sm font-medium whitespace-nowrap hover:bg-accent/05 hover:border-accent transition-colors duration-200"
              >
                All releases on GitHub
              </a>
            </div>
          </div>
        </section>

        <section className="py-12 md:py-16">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">Build from source</h2>
            <p className="mt-3 text-base leading-relaxed text-muted">
              macOS and Linux do not ship prebuilt installers. Build Nexora from
              source with the Tauri v2 toolchain.
            </p>
            <ul className="mt-4 list-disc ml-6 space-y-3 text-base leading-relaxed">
              <li>
                Prerequisites: a stable Rust toolchain (pinned by
                <code className="font-mono text-sm">src-tauri/rust-toolchain.toml</code>),
                Node.js and npm, and the
                <a
                  href="https://tauri.app/start/prerequisites/"
                  target="_blank"
                  rel="noopener"
                  className="text-accent hover:text-accent-hover transition-colors duration-200"
                >
                  Tauri v2 system dependencies
                </a>
                for your platform.
              </li>
              <li>
                Install dependencies:
                <code className="font-mono text-sm">npm install</code>
              </li>
              <li>
                Run in dev:
                <code className="font-mono text-sm">npm run tauri dev</code>
              </li>
              <li>
                Build production bundle:
                <code className="font-mono text-sm">npm run tauri build</code>
              </li>
            </ul>
          </div>
        </section>

        <section className="py-12 md:py-16 border-t border-border pt-[calc(1rem+1px)]">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">Checksums and verification</h2>
            <p className="mt-3 text-base leading-relaxed text-muted">
              When a release artifact is published, checksums are included on the
              GitHub Releases page for that release. Verify the downloaded file
              against the published checksum before installing.
            </p>
            <ul className="mt-4 list-disc ml-6 space-y-3 text-base leading-relaxed">
              <li>
                Download the installer from GitHub Releases.
              </li>
              <li>
                Compute the checksum of the downloaded file (for example with
                <code className="font-mono text-sm">certutil -hashfile</code> on
                Windows or <code className="font-mono text-sm">shasum -a 256</code>
                on macOS and Linux).
              </li>
              <li>
                Compare it to the checksum listed on the release page. If they
                match, the file was not altered in transit.
              </li>
            </ul>
          </div>
        </section>

        <section className="py-12 md:py-16 border-b border-border pb-[calc(1rem+1px)]">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">Platforms</h2>
            <div className="mt-4 grid md:grid-cols-3 gap-6">
              <div className="rounded-md border border-border bg-surface p-6">
                <p className="font-medium text-text">Windows</p>
                <p className="mt-2 text-sm text-muted">
                  Prebuilt NSIS setup.exe and MSI installers ship with each
                  release.
                </p>
              </div>
              <div className="rounded-md border border-border bg-surface p-6">
                <p className="font-medium text-text">macOS</p>
                <p className="mt-2 text-sm text-muted">
                  Build from source using the Tauri v2 prerequisites for macOS.
                </p>
              </div>
              <div className="rounded-md border border-border bg-surface p-6">
                <p className="font-medium text-text">Linux</p>
                <p className="mt-2 text-sm text-muted">
                  Build from source using the Tauri v2 prerequisites for Linux.
                </p>
              </div>
            </div>
          </div>
        </section>
      </div>
    </main>
  )
}
