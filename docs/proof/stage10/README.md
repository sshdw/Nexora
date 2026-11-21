# Stage 10 native QA frames

Source `06fd395c204a01634ed1286085e364a0ff909542`. Separate compiled Windows Tauri application (`nexora.exe`) with isolated synthetic `APPDATA`/`LOCALAPPDATA`/`TEMP`, synthetic Git merge conflict (`UU`), and SQLite `PRAGMA integrity_check=ok`, `foreign_key_check=[]` at `1202 × 831`.

| Frame | MD5 | Colors | Observation |
| --- | --- | ---: | --- |
| [01-vcs-conflicts-en.jpg](01-vcs-conflicts-en.jpg) | `4c0dd0530cd9ff553eff971fb0aa74dc` | 19595 | Version Control panel showing Conflicted files (UU README.md), Merge/pull in progress badge, Abort merge/pull, Remote & sync (Fetch/Pull/Push), and Branches. |
| [02-vcs-conflict-preview-en.jpg](02-vcs-conflict-preview-en.jpg) | `2fbe77672eb7699e488346f9ec2eec54` | 21661 | Conflict preview for README.md showing Ours (current branch) and Theirs (incoming) panes alongside Accept Ours / Accept Theirs / Ask Agent to Resolve. |
