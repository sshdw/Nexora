import Link from 'next/link'

const GITHUB_URL = 'https://github.com/sshdw/Nexora'

export default function Footer() {
  return (
    <footer className="border-t border-border bg-canvas">
      <div className="mx-auto max-w-[1240px] px-5 md:px-6 lg:px-8">
        <div className="py-10 md:py-14 flex flex-col md:flex-row gap-8 md:gap-0 md:items-start justify-between">
          <div className="flex flex-col gap-1">
            <a href="/" className="font-semibold text-text">
              Nexora
            </a>
            <p className="text-xs text-muted">
              MIT License &copy; 2026 Nexora
            </p>
          </div>

          <nav className="flex flex-col sm:flex-row gap-2 sm:gap-4 text-sm text-muted" aria-label="Footer">
            <Link href="/" className="hover:text-accent transition-colors duration-200">
              Product
            </Link>
            <Link href="/security" className="hover:text-accent transition-colors duration-200">
              Security
            </Link>
            <Link href="/docs" className="hover:text-accent transition-colors duration-200">
              Docs
            </Link>
            <Link href="/changelog" className="hover:text-accent transition-colors duration-200">
              Changelog
            </Link>
          </nav>

          <div className="flex flex-col sm:flex-row gap-2 sm:gap-4 text-sm text-muted">
            <a
              href={GITHUB_URL}
              target="_blank"
              rel="noopener"
              className="hover:text-accent transition-colors duration-200"
            >
              GitHub
            </a>
            <Link href="/download" className="hover:text-accent transition-colors duration-200">
              Download
            </Link>
          </div>
        </div>
      </div>
    </footer>
  )
}
