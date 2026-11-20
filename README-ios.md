# Horizon for iOS

A fork of [Horizon](https://github.com/horizon-rust/horizon) — Minecraft: Java Edition 26.3
written in Rust, on wgpu — that runs on iPad and iPhone. Everything the desktop game does, the
app does in its own process: no launcher, no Java, no child process.

- **`crates/horizon-ios/`** — the static library UIKit's `main.m` calls
  (`horizon_ios_run`). It builds the argument vector a launcher would have built, then starts
  the engine exactly as a desktop run does.
- **`ios/`** — the app around it: `main.m`, `Info.plist`, and `project.yml` for
  [XcodeGen](https://github.com/yonaskolb/XcodeGen).
- **`.github/workflows/ios-ipa.yml`** — the cloud build: compiles the engine for
  `aarch64-apple-ios` on `macos-15`, wraps it in the app, and publishes an IPA.
- **[docs/ios.md](docs/ios.md)** — the folders the app uses, prepared resources, why the build
  is Metal-only, what the first build leaves for later, and how to install the IPA.

## Build it

Cloud: **Actions → iOS IPA → Run workflow**. The artifact is
`Horizon-<version>-unsigned.ipa`.

Local (macOS):

```sh
rustup target add aarch64-apple-ios
brew install xcodegen
scripts/build-ios.sh
```

## Install it

iOS 17+ has no TrollStore, so sign the IPA first: Sideloadly / AltStore (a free Apple ID,
re-signed every 7 days), a paid developer account (a year), or ESign and similar.

## Prepared resources

The game plays from resources prepared from your **own** copy of Minecraft: Java Edition 26.3
(`horizon-prepare`, about 750 MB). A desktop launcher prepares them; iOS has neither the
launcher nor Java, so copy the prepared folder from a machine that does into the app's
`Documents/horizon-resources/` (Files app; the app shares its documents), or run the workflow
with `bundle_resources: true`. Without them the app opens a screen naming the folder it
looked in. See [docs/ios.md](docs/ios.md#prepared-resources).

## Playing it

A keyboard and a mouse or trackpad — an iPad Pro with a Magic Keyboard is the intended way —
or a paired Xbox/PlayStation controller. Touch controls are the next piece of work.

## Licence

Horizon is LGPL-3.0-or-later (see [LICENSE](LICENSE)); this fork keeps it. Not affiliated with,
endorsed by or connected to Mojang Studios or Microsoft. Minecraft is a trademark of Mojang and
Microsoft, and no Minecraft code, textures or sounds are in this repository: the game prepares
them from your own copy.