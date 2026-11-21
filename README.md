# HorizonPad

在 iPad 上把**你自己的**《我的世界》Java 版资源编译成原生格式，并交给内置的 Rust 引擎运行。
目标是复刻 [Horizon](https://playhorizon.dev) 那套「Java 版重写为 Rust」的思路，但把宿主换成 iPadOS。

- 引擎：Rust，**零 crates.io 依赖**（唯一外部代码是 vendored 的 `stb_vorbis.c`，由 `build.rs` 用系统 C 编译器编译），交叉编译为 `aarch64-apple-ios` 静态库
- 前端：SwiftUI + Metal，iPadOS 27.0 起
- 不含任何 Mojang 代码、贴图、声音：资源由用户自己的 jar 在设备上生成

## 先说清楚哪些能做、哪些不能

| 需求 | 在 iPad 上 |
|---|---|
| 读取 client.jar、解析方块状态/模型/贴图/语言/数据包 | ✅ 自研 ZIP + inflate，全程设备内完成 |
| 把上述数据编译成原生零解析格式 `.hzp` | ✅ 编译期完成，运行时不再解析 JSON |
| 用 Rust 引擎渲染、碰撞、行走、存读档 | ✅ 已实现（区块调色板压缩、面剔除、AABB） |
| 贴图采样、挖方块、放方块 | ✅ 全局贴图表 → `texture2d_array`，DDA 体素射线拾取 |
| 增量网格 + 贴图动画（`.mcmeta`） | ✅ 只重烘焙被改动的分区；岩浆/火焰按 tick 换帧，`interpolate` 支持交叉淡入 |
| 天空光 / 方块光 / 顶点 AO | ✅ 桶排序 BFS 双通道，烘焙进顶点色；昼夜可切 |
| 声音目录 / 衰减 / 播放队列 | ✅ `sounds.json` → `SND1`；Ogg 由 vendored `stb_vorbis` 解码成 PCM 交给 AVAudioEngine |
| 着色器包 | ✅ GLSL 子集→MSL 翻译 + 导入期入库 + 运行时 `makeLibrary` 接管渲染（失败回退内建管线） |
| 解析 `.class` 字节码、统计剩余 Java、生成 Rust stub | ✅ `hz classes` / `hz stubs` |
| **执行或 JIT 翻译 Java 字节码** | ❌ iOS 没有 JVM，且禁止 JIT 与 `fork/exec`（见 [docs/IOS_CONSTRAINTS.md](docs/IOS_CONSTRAINTS.md)） |
| 在设备上把 2 万个 Java 类自动翻成可运行的 Rust | ❌ 需要人工/AI 逐类移植；工具产出清单与骨架 |

所以「把 Java 版编译成 Rust」在这里分两层：

1. **数据层（设备内、已实现）**：jar 里所有*数据*被编译成 Rust 引擎直接消费的原生描述符——这是 Horizon 真正让游戏跑起来的那一半。
2. **行为层（离线、工具已就绪）**：`hz classes` 给出调用图与字节码体量，`hz stubs` 为每个类生成带 JVM 描述符的 Rust 模块骨架，由人或编码代理逐步补全，产物再合回引擎。

## 快速开始

```bash
# 引擎
cd engine
cargo test                     # 36 个测试
cargo build --release
./target/release/hz selftest   # 端到端：合成 jar → 编译 → 起引擎 → 出帧

# 真实 jar
./target/release/hz port ~/.minecraft/versions/26.3/26.3.jar out/26.3.hzp --version 26.3
./target/release/hz info out/26.3.hzp
./target/release/hz classes ~/.minecraft/versions/26.3/26.3.jar | head -c 2000
./target/release/hz shader-check shaderpack.zip     # 校验着色器包接口
./target/release/hz shader-translate shaderpack.zip out.metal   # 子集翻译
./target/release/hz sound-check some.ogg              # 解码并打印 PCM 统计
./target/release/hz stubs   ~/.minecraft/versions/26.3/26.3.jar stubs/
```

macOS 上构建 iPad 应用：

```bash
./scripts/build-ios.sh
cd app && xcodegen generate && open HorizonPad.xcodeproj
```

Windows 上验证管道（不需要 Xcode）：

```powershell
pwsh -File scripts/verify-pipeline.ps1   # 造 jar → 解包比对 → 编译 → 加载 → 量帧
python scripts/swift_check.py            # 结构检查（无 toolchain 也能跑）
bash scripts/swift_typecheck.sh          # parse 全部 + 类型检查 FFI/模型层
```

## 仓库结构

```
engine/                    Rust workspace，零三方依赖
  crates/hz-core            inflate / zip / json / nbt / ogg / sha1+sha256 / .hzp 容器
  crates/hz-class           JVM .class 解析、字节码调用图、Rust stub 生成
  crates/hz-vorbis          stb_vorbis 的 FFI 封装（vendored C，SHA-256 固定）
  crates/hz-port            jar → .hzp 流水线（方块/模型/贴图/动画/声音/着色器）+ 报告
  crates/hz-game            区块、注册表、玩家物理、分区网格缓存、动画、光照、声音、射线、.hzw 存读档
  crates/hz-ffi             Swift 用的 C ABI（staticlib）
  crates/hz-cli             hz 命令行（port/info/classes/stubs/unzip/hash/selftest）
  include/hz_ffi.h          C 头文件，与 hz-ffi 一一对应
app/                        iPadOS 应用
  HorizonPad/Engine         C ABI 封装、Metal 渲染器、触摸控制、会话时钟
  HorizonPad/Views          SwiftUI 界面
  HorizonPad/Import         导入与编译流程
  HorizonPad/Model          资源包库、设置
  HorizonPadTests           Swift↔Rust 接缝测试
docs/                       架构、iOS 限制、端口管线、格式、API、构建
scripts/                    构建与验证脚本
```

## 状态

| 模块 | 状态 | 测试 |
|---|---|---|
| inflate / ZIP | 完成 | 用 PowerShell 生成的真实 DEFLATE 归档逐文件哈希比对 |
| JSON / NBT / SHA | 完成 | 标准向量 + 往返 |
| blockstates / models | 完成（variants + multipart、parent 链、元素旋转） | 单测 |
| `.hzp` 容器 | 完成 | 往返 + 清单校验 |
| `.hzw` 存档 | 完成 | 往返（含稠密回退） |
| Rust 引擎 | 完成（区块/物理/剔除/射线/出帧） | 单测 + 自检 646 面 |
| 贴图管线 | 完成（模型面 → 全局贴图表 → 顶点携带下标 → `texture2d_array`） | `global_texture_table_merges_and_dedupes` + bench 实测 400 槽 |
| 方块交互 | 完成（DDA 射线、挖、放、拒绝埋人） | 4 个交互单测 |
| 增量网格 | 完成（分区缓存 + 边界失效 + 增量==全量逐字节相等） | 3 个缓存单测 |
| 贴图动画 | 完成（`.mcmeta` → `AN1`/`AN2`，逐帧跳变 + `interpolate` 交叉淡入） | 端口 + 运行时共 9 个单测 |
| 光照 | 完成（天空光零衰减竖井、方块光衰减、玻璃透光、AO、昼夜、窗口化重算） | 7 个光照单测（含窗口化 == 全场重算的逐格比对） |
| 声音 | 完成（`sounds.json` 编译、Ogg 容器/校验和、变体轮换、距离衰减、队列、**stb_vorbis 解码**) | 端口 5 + 容器 6 + 播放 7 + 解码 4 个单测（含真实 Vorbis 解码） |
| 着色器包 | 完成（接口解析、阶段链接、uniform 能力、GLSL 子集→MSL、入库、运行时编译接管、失败回退） | 15 个翻译单测 + 端口/注册表用例；MSL 由 macOS CI 用 `xcrun metal` 真编译 |
| `.class` 解析 | 完成（常量池、Code、invoke 目标） | 合成 class 单测 |
| SwiftUI + Metal | 21 个文件 `swiftc -parse` 通过；FFI 与模型层 `-typecheck` 通过 | 结构体检 + 类型检查 + ABI 对齐 |

Rust 侧共 **63 个测试**，`clippy -D warnings` 与 `cargo fmt --check` 均干净。

### 顶点缓冲：整帧拷贝 vs 分区上传（Windows，release）

| 指标 | 整帧路径 | 分区路径 |
|---|---|---|
| 空闲帧上传 | 2.6 MiB / 帧（60 fps ≈ 162 MB/s） | **0 字节** |
| 改一个方块 | 2.6 MiB | **≈ 0.5 MiB**（该分区 + 跨边界邻居） |
| 重建耗时 | 0.7 ms | 0.7 ms（省的是带宽，不是 CPU） |

### 实测数字（Windows，release）

基准 jar：7,403 条目（2,000 方块状态 × 2 variant、2,001 模型、2,000 PNG、2 份语言表、200 配方、1,200 个真实 class）

| 指标 | 结果 |
|---|---|
| `hz port` 耗时 | **0.083 s** |
| 产出 `.hzp` | 2,008,866 字节 / 6,204 条目 |
| 数据覆盖度 | **100%**（0 失败） |
| JVM 清点 | 1,200 类 / 1,200 方法 / 1,200 调用边，热点 `Target.run` ×1200 |
| `hz stubs` | 1,200 个 Rust 模块，0 跳过 |
| `hz info`（构造 registry） | 2,001 blocks / 2,001 models / 12,006 quads / 2,001 textures |
| 可重复性 | 同一 jar 两次编译 SHA-256 相同 |

`hz info` 会对包做**真正的加载检查**（构造 registry）：同一份基准包给出
`blocks 2001 / models 2001 / quads 12006 / textures 2001（其中 2000 有 PNG）`。

`hz bench` 量三种代价（Windows 桌面，release；400 种方块 × 400 张不同贴图，铺 2,560 块）：

| 场景 | 结果 |
|---|---|
| 一帧规模 | 7,206 面 / 43,236 顶点 / 2.6 MiB |
| 冷启动全量（光照 + 烘焙） | 6.7 ms |
| **空闲帧** | **0.01 µs**（200 次里 0 次做了工作）|
| 改一个方块（窗口化重算 + 重烘焙 + 拼帧） | **0.87 ms** |
| 其中窗口化光照 | ~0.8 ms |
| 全场重算光照 + 重烘焙（世界变大时退回） | 2.1 ms |

光照从「每次编辑全场重算」改成「只算编辑点 ±16 格」，编辑路径 **1.6 ms → 0.87 ms**。
剩下的时间主要花在两处：窗口内的 BFS（17k 格），以及整帧 2.7 MiB 的顶点拷贝。
基准世界只有 64×64×16，窗口占了三分之一，所以相对收益被低估了——世界越大，窗口占比越小。
下一步若要再压，得把顶点缓冲改成按分区，省掉整帧拷贝。

`hz selftest` 在合成包上出 646 个面、玩家落到 y=1.00。

Swift 与 `.metal` 代码按 iPadOS 27 的公开 API 写。**没有 Apple SDK 的部分已经用编译期手段查过了**：
21 个文件全部通过 `swiftc -parse`；`HZEngine.swift`（400 行 FFI 封装）与模型层通过 `-typecheck`
（后者对着一个 Combine 桩模块）。这抓出了三个真问题：两处漏 `import Combine`、一处元组 key path。
SwiftUI/Metal 的 API 误用仍只有 macOS 的 `xcodebuild` 能证明——CI 里那道 job 就是干这个的，
`docs/BUILD.md` 另有首次打开时的检查清单。

## 许可

MIT OR Apache-2.0（仅本仓库代码）。Minecraft 是 Mojang 与 Microsoft 的商标，本项目与其无关联。