'use client'

import OsDetection from '@/components/OsDetection'
import { downloadLabel } from '@/components/OsDetection'

/** Platform-aware primary download link. Renders "Download Nexora" on the
 *  server and swaps to the detected OS label after hydration. */
export default function DownloadButton() {
  return (
    <OsDetection>
      {(platform) => (
        <a
          href="/download"
          className="demo-cta"
          suppressHydrationWarning
        >
          {downloadLabel(platform)}
        </a>
      )}
    </OsDetection>
  )
}
