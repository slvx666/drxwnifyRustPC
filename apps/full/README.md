# Drxwnify Full

Tauri 2 + Svelte 5 + TypeScript + Vite. The UI calls the shared core (`drx-app`) through Tauri commands only;
no business logic in TypeScript (docs/ARCHITECTURE.md).

    cd apps/full/ui && npm install && npm run tauri dev      # development
    npm run tauri build                                       # installer (later milestone)

Builds go through `scripts/build.ps1` (limits while Apex runs), for example
`powershell -File scripts/build.ps1 npm --prefix apps/full/ui run tauri build`.
