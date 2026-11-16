# Stage8 PR8A — real Tauri desktop proof

Final product source: `bea3c98d163374fb6b57defdee462e66681226d7`. Frames03–13 are from the separately compiled real Tauri executable, not a browser/mock backend. Frames01–02 use the supplied pre-change production binary backed up before implementation.

Final QA uses a unique app identifier (`com.nexora.qa.stage8a.20261116`), redirected synthetic APPDATA/LOCALAPPDATA/TEMP/TMP/WebView profile, and an explicit synthetic workspace. Baseline DB was redirected too, but its Tauri fallback workspace tooltip used the normal app-data location; baseline frames do not establish filesystem isolation. No run or provider request was submitted in that baseline.

Historical messages/Missing reference rows were seeded synthetic fixtures; they do not claim a live provider response. Native file selection, managed-copy creation, hash verification, mode switching, admission errors, copy removal and keyboard focus were exercised against the real backend. No provider/keyring connection or live external model request was used. Provider serialization and draft-rejection behavior are covered by automated tests. Static frames do not establish animation/reduced-motion behavior.

Normal geometry1536x816; explicitly narrower frames12–13 use1026x824. Every raw JPEG has a unique MD5 and >900 RGB colors. The purple pointer halo is visible in native dialogs as well and is not part of the app styling.

Observed final shutdown: exit0. SQLite schema19 integrity_check=ok, foreign_key_check clean, providers0/running0. All original fixture SHA256 hashes match; all5 production artifact SHA256 hashes match restored backups. Local receipts are under `.ai/validation/stage8-pr8a/native/` (not committed).

| Frame | MD5 | RGB colors | What is shown |
|---|---|---:|---|
| [01-baseline-empty.jpg](01-baseline-empty.jpg) | `94dd2db84b547d9dc9d2afc825296172` | 5511 | Pre-change supplied production binary; empty synthetic database. |
| [02-baseline-agent.jpg](02-baseline-agent.jpg) | `6524ab8c34af2bc61120ce071d753880` | 9882 | Pre-change Agent composer; no selected model or provider. |
| [03-final-legacy-missing.jpg](03-final-legacy-missing.jpg) | `49e6afe8fc26bd0050e9687bf140dadf` | 18387 | Final build: seeded legacy history and draft show Missing; original existing legacy file remains available. |
| [04-final-agent-attach-menu.jpg](04-final-agent-attach-menu.jpg) | `32634daae84afe2d434c5169068efe28` | 10267 | Final Agent mode: Attach files action is available. |
| [05-final-agent-files.jpg](05-final-agent-files.jpg) | `919c37056548100afe87ba0d65501da4` | 13323 | TXT and PNG selected through the real native file picker; both managed draft copies appear in Agent. |
| [06-final-chat-files.jpg](06-final-chat-files.jpg) | `081e163da4c996b02c71f395d2e1dbf7` | 13118 | Switching to Chat retains both draft attachments. |
| [07-final-oversize-draft-retained.jpg](07-final-oversize-draft-retained.jpg) | `2816e24cc706ffb8b295824590120cb0` | 18961 | 20 MiB + 1 byte rejected; existing files and typed synthetic Russian text remain. |
| [08-final-sensitive-path-draft-retained.jpg](08-final-sensitive-path-draft-retained.jpg) | `4bc564c80b9dd202083220c36b6245f9` | 19167 | Synthetic .env path rejected; existing files and typed text remain. |
| [09-final-managed-copy-removed.jpg](09-final-managed-copy-removed.jpg) | `f8bbd29a68f76f83855fb060a48a39dc` | 15594 | After removing PNG, TXT and typed text remain; disk/DB receipts confirm only the PNG managed copy was removed. |
| [10-final-ru-agent-draft.jpg](10-final-ru-agent-draft.jpg) | `93ff8f84d075725731c23698936bcd46` | 13329 | Russian interface after Settings round-trip; TXT and typed draft retained. |
| [11-final-ru-legacy-missing.jpg](11-final-ru-legacy-missing.jpg) | `617eab08cac86d39c22e3d79814947bc` | 20874 | Russian historical and draft Нет файла indicators. |
| [12-final-ru-narrow-missing.jpg](12-final-ru-narrow-missing.jpg) | `19918d73c179fb39af5603f609948a00` | 20527 | Russian Missing state at1026x824 window geometry. |
| [13-final-keyboard-remove-focus.jpg](13-final-keyboard-remove-focus.jpg) | `6628982e83454adea8aa71c050273299` | 20681 | Shift+Tab reaches the attachment removal button with visible focus ring and Russian tooltip. |
