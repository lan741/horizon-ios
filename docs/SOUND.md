# 声音

## 管线

```
assets/<ns>/sounds.json          → hz-port::sounds  → SND1 目录（事件 → 变体）
assets/<ns>/sounds/**.ogg        → 校验 Ogg 容器   → kind::SOUND 原样入包
```

`SND1` 只描述「事件名 → 资源 id / 权重 / 音量 / 音高 / 是否流式」。
资源 id 就是包里的 `sounds/<id>` 条目名，所以运行时不解析 JSON、不猜文件名。

## iOS 上没有解码器，所以引擎自带一个

**AVFoundation 不解码 Ogg Vorbis。** Minecraft 的音效全是 Ogg。所以解码放在引擎里，
而不是指望平台：

```
hz_game_sound_data ──► hz_pcm_decode（vendored stb_vorbis）
                          │  交错 f32、归一化到 -1..1
                          ▼
                     AVAudioPCMBuffer ──► AVAudioPlayerNode
```

能原生播放的格式（WAV / AIFF / CAF / MP3 / AAC）仍走 `AVAudioFile`，
所以用户自己塞进去的音效也能用。若某个构建没链上解码器（`hz_vorbis_available() == 0`），
Ogg 会被计数并点名报出来——不会静默。

## vendored `stb_vorbis`

| 项 | 值 |
|---|---|
| 来源 | `raw.githubusercontent.com/nothings/stb/master/stb_vorbis.c` |
| 版本 | v1.25，单文件，public domain |
| 大小 | 199,371 字节 |
| SHA-256 | `97a8febc738e6a29fafc8d8bc557c7f17ad3db7ba6544485f59395cc6008b764` |
| 编译 | `crates/hz-vorbis/build.rs`，仅用 `std`：找 `clang`/`cc`，`-c -O2 -DNDEBUG`，再 `llvm-ar`/`ar` 归档 |
| 校验 | 编译前比对 SHA-256；不匹配就不带解码器构建，并发出 `cargo:warning` |
| iOS | `build.rs` 对 `aarch64-apple-ios` 追加 `-target arm64-apple-ios -isysroot $(xcrun --show-sdk-path)` |

这**不引入 crates.io 依赖**：`build.rs` 只用标准库，C 文件是 vendored 的。
代价是构建机器上需要一个 C 编译器；没有的话构建仍然成功，只是解码器不可用。

## 验证方式

```bash
hz sound-check engine/tests/data/tiny.ogg
```

```
container: vorbis (9 pages, 296 packets, checksums_ok=true)
header:    44100 Hz, 2 channel(s), nominal 160003 bps
decoded:   269188 frames, 2 channel(s), 44100 Hz, 6.10 s
levels:    peak 0.147, rms 0.020
```

容器信息来自项目自己的 `hz-core::ogg`，解码结果来自 stb_vorbis——两者对采样率/声道数的
一致本身就是一个交叉校验。测试夹具是一个 104 KB 的公共领域 Vorbis 文件
（SHA-256 `f57b56d8…`），`cargo test -p hz-vorbis` 会真的解码它并断言帧数、归一化和非静音。

## 引擎侧的约定

- **变体选择**：按权重轮换（游标递增取模），不用 RNG，所以行为可测、可复现。
- **衰减**：16 格内线性（`1 - d/16`），与大多数原版事件一致；超出即不排队。
- **队列上限** 64：平台不取走时不会无限增长。
- **流式标记**（`stream: true`）目前只存不用：现在是一次解码整段；流式播放要配合分块解码。

## Ogg 解析做了什么

`hz-core::ogg` 实现 RFC 3533 的页解析：捕获串校验、段表、包重组、CRC 校验
（多项式 0x04C11DB7，非反射、零初值、不取反——和 ZIP/PNG 的 CRC 不是一回事），
以及 Vorbis 识别头（声道、采样率、码率、块大小）。

校验和不匹配不会拒绝文件，只把 `checksums_ok` 置假并让导入期发出告警：
一个能播但校验不过的文件，比一个被拒绝的文件更有用。
