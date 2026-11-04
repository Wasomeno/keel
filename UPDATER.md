# Keel auto-update

In-app update for the macOS app, without notarization. The app checks a static manifest on GitHub Releases, verifies a minisign signature and a sha256, replaces `Keel.app` in place, restarts the launchd engine if it was started from that bundle, and relaunches.

The first install stays manual. An update never runs from a disk image, from App Translocation, from `cargo run`, or from `KeelDebug.app`.

## Decisions

- Zed-style replace of the `.app`. No Sparkle, no Mac App Store, no binary deltas.
- The release is signed with one long-lived self-signed code-signing certificate (`Keel Release`). That is not notarization and needs no Apple Developer membership. It exists so Keychain and notification permission survive an update. Ad-hoc signatures change identity every build, and macOS drops those grants.
- The archive is also minisigned. The public key is compiled into the app. A GitHub release is not trusted on its own.
- The archive is the existing `.tar.xz` of `Keel.app` (the smallest published artifact, about 139 MB at 0.2.3). One full download per update.
- The feed is `latest.json`, uploaded to every GitHub release under that exact asset name. The client fetches `https://github.com/Wasomeno/keel/releases/latest/download/latest.json`.
- Install only after the user asks to relaunch. A check may download in the background. Nothing restarts mid-turn by itself.
- User data under `~/Library/Application Support/Keel` is not read or written, except a small `update-check.json` beside `ui-settings.json`.
- Linux headless and the systemd unit are out of scope. The updater is macOS, and only the bundled GUI.

## What the app does today

Relevant facts the implementation has to keep working:

- Version is the workspace `Cargo.toml` `version` (0.2.3). `CFBundleShortVersionString` and `CFBundleVersion` are both that string (`scripts/bundle-mac.sh`). The client compares `env!("CARGO_PKG_VERSION")` to the manifest. It does not parse the plist for the decision.
- Bundle id of the release app is `com.keel.dev`. The debug bundle is `com.keel.dev.debug` (`scripts/dev-app.sh`).
- `scripts/bundle-mac.sh` signs with `KEEL_SIGN_IDENTITY`, default `Keel Dev`, and falls back to `codesign --sign -`. `scripts/dist-mac.sh` can notarize, and this plan does not start using that path.
- Opening the app runs the engine in-process and takes `engine.lock` (`apps/keel/src/main.rs`, `headed` → `acquire`). `keel daemon` installs `~/Library/LaunchAgents/sh.keel.engine.plist`, which runs `Contents/MacOS/keel headless` (`apps/keel/src/daemon.rs`). If that daemon holds the lock, the window attaches (`Outcome::Attach`) and does not own the engine.
- The plist template is `scripts/sh.keel.engine.plist`. `KeepAlive` is `SuccessfulExit = false`, and a supervised headless process that finds the lock already held exits 0. `launchctl kickstart -k` is the way to replace the running daemon with the binary now on disk.
- Settings live in `ui-settings.json`, schema `UiSettings` in `crates/proto/src/settings.rs`, read and written from `crates/ui/src/state.rs`. Settings home is `render_settings_home` in `crates/ui/src/view/settings.rs`.
- `ureq` and `sha2` are already workspace dependencies. Nothing verifies release signatures today.

## Release identity

Create the certificate once, on the machine that builds releases:

1. Keychain Access → Certificate Assistant → Create a Certificate. Name `Keel Release`, type Code Signing, let me override defaults. Back up the `.p12` and its password outside the repo.
2. Every release build sets `KEEL_SIGN_IDENTITY="Keel Release"`.
3. `scripts/dist-mac.sh` and `scripts/bundle-mac.sh` refuse to produce an update archive when the identity is missing or is `-`. A local dev bundle may still fall back to ad-hoc. An update archive may not.
4. Sign inside-out, without `--deep`: each CEF helper `.app`, the CEF framework, then the outer `Keel.app`. `--deep` re-signs nested bundles incorrectly and is the usual reason `codesign --verify --strict` fails after a copy. The updater refuses a tree that does not pass that check, so the release script must pass it before it tars.

Users do not install this certificate. `codesign --verify` succeeds on their machine because the certificate is embedded in the signature. Gatekeeper still does not trust it. The first launch of a quarantined download still needs System Settings → Privacy & Security → Open Anyway. After the quarantine attribute is gone, later in-place updates launch without that dialog.

People on the current ad-hoc 0.2.3 build update once by downloading the first certificate-signed release. The updater cannot be retrofitted into a build that does not contain it.

The designated requirement of a certificate-signed app is the certificate leaf hash, which is stable. An ad-hoc requirement is the code-directory hash, which changes every build. That is why the certificate is part of this plan even though notarization is not.

## Manifest

`latest.json` at the root of the release assets:

```json
{
  "version": "0.2.4",
  "notes_url": "https://github.com/Wasomeno/keel/releases/tag/v0.2.4",
  "min_os": "12.0",
  "assets": {
    "darwin-aarch64": {
      "url": "https://github.com/Wasomeno/keel/releases/download/v0.2.4/Keel-0.2.4-macos-arm64.tar.xz",
      "sha256": "<64 lowercase hex>",
      "size": 145000000,
      "signature": "<minisign signature text>"
    }
  }
}
```

Rules:

- `version` is plain semver: `MAJOR.MINOR.PATCH`. No prerelease, no build metadata. The client uses the `semver` crate. A prerelease in the feed is ignored. Equal or older is not an update.
- Asset key is `darwin-` plus `std::env::consts::ARCH` (`aarch64` or `x86_64`). A missing key is "no build for this Mac", not a failed download. Do not install an arm64 archive over an x86_64 app. When a universal binary exists, publish it under both keys with the same url, or add `darwin-universal` and let either arch select it. v1 can ship arm64 only, matching the current GitHub assets.
- `url` is the versioned release asset, never `releases/latest/download/...`. The manifest is the only thing fetched from the floating `latest` URL.
- `signature` is the full minisign `.minisig` text for those exact bytes.
- `size` is the archive size in bytes. The client rejects a body that does not match, and rejects a `size` above 1 GiB.
- `notes_url` is opened in the browser from the Settings row. It is not rendered as HTML inside the app.
- `min_os` is informational in v1. Keel's plist already requires macOS 12. If a later release raises it, the client compares it to the running OS and shows the requirement instead of downloading.

Publish order: upload the `.tar.xz` first, then `latest.json`. A client that sees a new manifest must already be able to download the archive.

Override the manifest URL with `KEEL_UPDATE_URL` so tests can point at a local file server. Unset in releases.

## Archive

The tar contains one top-level entry, `Keel.app`, with symlinks preserved and extended attributes omitted:

```sh
tar --no-xattrs -cJf "Keel-$VERSION-macos-arm64.tar.xz" -C "$STAGE" Keel.app
```

`--no-xattrs` keeps `com.apple.quarantine` out of the archive. The CEF framework's symlinks have to survive; a tar that follows them breaks the signature. The release script extracts the archive into a temp directory and runs `codesign --verify --strict` on the result before it uploads.

The client rejects an archive whose first entry is not `Keel.app`, whose entries escape that directory (`..`, absolute paths, links that point outside), or that contains a second top-level name.

## App signature and the update signature

Two different signatures, both required:

| Check | Key | When |
|---|---|---|
| minisign over the `.tar.xz` bytes | Public key compiled into the binary | Before extract |
| sha256 of those bytes | Hex in `latest.json` | While streaming to disk |
| `codesign --verify --strict` on the extracted `Keel.app` | `Keel Release` certificate inside the bundle | Before the live app is touched |
| `codesign --verify --strict` on the installed app | Same | After the copy, before relaunch |

The minisign public key is a file in the repo, `apps/keel/update-key.pub`, loaded with `include_str!`. The private key is not in the repo. The release script reads it from `KEEL_UPDATE_SECRET` (a path to the minisign secret key) and fails if that variable is unset. Rotate the key by shipping one build that trusts both keys, then dropping the old one in the build after that. v1 trusts one key.

Add `minisign-verify` and `semver` as workspace dependencies. Use `ureq` for the two HTTP GETs and `sha2` for the digest. Stream the archive to a file under the temp dir. Do not hold the body in a `Vec`.

User-Agent is `Keel/<version>`. No other headers. No device id, no query string.

## When an update is allowed to run

All of these, or the client does not contact the network:

- `current_exe()` is `…/Keel.app/Contents/MacOS/keel`.
- The bundle id in that app's `Info.plist` is `com.keel.dev`.
- The canonical path does not contain `AppTranslocation`.
- The bundle directory is writable by this user. A read-only volume, including the disk image the app was opened from, fails this.
- `KEEL_UPDATE_URL` is https, or a `http://127.0.0.1` / `http://localhost` URL. Anything else is refused.

`cargo run` and `KeelDebug.app` stop here. Settings still shows the version and the sentence "Updates are available in the installed Keel app."

A bundled app that fails the writable check does contact nothing and shows: "Move Keel to the Applications folder to install updates." The manual Check button shows the same sentence. It does not download.

## Check, download, install

State machine, owned by the window, with the blocking work on a background thread. Status values: `Idle`, `Checking`, `UpToDate`, `Available { version }`, `Downloading { version, fraction }`, `Ready { version }`, `Installing`, `Failed { message }`. `fraction` is `None` until `Content-Length` is known.

Automatic check:

- On launch, if `UiSettings.auto_update` is not `Some(false)`, and `update-check.json` `checked_at` is older than 24 hours.
- One attempt. Network and HTTP errors log at info and return to `Idle`. They do not surface in the UI.
- A newer manifest moves to `Available`, then starts the download without asking. The download is the large part; applying it still waits for the user.
- Write `checked_at` only after a response is parsed, so an offline launch retries next launch.

Manual check (`Check Now`):

- Same request. Failures set `Failed` with a short message ("Could not reach the update server.", "The update signature is not valid.", "No build for this Mac.").
- `UpToDate` is visible ("Keel 0.2.4 is up to date.").

Download:

- Temp dir `keel-update-XXXX` from `std::env::temp_dir()`.
- Stream to `archive.tar.xz`, hashing as it writes. Refuse a body larger than `size` or larger than 1 GiB. Require the final length and sha256 to match.
- Verify minisign against the compiled public key. On failure, delete the temp dir and set `Failed`. Do not extract.
- Extract with the system `tar` (`-xJf`) into `stage/` inside that temp dir, after the path checks above. Then `codesign --verify --strict` on `stage/Keel.app`.
- Status becomes `Ready`. Leave the staged tree on disk until install or until a newer check replaces it.

Install, only from the Relaunch button:

- Confirm with the existing prompt style: title "Relaunch to update Keel?", body "The running session stops. Keel opens again on {version}." Buttons Relaunch and Cancel. If a turn is in flight, the body says so. Do not invent a second dialog system.
- Prefer a sibling swap when the parent of `Keel.app` is writable: copy or rsync the staged app to `Keel.app.next` in that parent, verify the signature, then `mv Keel.app Keel.app.previous` and `mv Keel.app.next Keel.app`. The running process keeps the old inode. Rename needs write permission on the parent, which `/Applications` often does not grant.
- Otherwise rsync in place, which only needs write permission inside the bundle. Flags: `-a --delete --exclude Icon?`. No `-X`, no `-E`, so extended attributes are not copied. This matches Zed's `install_release_macos`. Verify the staged tree first. If rsync's exit code is non-zero, set `Failed` with the stderr and do not relaunch. An in-place failure can leave a bundle that will not launch; the message tells the user to download the release again.
- `xattr -cr` on the installed `Keel.app`.
- `codesign --verify --strict` again. On failure, do not relaunch.
- If `sh.keel.engine` is loaded and its plist `ProgramArguments` first string equals this bundle's `Contents/MacOS/keel`, `launchctl kickstart -k gui/<uid>/sh.keel.engine` after the files are in place and before the new window opens. A daemon started from a different binary (a dev build) is left alone. Read the plist from `launch_agent_plist_path()`; do not guess the path.
- Spawn a short-lived waiter, then flush the engine and exit. The waiter sleeps until this pid is gone, deletes `Keel.app.previous` if the swap path was used, and `open`s the new bundle. The new process takes `engine.lock` or attaches to the daemon just restarted.
- Remove the temp dir on the way out. On the next launch, delete temp dirs named `keel-update-*` older than a day, the same leak Zed hit by deleting a directory that was still busy.

The waiter is a `/bin/sh` process, not another copy of Keel. It does not link the app and it does not hold `engine.lock`.

## Daemon and the lock

Replacing the bundle does not restart a running headless engine. Launchd keeps the old process, the plist path still points at `Contents/MacOS/keel`, and the new window would attach to the old engine.

The kickstart above is the whole fix. Do it only after the new bytes are in place and the signature verifies. Under `KEEL_SUPERVISED`, the old headless exits 0 when kicked, and `KeepAlive` does not treat that as a crash loop because kickstart starts it again explicitly.

Do not delete or rewrite the plist. The binary path is unchanged. Do not call `keel daemon uninstall`.

## Settings and UI

`UiSettings` gains one field:

```rust
/// Settings → Updates. Absent means check. `Some(false)` turns checks off.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub auto_update: Option<bool>,
```

Absent stays "on" so existing `ui-settings.json` files check. `save_settings` already rewrites the whole struct.

`update-check.json` in the data dir, next to `ui-settings.json`:

```json
{ "checked_at": 1762220000 }
```

Not part of `UiSettings`. A failed parse means "check now".

Settings home, in `render_settings_home`, gets an Updates section after Appearance. Same section header, divider, and button patterns as the sections around it.

- Line 1: "Keel {version}".
- Line 2: the status sentence for the state machine.
- Switch: "Check automatically", bound to `auto_update`.
- Button: `Check Now` when idle or up to date, disabled while checking or downloading, `Relaunch to Update` when `Ready`.
- A notes link when `notes_url` is present, opened with the same URL opener the rest of the app uses.

When a download reaches `Ready` and Settings is closed, show one dismissible line in the window chrome, not a macOS notification banner. Those banners are for agent events (`NotifySettings`). The line's action opens Settings on the Updates section. Dismissing it does not delete the staged archive. The next launch shows it again while `Ready` is still true.

Copy stays short. No progress percentage in the chrome line. The Settings row shows the fraction during `Downloading`.

## Release script

Add `scripts/publish-mac.sh`. It does not upload. It writes the files a person attaches to the GitHub release, in the right shape.

Inputs: `KEEL_SIGN_IDENTITY` (required, not `-`), `KEEL_UPDATE_SECRET` (path to the minisign secret key). Optional `KEEL_NOTARY_PROFILE` is ignored by this script.

Steps:

1. Read the version from the workspace `Cargo.toml`, the same `sed` `dist-mac.sh` uses.
2. `cargo build --release -p keel` for `aarch64-apple-darwin`. An x86_64 archive is a later flag, not part of v1.
3. Bundle with `KEEL_APP` set to a temp `Keel.app`, using the inside-out sign. Stop passing `--deep`.
4. `codesign --verify --strict` and fail the script on any error.
5. Tar as above. `shasum -a 256`. `minisign -S -s "$KEEL_UPDATE_SECRET" -m` the archive. The secret key's password comes from `KEEL_UPDATE_SECRET_PASSWORD` in the environment when the key is encrypted, never from a file in the repo.
6. Write `latest.json` beside the archive.
7. Extract the tar in a fresh temp dir and verify the signature again. This is the check that symlinks survived.
8. Print the two paths to upload, archive first, `latest.json` second.

`scripts/bundle-mac.sh` grows a sign function used by both the local bundle and `publish-mac.sh`. Local `Keel Dev` / ad-hoc behavior for `~/Applications/Keel.app` stays. Only `publish-mac.sh` refuses ad-hoc.

Document the first-time minisign key in a comment at the top of `publish-mac.sh`: `minisign -G -p apps/keel/update-key.pub -s <somewhere outside the repo>`. Commit the `.pub` only.

## Tests

Unit tests, no network, no GUI:

- Manifest parse. Missing asset for this arch. `size` over the cap. Non-https url. Version older, equal, newer, prerelease.
- Tar entry check accepts `Keel.app/Contents/MacOS/keel` and rejects `../Keel.app`, an absolute path, a second top-level name, and a symlink whose target escapes the bundle.
- Bundle gate: a fake `Keel.app` layout is accepted; a path containing `AppTranslocation` is not; a binary that is not `Contents/MacOS/keel` is not.
- Plist program-argument compare: the same path restarts; a different path does not. Use the fixture string from `scripts/sh.keel.engine.plist` filled in by `render_plist`.
- `auto_update` absent deserializes as `None` and is treated as on. `Some(false)` stays off. A `ui-settings.json` from before the field still parses.
- Minisign verify against a fixture keypair generated in the test. A flipped byte fails.

Manual test on a Mac, once, before calling the script done:

1. Install a certificate-signed build that contains the updater into `~/Applications`.
2. Serve a newer `latest.json` and archive on `127.0.0.1`. Set `KEEL_UPDATE_URL`.
3. Check, download, relaunch. The new version is running. `codesign --verify --strict` passes. A file planted in the data dir is still there.
4. Repeat with `keel daemon` running from that bundle. After relaunch, `keel status` shows the new binary's version and the daemon pid has changed.
5. Flip one byte of the archive. The app stays on the old version and Settings shows the signature error.
6. Open the app from the disk image. Check Now shows the move-to-Applications sentence and does not write into the image.
7. Turn the switch off, relaunch, stay offline. Nothing in the log about a fetch.

No browser pass. This is a native window. The manual run above is the verification.

## Out of scope

- Notarization, Developer ID, stapling, Sparkle, deltas.
- Silent install, install on a timer, phased rollout.
- Updating the Linux binary or the systemd unit.
- Homebrew. If a cask appears later, refuse to replace the app when it was installed by Homebrew and say so, the way Zed does. Do not build that detection now.
- Rendering release notes inside the app.
- Migrating Keychain items from the old ad-hoc identity. Users who set "Always Allow" on 0.2.3 will be asked again once, on the first certificate-signed launch.

## Implementation order

1. `publish-mac.sh`, inside-out signing, minisign key, `latest.json`. Ship one signed release that does not yet contain the client, or ship the client in the same release. Either way the first signed build is a manual download for current users.
2. Manifest, version, tar, and bundle-gate tests. No UI.
3. Download, verify, stage. Settings section with Check Now and the status line.
4. Install, quarantine strip, daemon kickstart, relaunch waiter.
5. Automatic daily check, the chrome line, and the manual test list above.

Each step is separately usable. Step 3 can tell the user a version is ready even if step 4 is not merged; the button stays "Check Now" until install exists.
