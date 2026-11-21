# 构建

Rust 侧任何平台都能构建和测试；iPad 应用需要 macOS + Xcode。

## 环境

| 组件 | 版本 | 说明 |
|---|---|---|
| Rust | stable ≥ 1.100 | `rust-toolchain.toml` 已锁定通道与 target |
| Xcode | 支持 iPadOS 27 SDK | 只在 macOS 上需要 |
| XcodeGen | ≥ 2.40 | `brew install xcodegen` |
| PowerShell | 5.1 或 7 | 仅 Windows 验证脚本需要 |

## Rust 引擎

```bash
cd engine
cargo test                       # 全部单测
cargo build --release
cargo build --release -p hz-cli  # 只要命令行
```

产物：`engine/target/release/hz`（Windows 上 `hz.exe`）。

### 命令

| 命令 | 作用 |
|---|---|
| `hz port <jar> <out.hzp> [--report r.json] [--version 26.3] [--quiet]` | 编译资源包并输出报告 |
| `hz info <pack.hzp>` | 打印条目数、种类、manifest |
| `hz classes <jar>` | JVM 统计（类/方法/字节码/调用边缘/热点目标）JSON |
| `hz stubs <jar> <dir>` | 每类生成一个 Rust stub 模块 |
| `hz unzip <jar> <dir>` | 用自研 inflate 解包（用于交叉验证） |
| `hz hash <file>` | SHA-1 / SHA-256 |
| `hz selftest` | 合成 jar → 编译 → 起引擎 → 报面数 |
| `hz bench <pack.hzp> [radius]` | 造一个混合方块的世界，量重建一帧的耗时、面数、顶点字节数、帧里出现的贴图槽数 |

## iOS / iPadOS

```bash
./scripts/build-ios.sh            # 交叉编译 device + simulator 静态库
cd app
xcodegen generate
open HorizonPad.xcodeproj
```

`build-ios.sh` 会：

1. `rustup target add aarch64-apple-ios aarch64-apple-ios-sim`
2. 分别编译 `hz-ffi`（`crate-type = ["staticlib", "cdylib"]`）
3. 把 `libhz_ffi.a` 拷到 `engine/target/ios/`（设备）与 `engine/target/ios-sim/`（模拟器）
4. 设备库做 `strip -S`

`project.yml` 里已经写好 `HEADER_SEARCH_PATHS`、`LIBRARY_SEARCH_PATHS`、`OTHER_LDFLAGS = -lhz_ffi`。
真机运行时把模拟器库路径换掉（或改用 `.xcframework`，见下）。

### 首次在 Xcode 打开时预计要处理的几件事

本仓库的 Swift 与 Metal 代码**没有在 Xcode 里编译过**（开发机是 Windows）。按重要性排序：

1. **Swift 版本模式**：`project.yml` 用 `SWIFT_VERSION 5.0` + `SWIFT_STRICT_CONCURRENCY: minimal`。
   若你的 Xcode 只接受 Swift 6 模式，请逐个修 `@Sendable` 捕获（`Porter.port` 的回调与
   `withCheckedThrowingContinuation`），或把 `ImportController.run()` 改成 `@MainActor` 里
   直接 `await` 一次 `Task.detached`。
2. **静态库架构**：模拟器在 Apple Silicon 上是 `aarch64-apple-ios-sim`，不能用设备库。
   想一份工程通吃，把两份 `libhz_ffi.a` 打成 `hz_ffi.xcframework`：
   ```bash
   xcodebuild -create-xcframework \
     -library engine/target/ios/libhz_ffi.a -headers engine/include \
     -library engine/target/ios-sim/libhz_ffi.a -headers engine/include \
     -output engine/target/hz_ffi.xcframework
   ```
   然后在 `project.yml` 里用 `frameworks: [../engine/target/hz_ffi.xcframework]`。
3. **Metal 3.1**：`METAL_LANGUAGE_VERSION 3.1` 要求 A14/M1 及以上且 iPadOS 16+。
   若目标设备更老，改成 `2.4`，并确认 `Shaders.metal` 没用新语法（当前没有）。
4. **`NavigationSplitView` + `NavigationStack` 嵌套**：iOS 27 上应该正常；若出现双重返回按钮，
   把 `RootView` 的 detail 换成单一 `NavigationStack`（去掉 `NavigationSplitView`）。
5. **文档类型**：`Info.plist` 声明了 `.hzp`/`.hzw` 的 UTI，`CFBundleDocumentTypes` 需要与
   `UTImportedTypeDeclarations` 一致——已在同一文件里。

### 模拟器上跑测试

```bash
xcodebuild test -project app/HorizonPad.xcodeproj \
  -scheme HorizonPad -destination 'platform=iOS Simulator,name=iPad Pro 13-inch (M4)'
```

`HorizonPadTests` 里有 6 组用例：ABI 版本、错误面、报告解析、progress 单调性、容器目录存在。

## 不装 Xcode 也能做的检查

三个层次，从便宜到有力：

```bash
python3 scripts/swift_check.py          # 结构：花括号、#if 配对、C 头与封装是否脱节
bash scripts/swift_typecheck.sh         # 需要 Swift toolchain（Linux/macOS/Windows 都有）
```

`swift_typecheck` 做三件事：

1. **`swiftc -parse` 全部 21 个 .swift** —— 语法、字面量、属性位置。这一步不需要 Apple SDK。
2. **`-typecheck` 只依赖 Foundation 的文件**（`Paths.swift`、`HZEngine.swift`），
   用 `-import-objc-header` 把真实的 `hz_ffi.h` 打进去。`HZEngine.swift` 是 400 行 FFI 封装，
   指针类型写错就是这里出事，值得单独检查。
3. **`-typecheck` 模型层**（`Library.swift`、`Settings.swift`）对着
   `scripts/swift_shims/Combine.swift` —— 一个只声明 `ObservableObject`/`@Published` 的桩模块。
   它没有行为，只有类型；目的是在非 Apple 主机上抓出拼错的成员名和漏掉的 import。

Windows 上装 toolchain：`winget install Swift.Toolchain`（脚本会自动找到 `swiftc` 与 SDK）。

`swift_typecheck.ps1`（Windows）与 `swift_typecheck.sh`（Linux/macOS/CI）做同样三件事，
后者在 Windows 的 Git Bash 里也能跑：它自己向上找 `Platforms/*/Windows.sdk`，找到就加 `-sdk`，
Linux/macOS 上找不到就跳过，行为一致。CI 用 `.sh` 那份，挂 `swift-actions/setup-swift`。

### 这两个检查当场抓到过的真问题

- `Library.swift`、`Log.swift` 声明了 `ObservableObject`/`@Published` 却只 `import Foundation`
  —— 缺 `import Combine`，在 Xcode 里是编译错误。
- `ForEach(dict.sorted(...), id: \.key)`：Swift 不支持指向元组元素的 key path，
  已换成 `KindRow`/`EnvRow` 这类 `Identifiable` 行类型。
- 桥接头里的 `#import` 被 clang-cl 当成 MSVC 类型库导入而失败，已改成 `#include`。

`swiftc -parse` 抓不到类型错误、`-typecheck` 抓不到 SwiftUI/Metal 的 API 误用；
后者只有 macOS 的 `xcodebuild` 能证明。所以 CI 里两者都跑，本地能省一次 Mac 往返。

它做四件小事，但每一件都对应真实的低毛错误：

1. 去掉注释与字符串（含 `\( )` 插值嵌套）后检查 `()`/`[]`/`{}` 配平；
2. `#if`/`#endif` 配对数；
3. 缩进里空格与 tab 混用；
4. **`hz_ffi.h` 里声明的每个函数是否都在 `HZEngine.swift` 里被调用**——防止加了 FFI 却忘了接。

CI 里这一步跑在 `ubuntu-latest` 上，先于 macOS 构建，失败得快、省机器。

## 在 GitHub 上编译（不需要 Mac）

可以。GitHub Actions 的 `macos-*` runner 本身就是一台 Mac，公开仓库免费额度足够跑这套构建。
仓库里已经有两个工作流，直接用：

| 工作流 | 触发 | 产出 |
|---|---|---|
| `.github/workflows/ci.yml` | push / PR / 手动 | 三平台引擎测试、ZIP 往返、Swift 结构检查、**iOS 模拟器构建 + 测试 + Metal 编译** |
| `.github/workflows/release.yml` | 手动 / `v*` 标签 | 未签名的模拟器 `.app`（zip）+ `SHA256SUMS` |

### 上云前的自检

```bash
python3 scripts/preflight.py
```

它检查三类"本机能跑、runner 上必炸"的问题，CI 里作为第一个 job 跑（几秒，先于 macOS）：

1. **绝对路径**进了脚本（`D:\...`、`/Users/...`）——这条在本仓库真实发生过：
   `swift_check.py` 与 `swift_refs.py` 曾把 `D:\rea\HorizonPad` 写死，在 runner 上
   前者会报错、后者会报 "checked 0 files: ok"，后者更糟：静默通过。
   现在两者都从 `__file__` 推导根目录。
2. **workflow 引用了不存在的文件**（改名或漏提交）。
3. **大文件**：单个文件超过 2 MiB 会提醒，避免基线仓库膨胀。

### 第一次怎么跑

```bash
cd HorizonPad
git init && git add -A
git commit -m "HorizonPad"
git branch -M main
git remote add origin https://github.com/<你>/<仓库>.git
git push -u origin main
```

推送后 Actions 自动开始。`ios` job 里这几步是本地做不到的：

1. `bash scripts/build-ios.sh` —— 交叉编译 `aarch64-apple-ios` 与 `-sim` 两份静态库；
2. `xcodegen generate` —— 生成 Xcode 工程；
3. `xcodebuild build/test` —— 真正编译 SwiftUI/Metal 并跑 `HorizonPadTests`；
4. `xcrun -sdk iphonesimulator metal -c` —— **真编译**翻译器吐出的 MSL。

工作流里有几处专门为“换个 runner 镜像也能跑”处理过：

- 用 `bash scripts/...` 而不是 `./scripts/...`（文件的可执行位在检出后不可靠）；
- 目标模拟器是运行时用 `xcrun simctl list devices available` 选出来的，不写死机型；
- 静态库搜索路径按 SDK 区分（`iphonesimulator*` → `engine/target/ios-sim`）。

### 能拿到什么，拿不到什么

| | 可以 | 说明 |
|---|---|---|
| 验证代码能编译 | ✅ | 模拟器构建 + 单测，就是这一条的价值 |
| 下载模拟器版 `.app` | ✅ | 解压后 `xcrun simctl install booted HorizonPad.app`，或直接 Xcode 里跑 |
| 安装到真机 iPad | ❌ | 需要签名证书与描述文件，仓库里没有，也不该有 |
| 上架 App Store | ❌ | 同上，另需审核 |

**这不是省略步骤，是签名本身的限制**：iOS 只安装由可信身份签名的包。想要真机包，两条路：

1. 在自己的 Mac 上用免费 Apple ID 签名（Xcode → Signing & Capabilities → Personal Team），
   这是唯一零成本的真机路径，但仍然需要一台 Mac；
2. 有付费开发者账号的话，把证书导出成 `.p12` 与描述文件放进仓库 Secrets，
   再把 `release.yml` 里的 `CODE_SIGNING_ALLOWED=NO` 换成真实签名步骤
   （`security import` + `xcodebuild -exportArchive`）。证书一旦进 CI 就等于交给 GitHub，
   自行权衡。

### 只想验证引擎、不碰 iOS

`ci.yml` 的 `engine` job 在 Ubuntu / macOS / Windows 上各跑一遍 127 个测试；
`roundtrip` job 在 Windows 上用 PowerShell 自带压缩器造 jar 做往返验证。
这两个 job 都不需要任何 Apple 工具。

## Windows 验证（无需 Xcode）

```powershell
cargo build --release --manifest-path engine\Cargo.toml
pwsh -File scripts\verify-pipeline.ps1
```

脚本会：

1. 用 PowerShell 自己的 ZIP 写出器造一个 Minecraft 形状的 jar（**真实 DEFLATE**，含 256 KiB 不可压数据）
2. 用 `hz unzip` 解回来，逐文件比对 SHA-256
3. `hz port` 编译成 `.hzp`，打印报告
4. `hz selftest` 起引擎并报告面数

四步全绿才退出 0。

## 常见问题

**`cargo` 找不到**：`rustup` 安装后需要把 `~/.cargo/bin` 加进 PATH（Windows 上重新开一个终端）。

**`class: bad magic`**：jar 里的 `.class` 被改过或本身就是占位文件。管线只把它记进
`warnings`，不影响数据编译。

**覆盖度 0%**：jar 里没有 `assets/`/`data/`（可能是 `server.jar` 或纯代码包）。
用完整 client jar。

**导入后没有贴图**：`textures` 计数为 0 说明 PNG 魔数校验失败——多见于被二次压缩过的包。