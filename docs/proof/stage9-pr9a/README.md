# Stage 9A native QA frames

Source `4a1af173b96d8420733d724c7442b3889ccf73ec` (plus `01..03, 05..06, 10` unchanged from `5508ca8c6395041cc85bfd9dab3f624104e9663b`). Separate compiled Windows Tauri application with a synthetic profile and workspace. No provider configured. The frames document the English UI at a fixed 1202 × 831 viewport. Provider serialization and Send/Continue interactions are covered by automated checks.

| Frame | MD5 | Colors | Observation |
| --- | --- | ---: | --- |
| [01-exact-slash-match.jpg](01-exact-slash-match.jpg) | `6b80d630276592b94ca7a0f38a4fba96` | 10267 | Exact project /read ranks above the /review description match. |
| [02-pinned-project-skill.jpg](02-pinned-project-skill.jpg) | `438240fbac0722910aa767ae75064710` | 9435 | Enter opens /read with its path, Git revision and SHA-256. Include is disabled before trust. |
| [03-trusted-project-skill.jpg](03-trusted-project-skill.jpg) | `293b12d62e7fe4002e5bdbcae6e817eb` | 10399 | The trusted skill offers explicit inclusion and opt-in suggestions. |
| [04-staged-skill.jpg](04-staged-skill.jpg) | `2c827d37e464670c0c82c28e39567a63` | 10031 | Agent draft shows the selected /read skill and the no-shell warning. |
| [05-cleared-draft.jpg](05-cleared-draft.jpg) | `82954480eb3b17a86e39d37ad401b83c` | 10283 | /clear removes the command text and the selected skill. |
| [06-file-preview.jpg](06-file-preview.jpg) | `f53105d18ccf71f59a6ded9ac7b107f2` | 9710 | @file previews the synthetic Rust source. |
| [07-git-diff-preview.jpg](07-git-diff-preview.jpg) | `4e1b4b491151ad2e5ab649fc3f55db6b` | 8479 | @diff previews a tracked change from HEAD to the working tree with +/- hunk lines and unclipped shell warning. |
| [08-folder-preview.jpg](08-folder-preview.jpg) | `f1d9deef62dae5cb627a1d1e768bf239` | 8617 | @folder previews sample/example.rs, omits the synthetic .env fixture, and keeps shell warning unclipped. |
| [09-symbol-preview.jpg](09-symbol-preview.jpg) | `37725f6e769e86bb38e02c7b5229af6f` | 8627 | @symbol previews the exact name with its line number and keeps shell warning unclipped. |
| [10-keyboard-scroll.jpg](10-keyboard-scroll.jpg) | `8d5178b1e376d48f3ed702075a91292d` | 10396 | Arrow Up wraps to /read and scrolls the highlighted row into view. |
