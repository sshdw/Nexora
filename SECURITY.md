# Security Policy

## Supported versions

| Version | Supported |
| ------- | --------- |
| 1.5.x   | Yes |
| < 1.5   | No (upgrade first, then report) |
| 2.0 pre-release (`main`) | Yes — flag it as pre-release in the report |

## Reporting a vulnerability

Use **GitHub → Security → Report a vulnerability** (private vulnerability
advisory). Do not open a public issue for anything that could leak data,
bypass approvals/permissions, or exfiltrate credentials.

Include: version + OS, what you did, what you expected, what happened, and
redacted logs. Never include API keys, tokens, or message content.

## Scope notes

- API keys live in the OS keyring only — they must never appear in SQLite,
  logs, error text, or reports. A key that does is itself a vulnerability.
- The app executes shell commands and reads/writes files through agent runs:
  approval bypasses, sandbox escapes, and prompt-injection-to-tool-execution
  paths are in scope.
- Out of scope: provider-side (OpenAI/Anthropic/…) outages and billing.
