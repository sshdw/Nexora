import React from 'react'

import { knownLimitations, securityDiagram } from '@/lib/security-content'

export const metadata = {
  title: 'Security model',
  description:
    'Nexora security model: local-first workspace, OS keyring credentials, provider requests from the application backend, approval gates, application hardening, and known limitations.',
  openGraph: {
    url: '/security',
  },
}

export default function Page() {
  return (
    <main className="min-h-[100dvh] bg-canvas text-text font-sans">
      <div className="mx-auto max-w-[1240px] px-5 md:px-6 lg:px-8">
        <section className="py-12 md:py-20">
          <div className="max-w-[760px]">
            <h1 className="text-[42px] md:text-[54px] lg:text-[68px] leading-none tracking-tight">
              Security model
            </h1>
            <p className="mt-6 text-base leading-relaxed text-muted">
              Your workspace stays local. Requests go to the providers you
              choose. Credentials are stored in the OS keyring, not Nexora's
              database. This page describes how Nexora handles data, provider
              requests, permissions, shell execution, and what it cannot
              guarantee.
            </p>
          </div>
        </section>

        <section className="py-12 md:py-16 border-t border-border pt-[calc(1rem+1px)]">
          <div className="max-w-[760px]">
            <h2 className="text-lg font-semibold text-text">
              Technical diagram
            </h2>
            <p className="mt-2 text-sm text-muted">
              One restrained view of where information goes:
            </p>
            <div className="mt-6 space-y-3">
              {securityDiagram.map((row) => (
                <div key={row.label} className="flex items-center gap-2 text-sm">
                  <code className="font-mono text-accent shrink-0">
                    {row.label}
                  </code>
                  <span className="text-muted">&#8594;</span>
                  <code className="font-mono text-text">{row.goesTo}</code>
                </div>
              ))}
            </div>
          </div>
        </section>

        {sections.map((section) => (
          <section key={section.id} className="py-12 md:py-16">
            <div className="max-w-[760px]">
              <h2 id={section.id} className="text-lg font-semibold text-text">
                {section.title}
              </h2>
              <div className="mt-4 space-y-5">
                {section.blocks.map((block, i) => {
                  if (block.type === 'p') {
                    return (
                      <p key={i} className="text-base leading-relaxed">
                        {block.content}
                      </p>
                    )
                  }
                  if (block.type === 'ul') {
                    return (
                      <ul
                        key={i}
                        className="list-disc ml-6 space-y-2 text-base leading-relaxed"
                      >
                        {block.items.map((item, j) => (
                          <li key={j}>{item}</li>
                        ))}
                      </ul>
                    )
                  }
                  return null
                })}
              </div>
            </div>
          </section>
        ))}
      </div>
    </main>
  )
}

type Block =
  | { type: 'p'; content: React.ReactNode }
  | { type: 'ul'; items: Array<React.ReactNode> }

type Section = {
  id: string
  title: string
  blocks: Block[]
}

const sections: Section[] = [
  {
    id: 'security-overview',
    title: 'Security overview',
    blocks: [
      {
        type: 'p',
        content:
          'Nexora is a local-first AI coding workspace. Everything you write - conversations, prompts, attachments, and settings - lives in a single SQLite file on your disk. There is no mandatory Nexora account and no Nexora cloud sync. The network is touched only when you send a request to a provider you configured yourself, and that request is made from the application backend.',
      },
    ],
  },
  {
    id: 'what-stays-local',
    title: 'What stays local',
    blocks: [
      {
        type: 'ul',
        items: [
          <>
            Your workspace. Conversations, prompts, attachments, and settings
            live in a single portable SQLite file{' '}
            <code className="font-mono text-sm">nexora.db</code> in your
            per-user application-data directory. Copy it, back it up, or delete
            it.
          </>,
          'Offline by default. Browsing history, search, and settings changes need no connection. Startup never blocks on the network.',
          'Nothing is uploaded until you press send to a provider you chose.',
        ],
      },
    ],
  },
  {
    id: 'what-leaves-your-device',
    title: 'What leaves your device',
    blocks: [
      {
        type: 'p',
        content: 'Only what you explicitly send:',
      },
      {
        type: 'ul',
        items: [
          'The prompt and any attached context you include go to the selected AI provider.',
          'Provider API requests are made from the application backend, not from your browser.',
          'Provider responses return to your local workspace only.',
          'No credentials are sent to a Nexora server. Nexora does not operate a sync or credentials server.',
        ],
      },
    ],
  },
  {
    id: 'credentials',
    title: 'Credentials',
    blocks: [
      {
        type: 'p',
        content:
          'Credentials are stored in the OS keyring, not Nexora\'s database. API keys live in the operating system keyring only - they are never written to SQLite, logs, or error text. Provider requests are signed in the backend. A key that appears in SQLite, logs, or reports is itself a vulnerability.',
      },
    ],
  },
  {
    id: 'agent-permissions-and-approvals',
    title: 'Agent permissions & approvals',
    blocks: [
      {
        type: 'p',
        content:
          'Three autonomy levels - supervised, semi-autonomous (default), and full-autonomous. Approval gates park anything that needs a human decision. Shell commands always require your explicit approval unless you choose full-autonomous mode, where the agent runs to the end without stopping per-action. Every run exposes pause, resume, and cancel controls.',
      },
    ],
  },
  {
    id: 'shell-execution',
    title: 'Shell execution',
    blocks: [
      {
        type: 'p',
        content:
          'Shell execution is treated as dangerous. It is not a sandbox. Commands run on your machine with your user\'s permissions. Shell commands require explicit approval in supervised and semi-autonomous modes. In full-autonomous mode, mutating actions may run without per-action approval where current policy permits it. Dangerous shell execution still requires approval.',
      },
    ],
  },
  {
    id: 'prompt-injection-untrusted-content',
    title: 'Prompt injection / untrusted content',
    blocks: [
      {
        type: 'p',
        content:
          'Untrusted content can reach the agent through tool output, file contents, or provider responses. Treat tool results and file contents as untrusted input. Review diffs before accepting changes. Be cautious pasting content from untrusted sources into prompts - it can influence agent behavior, including tool calls.',
      },
    ],
  },
  {
    id: 'audit-and-rollback',
    title: 'Audit & rollback',
    blocks: [
      {
        type: 'p',
        content:
          'Every agent run leaves an append-only audit trail. Run snapshots, checkpoints, and rollback let you review and undo changes. A read-only run inspector shows what happened in each run, and a self-audit gate reviews actions before they execute where policy permits. Git integration - status, log, diff, and explain-diff - helps you review changes before committing.',
      },
    ],
  },
  {
    id: 'application-hardening',
    title: 'Application hardening',
    blocks: [
      {
        type: 'ul',
        items: [
          <>
            Rust <code className="font-mono text-sm">unsafe_code = "forbid"</code>.
          </>,
          'Open-source MIT code: inspect it yourself.',
          'Restrictive Content Security Policy (CSP) in tauri.conf.json.',
          'No remote crash upload.',
          'No conventional remote product telemetry - no telemetry upload anywhere.',
          'Error text carries no secret material.',
        ],
      },
    ],
  },
  {
    id: 'telemetry-and-diagnostics',
    title: 'Telemetry & diagnostics',
    blocks: [
      {
        type: 'p',
        content:
          'Nexora does not send conventional product telemetry. There is no remote crash upload and no telemetry upload anywhere. Diagnostics stay local. When you report an issue, you choose what to share - never include API keys, tokens, or message content.',
      },
    ],
  },
  {
    id: 'known-limitations',
    title: 'Known limitations',
    blocks: [
      {
        type: 'ul',
        items: [...knownLimitations],
      },
    ],
  },
  {
    id: 'threat-model',
    title: 'Threat model',
    blocks: [
      {
        type: 'p',
        content: 'In scope:',
      },
      {
        type: 'ul',
        items: [
          'Approval bypasses, sandbox escapes, and prompt-injection-to-tool-execution paths.',
          'Credentials appearing in SQLite, logs, or error text.',
          'Mutating shell or file actions running without intended approval.',
        ],
      },
      {
        type: 'p',
        content: 'Out of scope:',
      },
      {
        type: 'ul',
        items: [
          'Provider-side outages and billing (OpenAI, Anthropic, Gemini, OpenRouter, NVIDIA NIM, OpenCode Zen, and custom OpenAI-compatible endpoints).',
          'Compromise of your operating system or user account. If your OS is compromised, local keyring protection is limited.',
        ],
      },
    ],
  },
  {
    id: 'reporting-vulnerabilities',
    title: 'Reporting vulnerabilities',
    blocks: [
      {
        type: 'p',
        content:
          'Use GitHub &#8594; Security &#8594; Report a vulnerability (private vulnerability advisory). Do not open a public issue for anything that could leak data, bypass approvals or permissions, or exfiltrate credentials.',
      },
      {
        type: 'p',
        content:
          'Include: version + OS, what you did, what you expected, what happened, and redacted logs. Never include API keys, tokens, or message content.',
      },
    ],
  },
]
