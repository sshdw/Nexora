import DownloadButton from '@/components/DownloadButton'

const GITHUB_URL = 'https://github.com/sshdw/Nexora'

export default function DownloadCta() {
  return (
    <section className="py-16 md:py-20">
      <div className="mx-auto max-w-[1240px] px-5 md:px-6 lg:px-8">
        <div className="max-w-[760px] mx-auto text-center">
          <h2 className="text-[34px] md:text-[42px] lg:text-[48px] leading-none tracking-tight">
            Download Nexora
          </h2>
          <p className="mt-4 text-base leading-relaxed text-muted">
            Prebuilt Windows installer, or build Nexora from source. All
            releases are on GitHub.
          </p>
          <div className="mt-10 lg:mt-12 flex flex-col sm:flex-row gap-3 justify-center">
            <DownloadButton />
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
