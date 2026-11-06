import type { Metadata } from 'next'

import HeroDemo from '@/components/HeroDemo'
import DownloadButton from '@/components/DownloadButton'
import ObserveEverything from '@/components/sections/ObserveEverything'
import StayInControl from '@/components/sections/StayInControl'
import Review from '@/components/sections/Review'
import Providers from '@/components/sections/Providers'
import OpenSource from '@/components/sections/OpenSource'
import SecurityCta from '@/components/sections/SecurityCta'
import DownloadCta from '@/components/sections/DownloadCta'

const GITHUB_URL = 'https://github.com/sshdw/Nexora'

export const metadata: Metadata = {
  title: 'Code with agents. Keep the final say.',
  description:
    'A free, open-source AI coding workspace with visible agent actions, approvals, and reviewable changes.',
}

export default function Page() {
  return (
    <main className="min-h-[100dvh] bg-canvas text-text font-sans">
      <section className="pt-12 md:pt-16 lg:pt-24 pb-20 lg:pb-28">
        <div className="mx-auto max-w-[1240px] px-5 md:px-6 lg:px-8">
          <div className="max-w-[760px] mx-auto text-center">
            <h1 className="text-[42px] md:text-[54px] lg:text-[68px] leading-none tracking-tight">
              Code with agents.<br />
              Keep the final say.
            </h1>
            <p className="mt-6 text-base leading-relaxed text-muted">
              A free, open-source AI coding workspace with visible agent
              actions, approvals, and reviewable changes.
            </p>
            <div className="mt-10 lg:mt-12 flex flex-col sm:flex-row gap-3 justify-center">
              <DownloadButton />
              <a
                href="/download"
                className="text-sm font-medium text-muted hover:text-accent transition-colors duration-200"
              >
                Other downloads
              </a>
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
          <div className="mt-12 lg:mt-16 max-w-[1180px] mx-auto">
            <HeroDemo />
          </div>
        </div>
      </section>

      <section className="py-8 md:py-12">
        <div className="mx-auto max-w-[1240px] px-5 md:px-6 lg:px-8">
          <p className="text-center text-sm text-muted">
            Free &middot; Open source &middot; Local-first &middot; Bring your own
            models
          </p>
        </div>
      </section>

      <ObserveEverything />
      <StayInControl />
      <Review />
      <Providers />
      <OpenSource />
      <SecurityCta />
      <DownloadCta />
    </main>
  )
}
