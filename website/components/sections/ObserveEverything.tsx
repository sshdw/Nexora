import ActivitySpine, { ActivityNodeStatus } from '@/components/ActivitySpine'

export default function ObserveEverything() {
  const nodes: Array<{ id: string; label: string; status: ActivityNodeStatus }> = [
    { id: 'read', label: 'Read files', status: 'done' },
    { id: 'approval', label: 'Approval', status: 'done' },
    { id: 'edit', label: 'Edits', status: 'done' },
    { id: 'review', label: 'Review', status: 'active' },
  ]

  const calls = [
    { tool: 'read_file', target: 'src/auth.ts', time: '45 ms' },
    { tool: 'read_file', target: 'src/auth.test.ts', time: '30 ms' },
    { tool: 'command', target: 'npm test', time: '12 s' },
    { tool: 'write_file', target: 'src/auth.ts', time: '-' },
    { tool: 'write_file', target: 'src/auth.test.ts', time: '-' },
    { tool: 'verification', target: 'npm test', time: '11 s' },
  ]

  return (
    <section className="py-12 md:py-16">
      <div className="mx-auto max-w-[1240px] px-5 md:px-6 lg:px-8">
        <div className="grid md:grid-cols-2 gap-8 md:gap-12 items-start">
          <div className="max-w-[760px]">
            <p className="text-[10px] font-semibold uppercase tracking-wider text-muted mb-3">
              Observe everything
            </p>
            <h2 className="text-[34px] md:text-[42px] lg:text-[48px] leading-none tracking-tight">
              See what the agent is doing.
            </h2>
            <p className="mt-4 text-base leading-relaxed text-muted">
              Follow file reads, tool calls, commands, edits, and verification as one continuous run.
            </p>
          </div>
          <div className="bg-[#0E0E11] border border-[#303238] rounded-lg p-6 md:p-8">
            <ActivitySpine nodes={nodes} ariaLabel="Sample run Activity Spine" />
            <div className="mt-6">
              <p className="text-[10px] font-semibold uppercase tracking-wider text-[#A4A7AD] mb-3">
                Tool calls in this run
              </p>
              <ul className="space-y-2">
                {calls.map((call, index) => (
                  <li key={index} className="flex items-center justify-between gap-3 rounded-md border border-transparent px-3 py-2 text-left hover:border-[#6D727A]/50 transition-colors duration-200">
                    <div className="flex items-center gap-3">
                      <span
                        className="w-1.5 h-1.5 rounded-full bg-[#9AB8C8]"
                        aria-hidden="true"
                      />
                      <span className="font-mono text-xs text-[#F3F3F4]">{call.tool}</span>
                      <span className="font-mono text-xs text-[#A4A7AD]">{call.target}</span>
                    </div>
                    <span className="font-mono text-xs text-[#A4A7AD]">{call.time}</span>
                  </li>
                ))}
              </ul>
            </div>
          </div>
        </div>
      </div>
    </section>
  )
}
