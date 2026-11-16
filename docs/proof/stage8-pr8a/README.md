# Stage8 PR8A — real Tauri desktop proof

Latest product source: `0ce0afdebc3a2fbe314101a888178ca76f4ac419`. Frames14–17 use its separately compiled real Tauri executable. Frames03–13 preserve the earlier UI/runtime proof on `bea3c98d163374fb6b57defdee462e66681226d7`; all UI source files remain byte-identical. Frames01–02 use the supplied pre-change production binary. The manifest records each frame source. No frame is a browser/mock-backend capture.

Final QA uses unique app identifier `com.nexora.qa.stage8a.20261116`, redirected synthetic APPDATA/LOCALAPPDATA/TEMP/TMP/WebView profile,and explicit synthetic workspace. Baseline DB was redirected too,but its Tauri fallback workspace tooltip used the normal app-data location;baseline frames do not establish filesystem isolation. No run/provider request was submitted in that baseline.

Historical message/Missing rows are seeded fixtures,not a live provider response. Native file selection,managed-copy creation,SHA verification,mode switching,admission errors,copy removal,RU/narrow/keyboard focus were exercised with the real backend. No provider/keyring connection or live external model request was used. Static frames do not establish animation/reduced-motion behavior. Actual independent-process delete/clear coordination is covered by compiled Rust service regressions,not claimed as two visible Tauri windows.

Normal geometry1536x816;frames12–14 are1026x824. Every rawJPEG has uniqueMD5 and>900RGBcolors. Purple pointer halo is also visible in nativeOSdialogs;it is not app styling.

Latest native shutdown exit0;SQLite schema19 integrity_check=ok,foreign_key_check clean,providers0/running0. Original fixtureSHA256 unchanged;all5productionartifactSHA256 match restored backups. Latest admission verified3managedcopies;PNG removal retainedTXT+Rust and stable database-adjacentlock. Local receipts `.ai/validation/stage8-pr8a/native/` are not committed.

| Frame | Source | MD5 | RGB colors | What is shown |
|---|---|---|---:|---|
| [01-baseline-empty.jpg](01-baseline-empty.jpg) | `supplied` | `94dd2db84b547d9dc9d2afc825296172` | 5511 | Pre-change supplied production binary; empty synthetic database. |
| [02-baseline-agent.jpg](02-baseline-agent.jpg) | `supplied` | `6524ab8c34af2bc61120ce071d753880` | 9882 | Pre-change Agent composer; no selected model or provider. |
| [03-final-legacy-missing.jpg](03-final-legacy-missing.jpg) | `bea3c98d` | `49e6afe8fc26bd0050e9687bf140dadf` | 18387 | Final build: seeded legacy history and draft show Missing; original existing legacy file remains available. |
| [04-final-agent-attach-menu.jpg](04-final-agent-attach-menu.jpg) | `bea3c98d` | `32634daae84afe2d434c5169068efe28` | 10267 | Final Agent mode: Attach files action is available. |
| [05-final-agent-files.jpg](05-final-agent-files.jpg) | `bea3c98d` | `919c37056548100afe87ba0d65501da4` | 13323 | TXT and PNG selected through the real native file picker; both managed draft copies appear in Agent. |
| [06-final-chat-files.jpg](06-final-chat-files.jpg) | `bea3c98d` | `081e163da4c996b02c71f395d2e1dbf7` | 13118 | Switching to Chat retains both draft attachments. |
| [07-final-oversize-draft-retained.jpg](07-final-oversize-draft-retained.jpg) | `bea3c98d` | `2816e24cc706ffb8b295824590120cb0` | 18961 | 20 MiB + 1 byte rejected; existing files and typed synthetic Russian text remain. |
| [08-final-sensitive-path-draft-retained.jpg](08-final-sensitive-path-draft-retained.jpg) | `bea3c98d` | `4bc564c80b9dd202083220c36b6245f9` | 19167 | Synthetic .env path rejected; existing files and typed text remain. |
| [09-final-managed-copy-removed.jpg](09-final-managed-copy-removed.jpg) | `bea3c98d` | `f8bbd29a68f76f83855fb060a48a39dc` | 15594 | After removing PNG, TXT and typed text remain; disk/DB receipts confirm only the PNG managed copy was removed. |
| [10-final-ru-agent-draft.jpg](10-final-ru-agent-draft.jpg) | `bea3c98d` | `93ff8f84d075725731c23698936bcd46` | 13329 | Russian interface after Settings round-trip; TXT and typed draft retained. |
| [11-final-ru-legacy-missing.jpg](11-final-ru-legacy-missing.jpg) | `bea3c98d` | `617eab08cac86d39c22e3d79814947bc` | 20874 | Russian historical and draft ֽוע פאיכא indicators. |
| [12-final-ru-narrow-missing.jpg](12-final-ru-narrow-missing.jpg) | `bea3c98d` | `19918d73c179fb39af5603f609948a00` | 20527 | Russian Missing state at1026x824 window geometry. |
| [13-final-keyboard-remove-focus.jpg](13-final-keyboard-remove-focus.jpg) | `bea3c98d` | `6628982e83454adea8aa71c050273299` | 20681 | Shift+Tab reaches the attachment removal button with visible focus ring and Russian tooltip. |
| [14-current-ru-missing.jpg](14-current-ru-missing.jpg) | `0ce0afde` | `3040f8a37d3b2bd84c4f1d73404fe2b6` | 19649 | Current backend build: Russian Missing state in a1026x824 window. |
| [15-current-agent-code-photo.jpg](15-current-agent-code-photo.jpg) | `0ce0afde` | `1cac27c15fc6638d674fcf4291f88204` | 15230 | Current backend build: existing TXT plus newly admitted Rust code and PNG in Agent; managed SHA256 verified. |
| [16-current-chat-files.jpg](16-current-chat-files.jpg) | `0ce0afde` | `cd17bc058ffe9a91ded8db7fafaa27aa` | 14354 | Current backend build: all three files retained when switching to Chat. |
| [17-current-copy-removed.jpg](17-current-copy-removed.jpg) | `0ce0afde` | `fda57947792e33f4fab9ee3b746c32f0` | 13567 | Current backend build: after PNG removal,TXT and Rust copies/rows survive; original SHA unchanged and stable lock retained. |
