import Link from 'next/link'

export default function SecurityCta() {
  return (
    <section className="py-12 md:py-16">
      <div className="mx-auto max-w-[1240px] px-5 md:px-6 lg:px-8">
        <div className="max-w-[760px] mx-auto">
          <div className="bg-[#0E0E11] border border-[#303238] rounded-lg p-8 md:p-12 text-center">
            <h2 className="text-[#F3F3F4] text-[34px] md:text-[42px] lg:text-[48px] leading-none tracking-tight">
              Trust should be inspectable.
            </h2>
            <p className="mt-4 text-base leading-relaxed text-[#A4A7AD]">
              Your workspace stays local. Credentials are stored in the OS
              keyring. Agent actions remain visible. Read how Nexora handles
              data, provider requests, permissions, and known limitations.
            </p>
            <div className="mt-8">
              <Link href="/security" className="demo-btn">
                Read the security model &rarr;
              </Link>
            </div>
          </div>
        </div>
      </div>
    </section>
  )
}
