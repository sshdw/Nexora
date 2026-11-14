# Stage 5 PR 5A: real desktop proof

Raw full-window captures of the real Tauri debug build, with production frontend assets.
Baseline source: `164049070d193aebe14d802e435043c3ed11957f`.
Final source: `757467534516297efaa563a8696cab8f8db1d445`.
Baseline binary SHA256: `99295C470BCBF141CFC77126258172A2839011C4C7ECC9323F43A5B1F1164F55`.
Final binary SHA256: `3A43351FFE29472AB32FDD2FA194F2C3B173D305769503C2A6237317ABCB38C2`.

QA-only application identifiers/window title and isolated synthetic APPDATA,
LOCALAPPDATA and WebView storage were used. No personal database, credentials or
provider requests were used. Synthetic fixture has three conversations, no
messages/providers, and one retained completed run with one read step and three
durable events. The purple cursor glow belongs to the automation/OS pointer.
Frames are original PNGs, without crops, redraws or mock content. Geometry is
recorded per frame. Integrity counts use decoded RGB colors and raw-file MD5.

The index describes observed UI only. A target modal proves routing, not file
creation. A separate native export from source `e6eacc61` was manually approved
by the owner; its saved JSON contains conversation ID 92002 and the expected full
secondary title. This precedes the pointer/focus-only fix in `75746753`; file IO
was not repeated after that fix. Archived/palette routing was captured on the
final source; their native file writes were not performed. See receipts.json.
Save-dialog and permission screenshots are excluded from the visual proof.
Streaming, provider errors, motion/reduced-motion, live approval execution and
unpictured states are unassessed. New run/Composer presentation belongs to PR 5B.
Alt+S opened and closed Split in the final native build; Ctrl+K, Export query and
Enter opened the active primary export modal after Sessions was dismissed.
Keyboard layout was toggled twice, returning to its original state.
Title full-text attributes and exact focus return are covered by automated
integration tests; the UI Automation focused-element field was inconclusive.
The QA processes were intentionally stopped after capture (the earlier close
request did not exit); orderly final shutdown is not claimed. SQLite integrity,
foreign keys and business-row parity passed afterward. All five production
artifacts were restored and SHA256-verified. No provider or send action occurred.

## Frame index

| Frame | Dimensions | RGB colors | MD5 | What is shown |
| --- | --- | ---: | --- | --- |
| [00-before-empty.png](00-before-empty.png) | 1536×816 | 5602 | `d87794bcfb627a8608442b232ec59a33` | Baseline empty shell; top strip is absent. |
| [01-before-sessions-actions.png](01-before-sessions-actions.png) | 1536×816 | 6765 | `5a20c4da6adf9f11cd7e15ced3d73700` | Baseline Sessions row action placement. |
| [02-before-single-header.png](02-before-single-header.png) | 1536×816 | 13989 | `b97d8f2b7c23b3512e1e3bf012f1efc4` | Baseline standalone conversation header and duplicated workspace. |
| [03-before-multiple-tabs.png](03-before-multiple-tabs.png) | 1536×816 | 10830 | `91ba48ce17afd4c7af24b79d54f6cdc9` | Baseline multiple tabs and workspace chip in the tab strip. |
| [04-before-split-headers.png](04-before-split-headers.png) | 1536×816 | 24921 | `2ab5b774a05c8b8e65d433369e0aef03` | Baseline split panes with standalone headers. |
| [10-after-single-session.png](10-after-single-session.png) | 1536×816 | 8779 | `456cff4df0e0ce7aa3785b5edfc9b973` | One long-title session; trailing Split remains visible and disabled. |
| [11-after-empty-stable-split.png](11-after-empty-stable-split.png) | 1536×816 | 1668 | `65fc130ae8c0711dfcbcc9b5a77dff22` | Zero open sessions; Split retains its trailing strip position. |
| [12-after-loaded-primary.png](12-after-loaded-primary.png) | 1536×816 | 13238 | `4f912411cc954a7245ac0198f7504cdb` | Loaded synthetic retained run; standalone pane header is removed. |
| [13-after-sessions-export-actions.png](13-after-sessions-export-actions.png) | 1536×816 | 15679 | `c6c9ab8e27bcdb3a77c3789d028b6b0e` | Focused Sessions row exposes Rename, Archive, Export and Delete actions. |
| [14-after-inactive-export-target.png](14-after-inactive-export-target.png) | 1536×816 | 14730 | `ff0d65b800e051070bf0949485e996fa` | Export names the inactive secondary session while primary stays selected. |
| [15-after-archived-export-target.png](15-after-archived-export-target.png) | 1536×816 | 14872 | `27b9e509ae2898d5f8c8ec864cb59a0b` | Archived row Export names the archived session while primary stays selected. |
| [16-after-palette-export.png](16-after-palette-export.png) | 1536×816 | 14683 | `3b3fef33cb8749372e8d2f0d490b5dca` | Existing command palette Export opens the current primary session target. |
| [17-after-multiple-tabs.png](17-after-multiple-tabs.png) | 1536×816 | 14493 | `5b7707f8b6ef33ebeed6b5b3b563c719` | Two open sessions share one stable trailing Split control without a workspace chip. |
| [18-after-split-identities.png](18-after-split-identities.png) | 1536×816 | 17704 | `0f2ce89ed61adb219426dfd26a7552d5` | Split panes show compact individual title badges and independent content. |
| [19-after-narrow-split.png](19-after-narrow-split.png) | 1026×751 | 17981 | `51bc435c3191edf7a3a4d418a18c3236` | Narrow restored window retains compact split identities and a trailing Close split control. |
| [20-after-ru-single.png](20-after-ru-single.png) | 1026×751 | 14517 | `73adcae9e4cb5623e142845e991832f2` | Russian shell labels at narrow width retain the permanent split control. |
| [21-after-export-focus-return.png](21-after-export-focus-return.png) | 1536×816 | 14559 | `c3ff9ccfa10be0be52883573e7faf03f` | After pointer cancellation, Sessions and its exact secondary-row Export invoker remain mounted; focus restoration is covered by integration tests. |
| [22-after-palette-command.png](22-after-palette-command.png) | 1536×816 | 3282 | `3b8b9f7bf4b641daf76930621a3f2e2d` | Keyboard-filtered palette exposes the existing Export active conversation command. |
| [23-after-independent-drafts.png](23-after-independent-drafts.png) | 1536×816 | 20029 | `e08c898922a3b5d55182e691be6c8e14` | Two distinct unsent synthetic drafts remain visible in their respective split panes. |
| [24-after-ru-archived-split.png](24-after-ru-archived-split.png) | 1026×751 | 20441 | `2082bd157640de462b5d88f10d52e31d` | Narrow Russian Split identifies the archived pane with В архиве and retains primary content/draft. |
