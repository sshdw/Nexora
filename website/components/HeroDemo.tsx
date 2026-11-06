'use client'

import { useCallback, useEffect, useRef, useState } from 'react'
import ActivitySpine from './ActivitySpine'

const PROJECT = 'example-app'
const PROMPT = 'Fix the failing authentication test.'
const FILES = ['src/auth.ts', 'src/auth.test.ts']
const COMMAND = 'npm test'

type DemoState =
  | 'idle'
  | 'reading'
  | 'calling'
  | 'foundIssue'
  | 'commandRequested'
  | 'awaitingApproval'
  | 'editing'
  | 'diffReady'
  | 'verifying'
  | 'passed'
  | 'finished'

const IDLE_DELAY = 800
const STATE_DELAY = 350
const GATED_DELAY = 1000

const GATED_STATES = new Set<DemoState>(['awaitingApproval', 'verifying'])

const autoNext = (state: DemoState): DemoState | null => {
  switch (state) {
    case 'idle':
      return 'reading'
    case 'reading':
      return 'calling'
    case 'calling':
      return 'foundIssue'
    case 'foundIssue':
      return 'commandRequested'
    case 'commandRequested':
      return 'awaitingApproval'
    case 'awaitingApproval':
      return 'editing'
    case 'editing':
      return 'diffReady'
    case 'diffReady':
      return 'verifying'
    case 'verifying':
      return 'passed'
    case 'passed':
      return 'finished'
    case 'finished':
      return null
  }
}

const spineNodes = [
  { id: 'read', label: 'Read files' },
  { id: 'approval', label: 'Approval' },
  { id: 'edit', label: 'Edits' },
  { id: 'review', label: 'Review' },
] as const

const spineTarget: Record<DemoState, string | null> = {
  idle: null,
  reading: 'read',
  calling: 'approval',
  foundIssue: 'approval',
  commandRequested: 'approval',
  awaitingApproval: 'approval',
  editing: 'edit',
  diffReady: 'edit',
  verifying: 'review',
  passed: 'review',
  finished: 'review',
}

const spineStatus = (
  state: DemoState,
): Record<string, 'pending' | 'active' | 'done'> => {
  const base: Record<string, 'pending' | 'active' | 'done'> = {
    read: 'pending',
    approval: 'pending',
    edit: 'pending',
    review: 'pending',
  }
  switch (state) {
    case 'idle':
      return base
    case 'reading':
      return { ...base, read: 'active' }
    case 'calling':
    case 'foundIssue':
    case 'commandRequested':
    case 'awaitingApproval':
      return { ...base, read: 'done', approval: 'active' }
    case 'editing':
    case 'diffReady':
      return { ...base, read: 'done', approval: 'done', edit: 'active' }
    case 'verifying':
    case 'passed':
      return { ...base, read: 'done', approval: 'done', edit: 'done', review: 'active' }
    case 'finished':
      return { ...base, read: 'done', approval: 'done', edit: 'done', review: 'done' }
  }
}

const panelContent = (state: DemoState): React.ReactNode => {
  switch (state) {
    case 'idle':
      return (
        <>
          <p>Agent is idle.</p>
          <p className="mt-2 text-xs text-[#A4A7AD]">
            Ready to walk through: {PROJECT}. {PROMPT}
          </p>
          <p className="mt-4 text-xs text-[#A4A7AD]">
            The walkthrough plays once on its own. Click Allow, Run tests,
            or any Activity Spine step to take manual control.
          </p>
        </>
      )
    case 'reading':
      return (
        <>
          <p>Agent reads the project files.</p>
          <p className="mt-2 text-xs font-mono text-[#A4A7AD]">
            {FILES.join('  ·  ')}
          </p>
        </>
      )
    case 'calling':
      return (
        <>
          <p>Tool call: read_file({FILES[0]})</p>
          <p className="mt-2 text-xs text-[#A4A7AD]">Agent inspects the test and the source.</p>
        </>
      )
    case 'foundIssue':
      return (
        <>
          <p>Issue found.</p>
          <p className="mt-2 text-xs text-[#A4A7AD]">
            validateCredentials returns before checking the password hash.
          </p>
        </>
      )
    case 'commandRequested':
      return (
        <>
          <p>Command request: {COMMAND}.</p>
          <p className="mt-2 text-xs text-[#A4A7AD]">
            Shell commands stop for your approval.
          </p>
        </>
      )
    case 'awaitingApproval':
      return (
        <div className="flex max-w-xs flex-col gap-3">
          <p>Allow {COMMAND} to run?</p>
          <p className="mt-1 text-xs text-[#A4A7AD]">
            This command can read and modify project files.
          </p>
        </div>
      )
    case 'editing':
      return (
        <>
          <p>Agent edits two files.</p>
          <p className="mt-2 text-xs font-mono text-[#A4A7AD]">
            {FILES.join('  ·  ')}
          </p>
        </>
      )
    case 'diffReady':
      return (
        <>
          <p>Diff ready. Review the change.</p>
          <DemoDiff />
        </>
      )
    case 'verifying':
      return (
        <>
          <p>Verification: {COMMAND}.</p>
          <DemoDiff />
        </>
      )
    case 'passed':
    case 'finished':
      return (
        <>
          <p className="text-[#9AB8C8] font-medium">Tests passed · 2 files changed</p>
          <DemoDiff className="mt-4" />
        </>
      )
  }
}

function DemoDiff({ className = '' }: { className?: string }): React.ReactNode {
  const lines = [
    'diff --git a/src/auth.ts b/src/auth.ts',
    '--- a/src/auth.ts',
    '+++ b/src/auth.ts',
    '@@ -22,7 +22,7 @@ export function validateCredentials',
    '   const record = users.find((u) => u.name === user)',
    '-  return record?.password === password',
    '+  return record ? record.password === password : false',
    ' }',
  ]
  return (
    <pre className={`mt-2 overflow-auto rounded-md border border-[#303238] bg-[#18191C] p-3 text-xs font-mono leading-none ${className}`}>
      {lines.map((line, i) => {
        const cls =
          line.startsWith('+')
            ? 'diff-line add'
            : line.startsWith('-')
              ? 'diff-line remove'
              : line.startsWith('@') || line.startsWith('diff') || line.startsWith('---') || line.startsWith('+++')
                ? 'diff-line hdr'
                : 'diff-line ctx'
        return (
          <span key={i} className={cls}>
            {line}
          </span>
        )
      })}
    </pre>
  )
}

function useReducedMotion(): boolean {
  const [reduced, setReduced] = useState(false)
  useEffect(() => {
    const mq = window.matchMedia('(prefers-reduced-motion: reduce)')
    const update = () => setReduced(mq.matches)
    update()
    mq.addEventListener('change', update)
    return () => mq.removeEventListener('change', update)
  }, [])
  return reduced
}

export default function HeroDemo() {
  const [state, setState] = useState<DemoState>('idle')
  const [manual, setManual] = useState(false)
  const [openFile, setOpenFile] = useState<string | null>(null)
  const timers = useRef<Array<ReturnType<typeof setTimeout>>>([])
  const reducedMotion = useReducedMotion()

  const clearTimers = useCallback(() => {
    timers.current.forEach((t) => clearTimeout(t))
    timers.current = []
  }, [])

  // Reduced motion: show a completed, informative static state; no autoplay.
  useEffect(() => {
    if (reducedMotion && state !== 'finished') {
      setState('finished')
    }
  }, [reducedMotion, state])

  // Autopilot: plays the walkthrough once unless the user takes manual control.
  useEffect(() => {
    if (reducedMotion) {
      clearTimers()
      return
    }
    clearTimers()
    const next = autoNext(state)
    if (next === null) {
      return
    }
    if (GATED_STATES.has(state) && manual) {
      return
    }
    if (manual && state === 'idle') {
      return
    }
    const delay =
      state === 'idle'
        ? IDLE_DELAY
        : GATED_STATES.has(state)
          ? GATED_DELAY
          : STATE_DELAY
    const timer = setTimeout(() => {
      if (GATED_STATES.has(state) && manual) {
        return
      }
      setState(next)
    }, delay)
    timers.current.push(timer)
    return () => clearTimeout(timer)
  }, [state, manual, reducedMotion, clearTimers])

  const goTo = useCallback(
    (next: DemoState) => {
      setManual(true)
      setState(next)
    },
    [],
  )

  const onAllow = useCallback(() => {
    setManual(true)
    setState('editing')
  }, [])

  const onRunTests = useCallback(() => {
    setManual(true)
    setState('passed')
  }, [])

  const onReplay = useCallback(() => {
    setManual(true)
    setState('idle')
    setOpenFile(null)
  }, [])

  const onSpineSelect = useCallback(
    (id: string) => {
      // Jump to the flow step that matches the selected spine node.
      let target: DemoState = state
      if (id === 'read') target = 'reading'
      if (id === 'approval') target = 'awaitingApproval'
      if (id === 'edit') target = 'editing'
      if (id === 'review') target = 'verifying'
      goTo(target)
    },
    [goTo],
  )

  return (
    <section className="demo max-w-[1180px] mx-auto" aria-label="Interactive demo: Nexora agent walkthrough">
      <div className="flex flex-col gap-4 pt-4 pb-6">
        <div className="flex items-center gap-2">
          <span className="font-mono text-xs text-[#A4A7AD]">{PROJECT}</span>
        </div>
        <h2 className="text-lg font-medium text-[#F3F3F4] leading-none">{PROMPT}</h2>
        <div className="flex flex-wrap gap-2">
          {FILES.map((file) => (
            <button
              key={file}
              type="button"
              aria-label={`Open ${file}`}
              onClick={() => setOpenFile((current) => (current === file ? null : file))}
              className={`rounded-md border border-transparent px-3 py-1.5 text-xs font-mono transition-colors duration-200 ${
                openFile === file
                  ? 'bg-[#18191C] border-[#9AB8C8] text-[#F3F3F4]'
                  : 'text-[#A4A7AD] hover:text-[#F3F3F4] hover:border-[#6D727A]/50'
              }`}
            >
              {file}
            </button>
          ))}
        </div>
        <div className="flex items-center gap-2">
          <span className="text-xs text-[#A4A7AD]">Command:</span>
          <span className="font-mono text-xs text-[#F3F3F4]">{COMMAND}</span>
        </div>

        <div className="flex flex-col lg:flex-row gap-6">
          <ActivitySpine
            nodes={spineNodes.map((node) => ({
              ...node,
              status: spineStatus(state)[node.id],
            }))}
            activeId={spineTarget[state]}
            ariaLabel="Demo walkthrough Activity Spine"
            onSelect={onSpineSelect}
          />
          <div className="flex-grow">
            <div className="demo-panel" key={state}>
              <div className="demo-slide" data-state={state} key={state}>
                {panelContent(state)}
                {state === 'awaitingApproval' && (
                  <div className="mt-6 flex gap-3">
                    <button type="button" className="demo-cta" onClick={onAllow}>
                      Allow
                    </button>
                  </div>
                )}
                {state === 'verifying' && !manual && (
                  <p className="mt-2 text-xs text-[#A4A7AD]">Running tests…</p>
                )}
                {state === 'verifying' && manual && (
                  <div className="mt-6 flex gap-3">
                    <button type="button" className="demo-btn" onClick={onRunTests}>
                      Run tests
                    </button>
                  </div>
                )}
                {state === 'finished' && (
                  <div className="mt-6 flex gap-3">
                    <button type="button" className="demo-btn" onClick={onReplay}>
                      Replay
                    </button>
                  </div>
                )}
              </div>
            </div>
          </div>
        </div>

        {openFile !== null && (
          <div className="mt-6 border border-[#303238] bg-[#18191C] rounded-md overflow-hidden">
            <div className="border-b border-[#303238] bg-[#18191C]/50 px-3 py-2 text-xs font-mono text-[#A4A7AD]">
              {openFile} - read-only excerpt from the demo run
            </div>
            <pre className="p-3 text-xs font-mono text-[#F3F3F4] leading-relaxed">
              {openFile === 'src/auth.ts'
                ? 'export function validateCredentials(user: string, password: string): boolean {\n  const record = users.find((u) => u.name === user)\n  if (!record) return false\n  return record.password === password\n}'
                : 'test("rejects wrong password", () => {\n  expect(validateCredentials("alice", "wrong")).toBe(false)\n})'}
            </pre>
          </div>
        )}
      </div>
    </section>
  )
}
