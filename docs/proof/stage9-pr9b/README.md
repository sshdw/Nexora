# Stage 9B native QA frames

Source `d245e1d6728ca5089176e6b2696e47112c55e10f`. Separate compiled Windows Tauri application (`nexora.exe`) with isolated synthetic `APPDATA`/`LOCALAPPDATA`/`TEMP` and SQLite `PRAGMA integrity_check=ok`, `foreign_key_check=[]` at `1202 × 831`.

| Frame | MD5 | Colors | Observation |
| --- | --- | ---: | --- |
| [01-mcp-empty-en.jpg](01-mcp-empty-en.jpg) | `889506a54463ba1017b30f147f6636ca` | 23836 | Settings MCP servers section with zero bundled servers, Personal-only scope, and explicit approval notice. |
| [02-mcp-add-stdio-en.jpg](02-mcp-add-stdio-en.jpg) | `f51c1a25dc4f3e7ca80e918475707f98` | 24224 | Local stdio MCP server form with command, args, and non-secret env vars. |
| [03-mcp-add-http-oauth-en.jpg](03-mcp-add-http-oauth-en.jpg) | `414bf0613501ab4a847c94d7b374456c` | 25039 | Streamable HTTP MCP server form with https/localhost endpoint URL and OAuth 2.1 (PKCE S256) toggle. |
