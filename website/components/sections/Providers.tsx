'use client'

import { useState } from 'react'

const providers = [
  { name: 'OpenAI', slug: 'openai' },
  { name: 'Anthropic', slug: 'anthropic' },
  { name: 'Gemini', slug: 'google-gemini' },
  { name: 'OpenRouter', slug: 'openrouter' },
  { name: 'NVIDIA NIM', slug: 'nvidia' },
  { name: 'OpenCode Zen', slug: 'opencode' },
]

function ProviderChip({ name, slug }: { name: string; slug: string }) {
  const [missing, setMissing] = useState(false)

  return (
    <div className="rounded-md border border-[#303238] bg-[#18191C] px-3 py-2">
      <div className="flex items-center gap-2">
        {slug && !missing && (
          <img
            src={`https://cdn.simpleicons.org/${slug}/white`}
            alt=""
            width="16"
            height="16"
            loading="lazy"
            decoding="async"
            onError={() => setMissing(true)}
          />
        )}
        <span className="font-mono text-xs text-[#A4A7AD]">{name}</span>
      </div>
    </div>
  )
}

export default function Providers() {
  return (
    <section className="py-12 md:py-16">
      <div className="mx-auto max-w-[1240px] px-5 md:px-6 lg:px-8">
        <div className="max-w-[760px] mx-auto text-center">
          <h2 className="text-[34px] md:text-[42px] lg:text-[48px] leading-none tracking-tight">
            Use your models, not ours.
          </h2>
          <p className="mt-4 text-base leading-relaxed text-muted">
            Connect OpenAI, Anthropic, Gemini, OpenRouter, NVIDIA NIM, OpenCode
            Zen, or your own OpenAI-compatible endpoint. Credentials are stored
            in the OS keyring.
          </p>
        </div>

        <div className="mt-10 lg:mt-14 flex flex-wrap justify-center gap-4">
          {providers.map((provider) => (
            <ProviderChip key={provider.name} {...provider} />
          ))}
          <div className="rounded-md border border-[#303238] bg-[#18191C] px-3 py-2">
            <span className="font-mono text-xs text-[#A4A7AD]">
              OpenAI-compatible endpoint
            </span>
          </div>
        </div>
      </div>
    </section>
  )
}
