const GITHUB_URL = 'https://github.com/sshdw/Nexora'

export default function OpenSource() {
  return (
    <section className="py-12 md:py-16">
      <div className="mx-auto max-w-[1240px] px-5 md:px-6 lg:px-8">
        <div className="max-w-[760px] mx-auto text-center">
          <h2 className="text-[34px] md:text-[42px] lg:text-[48px] leading-none tracking-tight">
            No black box between you and your agents.
          </h2>
          <p className="mt-4 text-base leading-relaxed text-muted">
            Inspect the code. Bring your own models. Own your workflow.
          </p>
          <p className="mt-6 text-sm text-muted">
            MIT licensed · Open source on GitHub · No account required
          </p>
          <div className="mt-8">
            <a
              href={GITHUB_URL}
              target="_blank"
              rel="noopener"
              className="text-sm font-medium text-muted hover:text-accent transition-colors duration-200"
            >
              View on GitHub
            </a>
          </div>
        </div>
      </div>
    </section>
  )
}
