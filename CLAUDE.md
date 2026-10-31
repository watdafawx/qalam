# qalam

Windows typing assistant in Rust (single exe). Repo: github.com/watdafawx/qalam.

## Release steps (every time)
1. `cargo test`, bump `version` in Cargo.toml, `cargo build --release`.
2. Commit, `git tag vX.Y.Z`, push main + tags (use PowerShell if Bash can't reach GitHub). CI publishes the release.
3. Replace the installed copy and restart it, without asking:
   `Get-Process qalam | Stop-Process -Force; Copy-Item target\release\qalam.exe "$env:LOCALAPPDATA\Programs\qalam\qalam.exe" -Force; Start-Process "$env:LOCALAPPDATA\Programs\qalam\qalam.exe"`
