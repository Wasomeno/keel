# Keel build policy

Applies to every agent and workstream that works in this repository. Ask the human before any exception.

## Build output

- Build and check from the repository root, `/Users/kevinananda/Codes/keel`. Its `target/` is the one shared Cargo target. `.cargo/config.toml` sets no `target-dir`, so this is Cargo's default.
- From any other checkout or worktree, run cargo with `CARGO_TARGET_DIR=/Users/kevinananda/Codes/keel/target`. Do not create per-agent, per-worktree, or per-task target directories.
- Use the shared aliases: `cargo t` for tests and `cargo app` to run Keel. Avoid bare `cargo check`, `--workspace`, and single-crate `test -p` runs that pull in gpui. `.cargo/config.toml` notes that each of these rebuilds gpui under new hashes, gigabytes each time.
- Prefer the smallest check that covers the change. Reuse `target/debug/pi` and `target/debug/pi-packages`. Run `scripts/fetch-pi.sh target/debug` only when they are missing.

## Manual validation

- The only app bundle for manual validation is `target/KeelDebug.app`. Build it and refresh it in place with `scripts/dev-app.sh`, run from the main checkout. Do not create a second `.app`.
- `scripts/dev-app.sh` stops any running KeelDebug and `target/debug/keel` before it relaunches. Check whether the app is running first. If the human may be using it, ask before running `scripts/dev-app.sh` or `cargo app`.

## Needs explicit authorization

- Release packages: `scripts/dist-mac.sh`, `scripts/publish-mac.sh`, DMG, ZIP, or update archives. `dist-mac.sh` also deletes its per-arch build directories when it finishes, unless `KEEL_KEEP_BUILD` is set.
- `scripts/bundle-mac.sh`, which writes `~/Applications/Keel.app` by default.
- Cleanup: `cargo clean`, deleting `target/` contents, or deleting `.cef/` or `target/pi-cache`. If disk space is short, report it instead.
- Changes to runtime config, scripts, or CI and release workflows made to work around this policy. The release scripts' `target/` paths are intentional, so leave them alone.
