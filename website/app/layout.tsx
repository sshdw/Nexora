import type { Metadata } from 'next'

import Navbar from '@/components/Navbar'
import Footer from '@/components/Footer'

import './globals.css'

export const metadata: Metadata = {
  title: {
    template: '%s | Nexora',
    default: 'Nexora',
  },
  description:
    'Nexora — a free, open-source, local-first AI coding workspace with visible agent actions, approvals, and reviewable changes.',
  metadataBase: new URL(process.env.SITE_URL ?? 'https://prismdev.duckdns.org'),
  alternates: {
    canonical: '/',
  },
  openGraph: {
    type: 'website',
    url: '/',
    title: 'Nexora',
    description:
      'A free, open-source AI coding workspace with visible agent actions, approvals, and reviewable changes.',
    siteName: 'Nexora',
  },
  twitter: {
    card: 'summary_large_image',
    title: 'Nexora',
    description:
      'A free, open-source AI coding workspace with visible agent actions, approvals, and reviewable changes.',
  },
  robots: {
    index: true,
    follow: true,
  },
}

// Inline theme initializer: applies the saved theme (or system preference) as
// early as possible, before React hydrates, to avoid a theme flash.
const THEME_INIT = `(function(){try{var e=document.documentElement,t=localStorage.getItem('nexora-theme');var n=t==='system'||!t;if(n){var r=window.matchMedia('(prefers-color-scheme: dark)');e.classList.toggle('dark',r.matches);e.dataset.theme=r.matches?'dark':'light'}else{e.classList.toggle('dark',t==='dark');e.dataset.theme=t}}catch(e){}})();`

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <head>
        <script id="nexora-theme-init" dangerouslySetInnerHTML={{ __html: THEME_INIT }} />
        <link rel="icon" href="/favicon.svg" type="image/svg+xml" />
      </head>
      <body className="font-sans antialiased selection:bg-accent selection:text-surface">
        <Navbar />
        {children}
        <Footer />
      </body>
    </html>
  )
}
