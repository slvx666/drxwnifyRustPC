# PROGRESS

## Status (2026-11-14)
- Milestone 0 in progress: workspace skeleton (6 core crates + Mini stub), LICENSE (GPL-3.0), .gitignore,
  throttled build script. Not done yet: CI (Windows + Linux), docs/PARITY.md, docs/DECISIONS.md, docs/PERFORMANCE.md.
- Owner decisions and the plan: `C:\Users\burger\Desktop\clavdiy\Drownify\android\concepts_brief_pc_rust.md`.
- Sources: `Drownify/spec/`, `Drownify/android/docs/PORTING-2.3.0.md`, `Drownify/android/`; `pc/` for layout/themes only.

## Build
    powershell -NoProfile -File scripts\build.ps1 cargo build --workspace
The script lowers priority and jobs while Apex or the iOS VM runs; cargo/npm caches go to D:.
