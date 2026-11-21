# Stage 11 PR 11A native QA frames

Source `686576c8937e4dcd06261d620be2320fe0f95240`. Separate compiled Windows Tauri application (`nexora.exe`) with isolated synthetic `APPDATA`/`LOCALAPPDATA`/`TEMP`, cold startup `970 ms`, and SQLite `PRAGMA integrity_check=ok`, `foreign_key_check=[]` at `1202 × 831`.

| Frame | MD5 | Colors | Observation |
| --- | --- | ---: | --- |
| [01-empty-state-native-primitives.jpg](01-empty-state-native-primitives.jpg) | `99a51b6152c3729eb9faa0c4f32ba595` | 4432 | Empty state with Dock RailItem/IconButton and Toolbar native primitives at 1202×831 (cold startup 970 ms). |
| [02-settings-native-primitives.jpg](02-settings-native-primitives.jpg) | `4bb5107382e3989eada974350c5d081e` | 19442 | Settings view with native Button, SegmentedGroup, and RailItem primitives in dark monochrome theme. |
