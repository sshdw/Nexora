export default function StayInControl() {
  return (
    <section className="py-12 md:py-16">
      <div className="mx-auto max-w-[1240px] px-5 md:px-6 lg:px-8">
        <div className="max-w-[760px] mx-auto text-center">
          <p className="text-[10px] font-semibold uppercase tracking-wider text-muted mb-3">
            Stay in control
          </p>
          <h2 className="text-[34px] md:text-[42px] lg:text-[48px] leading-none tracking-tight">
            Dangerous actions stop for you.
          </h2>
          <p className="mt-4 text-base leading-relaxed text-muted">
            Choose how much autonomy an agent gets. Shell commands stop for your
            approval unless you choose full-autonomous mode, where the agent runs
            to the end without stopping.
          </p>
        </div>

        <div className="mt-12 lg:mt-16 flex justify-center">
          <div className="w-full max-w-[760px] bg-[#0E0E11] border border-[#303238] rounded-lg p-6 md:p-8">
            <p className="text-[10px] font-mono text-[#A4A7AD] mb-1">
              Command: npm test
            </p>
            <p className="text-[#F3F3F4]">
              This command can read and modify project files. Approve or deny it
              before the agent continues.
            </p>
            <div className="mt-6 flex flex-col sm:flex-row gap-3">
              <button type="button" className="demo-cta">
                Allow
              </button>
              <button type="button" className="demo-btn">
                Deny
              </button>
            </div>
            <p className="mt-4 text-[11px] text-[#A4A7AD]">
              Autonomy: supervised · semi-autonomous (default) ·
              full-autonomous
            </p>
          </div>
        </div>
      </div>
    </section>
  )
}
