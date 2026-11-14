# Stage 5 PR 5B: Real Desktop Proof

Raw full-window captures of the real Tauri debug build (`nexora.exe`, source `0c519c0524c0909b5e4890760ae0fea70b1bd16d`) using isolated synthetic `APPDATA`, `LOCALAPPDATA`, and `WEBVIEW2_USER_DATA_FOLDER` (`0` providers, `0` external network requests).

## Frame Index

| Frame | Dimensions | RGB colors | MD5 | What is shown |
| --- | --- | ---: | --- | --- |
| [01-agent-composer-default.png](01-agent-composer-default.png) | 1440×860 | 6366 | `9e865e6beacf23fe8f94701df69364ec` | Agent mode Composer (`Chat`/`Agent`, `Plan`, `Access`, `ContextRing`, `ModelPicker`, `/`, `@`, `Send`) and completed run presentation. |
| [02-run-steps-expanded-diff-revert.png](02-run-steps-expanded-diff-revert.png) | 1440×860 | 6235 | `b76b8979e9db3b1542fa493ce47c0aa4` | Expanded compact run steps and per-file diff disclosure with 1-click `Revert` button. |
| [03-agent-plan-and-slash-popover.png](03-agent-plan-and-slash-popover.png) | 1440×860 | 7550 | `58a32764961cd08ec2c611a3ca7f55e9` | Agent `Plan` toggle active (`nex-composer-plan-banner`) with `/` command trigger popover open. |
| [04-chat-mode-attach-and-at-popover.png](04-chat-mode-attach-and-at-popover.png) | 1440×860 | 6172 | `8c946588a082d74b48f12f9db9d52bcf` | `Chat` mode (`+` attach trigger visible, `Plan` hidden) with `@` context trigger popover open. |
| [05-narrow-agent-composer.png](05-narrow-agent-composer.png) | 1024×720 | 5709 | `b2dfbf00dfb46b018a812434121b0c88` | Narrow restored window (`1024×720`) in Agent mode retaining clean layout and composer controls. |
