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

## Windows release

From the repository root, build the signed current-user NSIS installer with:

```powershell
$env:FASTTALK_SIGNING_THUMBPRINT = "<certificate thumbprint>"
$env:FASTTALK_TIMESTAMP_URL = "<RFC 3161 timestamp URL>"
.\scripts\Build-Release.ps1
```

The release build rebuilds the pinned native workers, downloads and verifies the
current Microsoft Visual C++ x64 runtime, signs the native payload, runs the Rust
tests, and signs the application and installer. For a local packaging check only,
use `-Unsigned -SkipNativeBuild`. Validate the resulting installer with
`.\scripts\Test-Installer.ps1`.

Models are downloaded after installation or imported from a verified offline
pack. They are not embedded in the installer.

The current signed model release selects Qwen3.5 9B Q5_K_M. The planned
Qwen3.6 27B profile missed the 20 token/s gate under representative desktop
load, while this compatibility profile passed the throughput and latency gates.
Regenerate a manifest with `New-ModelManifest.ps1`, then sign it with an
explicit private seed using `Sign-ModelManifest.ps1`.

Hardware and model runtime choices are kept in
`config/runtime-profiles.json`. A profile owns its model bindings, llama.cpp
context and parallelism, GPU reserve, and measured worker-memory figures. The
runtime detects total VRAM and admits GPU TTS against that profile instead of
assuming every machine has a 24 GB GPU.
