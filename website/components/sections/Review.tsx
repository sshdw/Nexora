export default function Review() {
  return (
    <section className="py-12 md:py-16">
      <div className="mx-auto max-w-[1240px] px-5 md:px-6 lg:px-8">
        <div className="grid md:grid-cols-2 gap-10 md:gap-16 items-start">
          <div className="max-w-[760px]">
            <h2 className="text-[34px] md:text-[42px] lg:text-[48px] leading-none tracking-tight">
              Review the change, not just the answer.
            </h2>
            <p className="mt-4 text-base leading-relaxed text-muted">
              See what changed, inspect the diff, and keep the result connected
              to the run that produced it.
            </p>
          </div>
          <div className="bg-[#0E0E11] border border-[#303238] rounded-lg overflow-hidden">
            <div className="border-b border-[#303238] bg-[#18191C]/50 px-4 py-3">
              <div className="flex items-center justify-between">
                <span className="font-mono text-xs text-[#A4A7AD]">
                  src/auth.ts
                </span>
                <span className="text-[10px] text-[#F3F3F4]">+2 -1</span>
              </div>
            </div>
            <pre className="p-4 text-xs font-mono leading-none">
              <span className="block diff-line hdr">diff --git a/src/auth.ts b/src/auth.ts</span>
              <span className="block diff-line hdr">--- a/src/auth.ts</span>
              <span className="block diff-line hdr">+++ b/src/auth.ts</span>
              <span className="block diff-line hdr">
                @@ -22,7 +22,7 @@ export function validateCredentials
              </span>
              <span className="block diff-line ctx pl-4">
                {'const record = users.find((u) => u.name === user)'}
              </span>
              <span className="block diff-line remove">
                -  return record?.password === password
              </span>
              <span className="block diff-line add">
                +  return record ? record.password === password : false
              </span>
              <span className="block diff-line ctx pl-4">
                {'}'}
              </span>
            </pre>
          </div>
        </div>
      </div>
    </section>
  )
}
