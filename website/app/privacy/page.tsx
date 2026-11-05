export default function Page() {
  return (
    <main className="min-h-[100dvh] bg-canvas text-text font-sans">
      <div className="mx-auto max-w-[1240px] px-5 md:px-6 lg:px-8">
        <section className="py-12 md:py-16">
          <div className="max-w-[760px]">
            <h1 className="text-[42px] md:text-[54px] lg:text-[68px] leading-none tracking-tight">
              Privacy
            </h1>
            <p className="mt-4 text-base leading-relaxed text-muted">
              Nexora runs locally and does not send your code or prompts to any telemetry service.
            </p>
          </div>
        </section>
      </div>
    </main>
  )
}
