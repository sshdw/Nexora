'use client'

import { useEffect, useState } from 'react'

export type OsPlatform = 'windows' | 'macos' | 'linux' | 'unknown'

/** Detect the desktop operating system from the user agent. */
export function detectOs(userAgent: string): OsPlatform {
  const ua = userAgent.toLowerCase()
  if (/windows/.test(ua)) {
    return 'windows'
  }
  if (/macintosh|mac os x/.test(ua)) {
    return 'macos'
  }
  if (/linux/.test(ua) && !/android/.test(ua)) {
    return 'linux'
  }
  return 'unknown'
}

/** Platform-aware download label for the primary CTA. */
export function downloadLabel(platform: OsPlatform): string {
  switch (platform) {
    case 'windows':
      return 'Download for Windows'
    case 'macos':
      return 'Download for macOS'
    case 'linux':
      return 'Build from source'
    default:
      return 'Download Nexora'
  }
}

interface OsDetectionProps {
  children: (platform: OsPlatform) => React.ReactNode
  /** Value rendered before the client detects the OS (server render). */
  fallback?: OsPlatform
}

/** Detects the OS on mount and passes it to children. Renders `fallback`
 *  on the server and before hydration to avoid layout shifts. */
export default function OsDetection({
  children,
  fallback = 'unknown',
}: OsDetectionProps) {
  const [platform, setPlatform] = useState<OsPlatform>(fallback)

  useEffect(() => {
    setPlatform(
      typeof navigator !== 'undefined' ? detectOs(navigator.userAgent) : fallback,
    )
  }, [])

  return <>{children(platform)}</>
}
