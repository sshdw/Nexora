# Follow-ups proof frames — D-08 6px grid + clickable links (02 §4.2.3)

Real Tauri dev build (`npm run tauri dev`), WebView2 CDP client-area captures,
branch `task/2.0-followups` at the follow-ups commit (on main `e17bc0d` after #318).

| File | CSS viewport | PNG | MD5 | Colours | What I saw |
|---|---|---|---|---|---|
| `f1-composer-1200x800.png` | 1200×800 | 1200×800 | `56324A46BDF5C4C8BF14EC58FF6D44FF` | 4974 | Composer card after the D-08 normalization: spacing on the 6px grid, no off-grid odd paddings; dock, thread, workspace row intact. |
| `f2-link-affordance-1200x800.png` | 1200×800 | 1200×800 | `C8EBC8F4CBE4639B42706ECF1F70234E` | 4888 | A real agent-authored markdown link in the thread with its affordance row: **Copy link + Open** (`nex-md-copy` styled buttons) — zero `<a href>` elements with model-authored URLs (DOM probe: `anchors: []`). |
| `f2b-link-open-hover-1200x800.png` | 1200×800 | 1200×800 | `4745F47B650CAA07FAC27843192A0ADA` | 4904 | The Open affordance under pointer hover (real mouseMoved): hover tone applied; bytes differ from f2 — distinct render. |
| `f3-security-negative-1200x800.png` | 1200×800 | 1200×800 | `520C24F7E3B7F1E5F054E17E4FFD0FFC` | 4894 | Security negative case: the model refused to emit a `javascript:` link ("it's a javascript: URL which can be an XSS vector") — and the backend validator tests reject the scheme table regardless of model behaviour (see PR body). |

All frames distinct (unique MD5), ≥4888 colours. The negative-case backend coverage is
deterministic: unit tests in the PR reject javascript:/data:/vbscript:/file:/ftp:/
relative/userinfo/oversize at the `open_external_url` sink.
