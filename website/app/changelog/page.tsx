export const metadata = {
  title: 'Changelog',
  description:
    'Release notes for each Nexora version. Latest release: v2.0.0. Full history in the repository CHANGELOG.md and GitHub Releases.',
  openGraph: {
    url: '/changelog',
  },
}

const GITHUB_URL = 'https://github.com/sshdw/Nexora'
const RELEASES_URL = `${GITHUB_URL}/releases`
const LATEST_RELEASE = `${GITHUB_URL}/releases/latest`
const CHANGELOG_BLOB = `${GITHUB_URL}/blob/main/CHANGELOG.md`

export default function Page() {
  return (
    <main className="min-h-[100dvh] bg-canvas text-text font-sans">
      <div className="mx-auto max-w-[1240px] px-5 md:px-6 lg:px-8">
        <section className="py-12 md:py-20">
          <div className="max-w-[760px]">
            <h1 className="text-[42px] md:text-[54px] lg:text-[68px] leading-none tracking-tight">
              Changelog
            </h1>
            <p className="mt-6 text-base leading-relaxed text-muted">
              Release notes for each Nexora version. Latest release:
              <strong>v2.0.0</strong>. Full history is in the repository
              <a
                href={CHANGELOG_BLOB}
                target="_blank"
                rel="noopener"
                className="text-accent hover:text-accent-hover transition-colors duration-200"
              >
                CHANGELOG.md
              </a>
              and on
              <a
                href={RELEASES_URL}
                target="_blank"
                rel="noopener"
                className="text-accent hover:text-accent-hover transition-colors duration-200"
              >
                GitHub Releases
              </a>
              .
            </p>
          </div>
        </section>

        <section className="py-12 md:py-16 border-t border-border pt-[calc(1rem+1px)]">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">Latest release: v2.0.0</h2>
            <p className="mt-4 text-base leading-relaxed text-muted">
              Visual rebuild of the Nexora desktop app. Multi-provider chat and
              supervised agent runs with everything stored on your own machine.
              Windows installers ship with this release; other platforms build
              from source.
            </p>
            <div className="mt-6 flex flex-col sm:flex-row gap-4">
              <a
                href={LATEST_RELEASE}
                target="_blank"
                rel="noopener"
                className="rounded-md bg-primary-cta-bg text-primary-cta-text px-5 py-2 text-sm font-medium whitespace-nowrap hover:bg-primary-cta-bg/90 transition-colors duration-200"
              >
                View v2.0.0 on GitHub Releases
              </a>
              <a
                href={RELEASES_URL}
                target="_blank"
                rel="noopener"
                className="rounded-md border border-border bg-surface text-text px-5 py-2 text-sm font-medium whitespace-nowrap hover:bg-accent/05 hover:border-accent transition-colors duration-200"
              >
                All releases
              </a>
            </div>
          </div>
        </section>

        <section className="py-12 md:py-16">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">What changed</h2>
            <p className="mt-4 text-base leading-relaxed text-muted">
              v2.0.0 is the current stable release. Earlier release notes are
              maintained in the repository
              <a
                href={CHANGELOG_BLOB}
                target="_blank"
                rel="noopener"
                className="text-accent hover:text-accent-hover transition-colors duration-200"
              >
                CHANGELOG.md
              </a>
              . Individual release artifacts and notes are published on
              <a
                href={RELEASES_URL}
                target="_blank"
                rel="noopener"
                className="text-accent hover:text-accent-hover transition-colors duration-200"
              >
                GitHub Releases
              </a>
              .
            </p>
          </div>
        </section>

        <section className="py-12 md:py-16 border-b border-border pb-[calc(1rem+1px)]">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">Where to find release notes</h2>
            <ul className="mt-4 list-disc ml-6 space-y-3 text-base leading-relaxed">
              <li>
                Repository:
                <a
                  href={CHANGELOG_BLOB}
                  target="_blank"
                  rel="noopener"
                  className="text-accent hover:text-accent-hover transition-colors duration-200"
                >
                  CHANGELOG.md
                </a>
              </li>
              <li>
                Releases page:
                <a
                  href={RELEASES_URL}
                  target="_blank"
                  rel="noopener"
                  className="text-accent hover:text-accent-hover transition-colors duration-200"
                >
                  GitHub Releases
                </a>
              </li>
              <li>
                Latest release:
                <a
                  href={LATEST_RELEASE}
                  target="_blank"
                  rel="noopener"
                  className="text-accent hover:text-accent-hover transition-colors duration-200"
                >
                  v2.0.0
                </a>
              </li>
            </ul>
          </div>
        </section>
      </div>
    </main>
  )
}
