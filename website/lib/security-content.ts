// Shared copy for /security. Single source of the verified security claims.
// Keep this file small; page structure lives in app/security/page.tsx.

export const securityDiagram = [
  { label: 'Workspace', goesTo: 'local SQLite' },
  { label: 'Credentials', goesTo: 'OS keyring' },
  { label: 'Requests', goesTo: 'provider you choose' },
] as const

export const knownLimitations = [
  'Selected AI providers receive the prompts and context you send to them.',
  'Shell execution is not a sandbox.',
  "Nexora can't guarantee the security or privacy of third-party providers.",
  'Full-autonomous mode may execute mutating actions without per-action approval where current policy permits it.',
  'Dangerous shell execution still requires approval.',
  'Open source does not equal an independent security audit.',
] as const
