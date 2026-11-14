# M04 access contracts — native Windows proof

Source: `f4001d725ee66e60d57b66ca83d36ad11e05c412`. Captured November14,2026 from an actual Tauri debug build, not a browser mock. This proof commit contains index/frames/receipts; product source unchanged after gate.

Production executable SHA256: `6D859D2C820437C65DEBF8FC33872EF281A358D6CE119CB1603904871E1FCF12`.
QA executable SHA256: `7553BA776224C2C21F9A3CFCFAB117EFDBB9621665CEE82C836DED6217287F8E`.

Local-only override changes Tauri identifier to `com.nexora.qa.m04.rf6c9037f3c` and window title to `Nexora M04 QA` (maximized). Production tauri.conf.json/permission behavior unchanged. Separate synthetic APPDATA/LOCALAPPDATA and shared synthetic WebView2 only; provider rows0, no provider requests/personal keyring or database access. All five saved production artifacts were restored and hashes verified afterward.

Four final launches exited0 with orderly shutdown; running work0, schema16, integrity/FK clean, unchanged run/step/journal rows1/1/3. An interrupted initial ReadOnly capture session was restarted; final successful repeat supplies the receipt. Profile values were seeded only while closed; security settings were not changed through UI automation.

| Frame | MD5 | Colors | What is shown |
|---|---|---:|---|
| [01-standard-composer-en.png](01-standard-composer-en.png) | `fc2966991d1875aa5e766d9c7952f966` | 13854 | Standard Composer, EN, retained run92001 |
| [02-standard-settings-en.png](02-standard-settings-en.png) | `f9b5cff671bccd580ae7f515c1ef0788` | 22993 | Standard Settings, EN, checkpoint and shell consent fallback |
| [03-standard-settings-ru.png](03-standard-settings-ru.png) | `643c7546c866f4328f046551b3232b89` | 23853 | Standard Settings, RU, same capability explanation |
| [04-read-only-composer-ru.png](04-read-only-composer-ru.png) | `22c7ee5c8e94fe32911ad2de1245385e` | 15706 | ReadOnly Composer, RU, after restart |
| [05-read-only-settings-ru.png](05-read-only-settings-ru.png) | `6837f5d07da5f361b1af935fa97315a8` | 24015 | ReadOnly Settings, RU, edits and commands prohibited |
| [06-supervised-composer-ru.png](06-supervised-composer-ru.png) | `0716474989080fd40d554ce103762420` | 14912 | Supervised Composer, RU, after restart |
| [07-supervised-settings-ru.png](07-supervised-settings-ru.png) | `260ae177f3a6161eb083844902fd1cff` | 24046 | Supervised Settings, RU, per-edit/command consent |
| [08-invalid-fallback-composer-ru.png](08-invalid-fallback-composer-ru.png) | `dc50637a91ae5dbcd64483845bf26dfd` | 14848 | Invalid stored value: ReadOnly fallback in Composer, RU |

Original1536×816 main-window captures only, unique md5/>900 colors. Tiny auxiliary cursor/tooltip surfaces excluded. The purple cursor marker is an automation/OS pointer effect, not app styling. Invalid-profile Settings was also observed as ReadOnly; its pixel-identical duplicate of frame05 was omitted, leaving eight unique frames.

Scope: Composer/Settings persisted profiles, EN/RU wording, truthful Standard checkpoint/per-command consent fallback, retained history, invalid-profile fail-closed display. Native pending approval actions/category/packages are unpictured/unassessed: no isolated provider credential route was used. Security execution is tested by backend/components and independent code review, not inferred from images. Profile keyboard/focus behavior was not manually exercised; component tests cover semantics. Native macOS/Linux acceptance remains later work.

Full source gate10/10 PASS: rootRust142, desktop1288+1 existing provider-env ignored, Vitest1091/57, both fmt/clippy, TypeScript/build/realTauri. Both-workspace advisories PASS. Details remain local under `.ai/validation/stage2-pr2a/`. Machine-readable proof: [native-receipt.json](native-receipt.json).
