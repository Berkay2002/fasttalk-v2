# FastTalk desktop app

This directory contains the Tauri v2 shell and React frontend. Product logic
lives in the Rust workspace crates. The WebView receives only narrow Tauri
commands and never launches or controls native workers directly.

From this directory:

```powershell
npm install
npm run build
npm run tauri dev
```

The project currently uses loopback ports 18080 for llama.cpp and 18081 for
NeMo-Speech.cpp. The CSP allows only those native worker endpoints.
