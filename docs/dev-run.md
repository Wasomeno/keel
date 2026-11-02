# Running Keel day to day

The `com.keel.dev` LaunchAgent runs Keel from a minimal app bundle,
`~/Applications/Keel.app/Contents/MacOS/keel -v`: a copy of the **release** build,
ad-hoc signed, bundle id `com.keel.dev`. The bundle is what lets macOS show
notifications under Keel's name and route a banner click back to Keel (it
also registers `keel://open-chat?id=<chat>` links). The data dir and settings
are the same as for the bare binary.
Release uses `codegen-units = 1`, thin LTO and stripped symbols (Cargo.toml).
The binary is about 30 MB (debug is 88 MB), and idle CPU is about 0.1 % (debug is about 0.3 %).

```sh
cargo build --release -p keel                  # ~3 min after a source change
scripts/bundle-mac.sh                          # copy into Keel.app, sign, register
launchctl kickstart -k gui/501/com.keel.dev    # relaunch on the new binary
```

A rebuild without `scripts/bundle-mac.sh` leaves the bundle on the old binary.

### Notifications

Banners need macOS's permission for Keel (bundle id `com.keel.dev`). The first
launch with banners on asks (a "Keel would like to send notifications" prompt).
Dismissing it (✕) counts as Don't Allow. Then turn it on in System Settings →
Notifications → Keel, or use the Open Settings button under Settings →
Notifications → Banner. Keel notices the change on its own.
Without permission there are no banners at all (sound and badge still work). There is
no AppleScript fallback: macOS showed those under Script Editor, and clicking one
opened Script Editor.

To try one: `launchctl setenv KEEL_TEST_NOTIFY latest` (or a chat id),
kickstart, then `launchctl unsetenv KEEL_TEST_NOTIFY`. About 4 s after launch
Keel posts a test banner; clicking it brings Keel forward on that chat.
`open 'keel://open-chat?id=<chat id>'` does the same through the URL scheme.

## Going back to the debug build

Use the debug build when you need debug assertions, line tables in backtraces, or fast edit/run loops.
The bare binary has no bundle, so banners fall back to AppleScript.

```sh
cargo build -p keel
launchctl bootout gui/501/com.keel.dev
sed -E -i '' 's#<string>[^<]*(MacOS|release)/keel</string>#<string>/Users/kevinananda/Codes/keel/target/debug/keel</string>#' /private/tmp/com.keel.dev.plist
launchctl bootstrap gui/501 /private/tmp/com.keel.dev.plist
```

Saved copies of both plists are in `~/keel-backups/opt_phase2/`:

- `com.keel.dev.debug.plist`
- `com.keel.dev.release.plist` (bare release binary, before the bundle)

The bundle plist is saved as `~/keel-backups/banner_click/com.keel.dev.bundle.plist`.

The live plist sits in `/private/tmp`, so it does not survive a reboot.
To keep the agent across reboots, copy the one you want to `~/Library/LaunchAgents/com.keel.dev.plist` and bootstrap from there.

Logs:

- `/tmp/keel-gui.log` and `/tmp/keel-gui.err`
- `~/Library/Application Support/Keel/logs/keel.log`

## Approval card preview

`launchctl setenv KEEL_DEMO_PERMISSION 1` (or `KEEL_TEST_CARD=permission|question|multi`), kickstart, and a sample card shows in the open chat ~5 s after launch, then folds into its receipt 8 s later and the receipt stays in the slot (UI only, nothing is sent). unset the variable and kickstart again to go back.

## Dotify dev hooks

- `KEEL_DOTIFY_IMPORT=<file>` (+ optional `KEEL_DOTIFY_ENV=dev`): Keel reads a token from the file, deletes it, and connects through the same path as Settings (Keychain). The token is never logged.
- `KEEL_DOTIFY_PROBE=1`: runs each read-only call once (check-status, summary, favorites, projects, projects?search, time-entities) and logs counts only.
- `KEEL_DEMO_DOTIFY=idle|popover|running|settings`: screenshot states. `idle`/`popover`/`running` are drawn locally and never call clock-in/out; `settings` opens Settings.
