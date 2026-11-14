# M04 access contracts — native Windows proof

Source: `b240896a5d45c5bcaafcd1ff07e3a6df6f478169`. Captured November14,2026 from an actual Tauri debug build, not a browser mock. This proof commit contains index/frames/receipts; product source unchanged after gate.

Production executable SHA256: `44EE28B3781C0EE5452386A51A4387E4E5B1CFC4A0907340D5E06428A8D96906`.
QA executable SHA256: `2B749719102858CADD62CF75B75865721B498575B225274C5E716D6129DB4A4F`.

Local-only override changes Tauri identifier to `com.nexora.qa.m04.rf6c9037f3c` and window title to `Nexora M04 QA` (maximized). Production tauri.conf.json/permission behavior unchanged. Separate synthetic APPDATA/LOCALAPPDATA and shared synthetic WebView2 only; provider rows0, no provider requests/personal keyring or database access. All five saved production artifacts were restored and hashes verified afterward.

Four final launches exited0 with orderly shutdown; running work0, schema16, integrity/FK clean, unchanged run/step/journal rows1/1/3. Earlier capture attempts were interrupted and restarted; these four final controlled launches supply the receipts. Profile values were seeded only while closed; security settings were not changed through UI automation.

| Frame | MD5 | Colors | What is shown |
|---|---|---:|---|
| [01-standard-composer-en.png](01-standard-composer-en.png) | `fb91e89f517a021187817395bc8070d0` | 13908 | Standard Composer, EN, retained run92001 |
| [02-standard-settings-en.png](02-standard-settings-en.png) | `52125c5edffa756a509202036e52e26b` | 22962 | Standard Settings, EN, checkpoint and shell consent fallback |
| [03-standard-settings-ru.png](03-standard-settings-ru.png) | `643c7546c866f4328f046551b3232b89` | 23853 | Standard Settings, RU, same capability explanation |
| [04-read-only-composer-ru.png](04-read-only-composer-ru.png) | `1d260a6eda519ce0368d2bd702b48add` | 14868 | ReadOnly Composer, RU, after restart |
| [05-read-only-settings-ru.png](05-read-only-settings-ru.png) | `e52cf21c75bb660bf6b23159dc9d5274` | 24057 | ReadOnly Settings, RU, edits and commands prohibited |
| [06-supervised-composer-ru.png](06-supervised-composer-ru.png) | `76ee33a892a46c9136b587668f96db93` | 14935 | Supervised Composer, RU, after restart |
| [07-supervised-settings-ru.png](07-supervised-settings-ru.png) | `cc78e0bda3dd07ac460bb7e3abbbe143` | 24091 | Supervised Settings, RU, per-edit/command consent |
| [08-invalid-fallback-composer-ru.png](08-invalid-fallback-composer-ru.png) | `f146615a6fe5aba40c55cb048cafbe4a` | 15089 | Invalid stored value: ReadOnly fallback in Composer, RU |

Original1536×816 main-window captures only, unique md5/>900 colors. Tiny auxiliary cursor/tooltip surfaces excluded. The purple cursor marker is an automation/OS pointer effect, not app styling. Invalid-profile Settings was also observed as ReadOnly; only its Composer fallback frame is included to avoid repeating the same Settings state. The invalid-profile synthetic conversation has a descriptive fixture title; retained run/step/journal rows remain unchanged.

Scope: Composer/Settings persisted profiles, EN/RU wording, truthful Standard checkpoint/per-command consent fallback, retained history, invalid-profile fail-closed display. Native pending approval actions/category/packages are unpictured/unassessed: no isolated provider credential route was used. Security execution is tested by backend/components and independent code review, not inferred from images. Profile keyboard/focus behavior was not manually exercised; component tests cover semantics. Native macOS/Linux acceptance remains later work.

Full source gate10/10 PASS: rootRust142, desktop1293+1 existing provider-env ignored, Vitest1091/57, both fmt/clippy, TypeScript/build/realTauri. Both-workspace advisories PASS. Details remain local under `.ai/validation/stage2-pr2a/`. Machine-readable proof: [native-receipt.json](native-receipt.json).
