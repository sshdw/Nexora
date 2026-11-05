/** @type {import('next').NextConfig} */
const nextConfig = {
  output: 'export',
  eslint: {
    ignoreDuringBuilds: true,
  },
  distDir: '.next',
  trailingSlash: true,
  images: {
    unoptimized: true,
  },
}

export default nextConfig
