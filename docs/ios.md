# Horizon on iOS

The game runs in the app's own process on iPad and iPhone (iPadOS / iOS 17 and later).
`.github/workflows/ios-ipa.yml` compiles the engine on a macOS runner and publishes an IPA;
`scripts/build-ios.sh` does the same on a Mac.

```
crates/horizon-ios/       the static library UIKit's `main.m` calls
ios/Sources/main.m        the app's `main`: computes the folders, calls the library
ios/Sources/Info.plist    bundle identity, Metal capability, ProMotion, Files-app sharing
ios/project.yml           the Xcode project xcodegen generates around them
.github/workflows/ios-ipa.yml   the cloud build
```

## What iOS changes about the start

A desktop start is a child process: the launcher builds a command, and the game takes its
arguments, its data folder and an install lease from it. iOS has no `fork` and no `exec`,
so `horizon_ios_run` (`crates/horizon-ios/src/lib.rs`) does what a launcher would have:

| Desktop | iOS |
|---|---|
| `LaunchArguments::for_command` reads the command's arguments, directory and environment | `LaunchArguments::for_args` takes the argument vector and the start directory directly |
| `entry::prepare` forks nothing but inspects `current_exe` for the lease | `entry::prepare_from` skips the holder: the app *is* the process |
| the launcher passes `--game-dir`, `--home`, `--resources` | `main.m` computes them from the sandbox (`Documents`, `Application Support`) |
| `horizon-game::main` loads native mods and the WebAssembly runtime | the iOS entry starts the engine directly: iOS refuses `MAP_JIT` and unsigned code pages |

Everything after that is one engine start: `entry::start` installs the hooks, opens the
event loop and runs the game until the player quits.

## Folders the app uses

| Path | Contents |
|---|---|
| `Documents/` | the game folder: `saves/`, `options.txt`, `resourcepacks/`, `shaderpacks/`, and `horizon-resources/` |
| `Library/Application Support/horizon/` | `logs/`, `crash-reports/`, telemetry consent |
| `Documents/horizon-resources/` (or the bundle, when bundled) | prepared resources from `horizon-prepare`, identified by `horizon-resources.json` |

`UIFileSharingEnabled` and `LSSupportsOpeningDocumentsInPlace` are on, so the Files app
(or Finder's file sharing) can copy a world or a prepared resources folder in.

### Prepared resources

The game plays from resources Horizon prepares from the player's **own** Minecraft: Java
Edition 26.3 install — `horizon-prepare --prepare` writes `.local/horizon-client/26.3`,
about 750 MB. On a desktop the launcher does that. On iOS neither the launcher nor a Java
runtime exists, so a Mac (or any machine that has Minecraft and Java 25) prepares them once:

```sh
# on a Mac or PC that has the launcher installed
~/.local/share/horizon-launcher/versions/<version>/.local/horizon-client/26.3   # macOS
%APPDATA%\horizon-launcher\versions\<version>\.local\horizon-client\26.3        # Windows
```

then either

- copy that folder into the app's `Documents/horizon-resources/` through the Files app, or
- bundle it in the IPA: run **iOS IPA** with `bundle_resources: true` and either the folder at
  `.local/horizon-client/26.3` in the checkout or a `resources_archive_url` pointing at a
  `tar.gz` of it. `.github/workflows/ios-ipa.yml` copies it to `ios/Resources/horizon-resources`,
  where `main.m` finds it with `NSBundle.pathForResource`.

A bundle carrying them is large (hundreds of megabytes); the resources are Mojang-derived and
stay out of the repository (`.gitignore`, `tools/check_public_tree.py`).

Without them the game opens its own "no resources" screen and names the folder it looked in.

## Why the build looks the way it does

- **Metal only.** `wgpu`'s `vulkan` and `gles` features are added per target, not globally
  (`crates/horizon-client/Cargo.toml`): an iOS build would otherwise link a Vulkan loader and
  an EGL that iOS does not have. `backend_order` (`gfx/device.rs`) returns Metal for `ios`,
  as for `macos`.
- **No controllers, no audio in the first build.** `gilrs` has no iOS gamepad backend and
  `cpal` needs an activated `AVAudioSession`; both are off (`--no-default-features`, which is
  what `horizon-ios`'s dependency line asks for). `horizon-dualsense` already builds
  output-less on iOS.
- **std's allocator.** `horizon-threads` hands iOS `std::alloc::System`: the OS reclaims
  memory under pressure and polices a foreground app's resident budget, and mimalloc's pools
  work against both. The measuring allocators (`allocator_stats`) still link mimalloc, which
  builds for the target.
- **No mod host.** `horizon-mod-host` and `horizon-mod-wasm` are not in the iOS graph:
  wasmtime needs writable-and-executable pages, which iOS denies. Content mods (`.hmod`) from
  the game folder still load through the engine.
- **The clipboard is the game's own.** `arboard` has no iOS backend, so copying and pasting
  works inside the game only, as on Android (`clipboard.rs`).
- **ProMotion.** `CADisableMinimumFrameDurationOnPhone` lets UIKit run the display link above
  60 Hz; the game's frame limiter follows the display's refresh rate.
- **The narrator is unavailable**: `horizon-narrator` has speech arms for Windows, macOS and
  Linux only, so the narrator option reports unavailable.

## Not in the first build

These are the follow-ups, in the order they would be worth doing:

1. **Touch controls.** The engine reads a keyboard, a mouse and controllers. A touch layer
   (virtual stick, look drag, tap-to-break, long-press-to-place) belongs in
   `horizon-client-input`, beside `keypad.rs`, which the Android build already drives.
2. **Audio.** `cpal` + an activated `AVAudioSession` (category `ambient`), then the mixer the
   desktop build uses.
3. **Gamepad.** `GCController` through `GameController.framework`, fed to the same `PadState`
   the desktop pads use.
4. **Clipboard.** `UIPasteboard` for text in and out.
5. **Narrator.** `AVSpeechSynthesizer`.
6. **A resource import screen.** Let the player pick a prepared-resources archive in the Files
   app instead of copying a folder by hand.

## Installing the IPA

iOS 17 and later has no TrollStore, so a signature is always needed:

| Way | Needs | Lasts |
|---|---|---|
| Sideloadly / AltStore / SideStore | a free Apple ID | 7 days, then re-sign |
| Sideloadly with a paid developer account | 99 USD/year | 1 year |
| ESign / a signing service | their certificate | theirs |

The workflow's default artifact is **unsigned** (`Horizon-<version>-unsigned.ipa`). To have CI
sign it instead, add `BUILD_CERTIFICATE_BASE64`, `P12_PASSWORD`,
`PROVISIONING_PROFILE_BASE64` and `APPLE_TEAM_ID` as repository secrets, set `bundle_id` to the
identifier the profile carries, and run the workflow with `sign: true`.

## Running it on a Mac without the cloud

```sh
rustup target add aarch64-apple-ios
brew install xcodegen
scripts/build-ios.sh              # ios/Horizon-<version>-unsigned.ipa
scripts/build-ios.sh --signed     # archive + export, with a keychain identity
```

## Verifying a build

- `cargo check -p horizon-ios --target aarch64-apple-ios` — the whole iOS graph, in minutes.
- The workflow prints `lipo -info` on the archive: it must say `arm64`.
- `du -sh Horizon.app`: without bundled resources the app is tens of megabytes; a sudden
  hundreds-of-megabytes jump means resources leaked in.
- On device: the first frame shows the loading screen, then the title screen. `Documents/logs`
  (see `Library/Application Support/horizon/logs/latest.log`) records the start, and a crash
  writes into `crash-reports/` there.