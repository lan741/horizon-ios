# 架构

三层，依赖单向向下，每一层都能单独测试。

```
SwiftUI 视图 / 触摸控制        app/HorizonPad/Views, Engine/MetalGameView.swift
        │  (Swift, @MainActor)
        ▼
Swift 封装：会话时钟 / Metal    app/HorizonPad/Engine/{HZEngine,GameSession,Renderer}.swift
        │  (C ABI, hz_ffi.h)
        ▼
Rust 引擎                       engine/crates/hz-{core,class,port,game,ffi}
        │  (纯 Rust)
        ▼
设备文件系统（应用容器）
```

## 为什么是这三个货真价实的分界

1. **Metal 之下不再有平台代码。** 引擎不知道 Metal、UIKit、Swift 的存在，因此同一份
   `hz-game` 也能被 `hz-cli` 在 Windows/Linux 上跑测试——`hz selftest` 就是这么工作的。
2. **C ABI 是唯一的跨语言面。** 13 个函数、一个回调、一个线程局部错误缓冲。
   Swift 侧不存在任何 `unsafe` 之外的技巧，也不需要在 Rust 里写 Swift 类型。
3. **视图层没有引擎指针。** `GameSession` 持句柄，视图只读 `stats`、只发输入。

## 数据流（一次导入）

```
fileImporter → 安全作用域 URL
   → Paths.importCopy（拷进容器，避免授权过期）
   → Engine.queue.async { Porter.port(...) }         ← 不阻塞主线程
        → hz_port_jar → 每完成一个阶段回调一次进度
   → PortOutcome（pack 摘要 + 报告 JSON）
   → Library.add(InstalledPack)                      ← 落 library.json
   → 界面上出现卡片
```

进度回调从 Rust 线程直接跳回 `@MainActor`，所以 UI 上的阶段名与百分比是引擎真实状态，
不是假动画。

## 数据流（一帧）

```
GameSession.step()  (60 Hz Timer)
   ├─ 累积时间到 1/20 s → 输入换算 → hz_game_set_player → hz_game_tick
   ├─ 有 tick 才重算：hz_game_rebuild_frame → hz_game_frame_data (拷贝 f32 数组)
   └─ renderer.submit(vertices:)
MTKView.draw()      (独立回调)
   └─ 上传顶点缓冲 → 一次 drawPrimitives(triangle)
```

引擎重建帧是**同步且完整**的：没有增量网格缓存（那是下一步）。区块小、绘制半径 1 时，
一帧几万个顶点在现代 iPad 上毫无压力；这也让「面剔除对不对」可以被单测直接断言。

## 贴图与交互

- 模型编译期：每个面解析 `#alias`，落到「该模型 texture 表」的下标。
- 加载期：`Registry::build` 把各模型的下标合并成一张**全局贴图表**（首次出现编号、重复去重），
  顺手把每个 `Quad::texture` 改写成全局下标，并把对应 PNG 挂在 `texture_pngs` 上。
- 出帧期：顶点第 13 个 float 就是全局下标；Swift 侧把全部 PNG 解成一片
  `texture2d_array`，着色器用 `atlas.sample(sampler, uv, slice)` 采样。
- 交互：`Game::raycast` 是 Amanatides & Woo 体素遍历，返回方块坐标、进入面的法线、距离。
  法线即放置偏移；`place_block` 会拒绝与玩家胶囊重叠的位置，避免把自己埋了。

## 分区网格缓存（增量烘焙）

```
set_block ──► 标脏：所在分区 + 跨边界时的邻居分区
tick      ──► 动画帧推进 ──► 只标脏用到该贴图的分区
rebuild_frame
   ├─ needs_work(view)？  否 → 立刻返回 false（空闲帧 0.03 µs）
   ├─ 重烘焙脏分区（复用 Vec，不清空重分配）
   └─ assemble：按 key 排序拼接可见分区 → 顶点流
```

`ViewKey`（玩家所在方块 + 绘制半径）变化也会触发重新拼接，所以跨区块移动不会漏画。
拼接顺序按分区 key 排序，保证同一世界两次构建产出字节相同的帧。

## 贴图动画

`.mcmeta` 在导入期变成 `AN1`/`AN2` 描述符；烘焙时按 `tick / frametime % frames` 压缩 V。
帧推进只标脏 `SectionMesh::animated` 里记过的分区——一片岩浆不会让整张地图重烘焙。

带 `interpolate: true` 的纹理是例外：混合系数每 tick 都在变，所以每个 tick 都要重烘焙
（和原版一致，原版也是每 tick 更新 sprite）。做法是把「下一帧的 V 偏移 + 混合系数」写进
顶点，着色器两次 nearest 采样再 `mix`——像素保持硬边，过渡是交叉淡入而不是模糊。

## 光照

两路 0..15 的通道，和原版一致：

| 通道 | 种子 | 传播 |
|---|---|---|
| 天空光 | 每列最高不透明方块之上全是 15 | **向下不衰减**（竖井到底还是亮的），其余五向 −1 |
| 方块光 | 发光方块按其亮度（火把 14、萤石 15…） | 六向都 −1 |

实现是桶排序 BFS（15 个桶，按亮度从高到低处理），不需要优先队列。
天空光的种子只把「六邻里有更暗的」那些格子入队——开阔天空下这个集合接近空，
这是把「每格都入队」从 6× 队列长度里救回来的关键（实测过，不这么做慢 4 倍）。

顶点着色：面片取**面前那一格**的亮度（原版行为），四个角各乘自己的 AO 系数。
AO 用经典的「两侧 + 对角」三邻规则，系数 1.0 / 0.86 / 0.72 / 0.58。

遮挡与发光来自 `Registry`：

- `is_opaque`：空气、玻璃、水、冰透光；**发光方块一律不挡光**（火把不能吞掉自己的光）
- `light_emission`：名字表（火把 14、萤石/岩浆/南瓜灯 15、哭泣黑曜石 10…）。
  原版把这些放在 Java 类里，等行为层移植后可以换成真值；现在这张表是可读、可测的近似。

编辑后不再全场重算，而是**只算编辑点 ±16 格的窗口**（`relight_around`）：

```
窗口 = Bounds::around(edit, 16) 裁剪到已加载范围
  ↑ 16 是硬界：光每步至少 -1，编辑不可能影响更远
边界 = 窗口外壳中「那一面不在世界边缘」的格子，用旧场的值做种子，并入队参与传播
内部 = 完整重算，逐格 blit 回去，顺手记录哪些分区真的变了
世界变大（出现新分区）→ 退回全场重算，因为旧场没覆盖新区域
```

三个坑都是测试逼出来的，记在这里免得重犯：

1. **边界种子必须进队列**。只写进数组不推入桶队列，边界光就不会向窗口内部传播。
2. **被世界边界裁剪的外壳不能当种子**。裁剪后它离编辑点可能只有 10 格（<15），
   那里的光仍会被编辑改变，却因为「是外壳」而不写回，结果与世界边缘的暗值不一致。
   判据改成「这一面是否与已加载范围的边界重合」。
3. **`is_opaque` 的顺序**。改成按 id 的稠密表后，空气不再是「提前 return false」，
   而名称表里空气不匹配任何透明条目 → 被判成实心 → 全世界变黑。空气固定是 id 0，
   表里显式排除。

稠密表（`opacity`/`emission` 两个 `Vec<u8>`）本身也是优化：烘焙时每个顶点要做 3 次 AO
查询、重算光照时每格 6 次邻居查询，原来每次都要做一次 `registry.name()` 的字符串切分。

天空光与方块光都烘焙进顶点色，所以昼夜滑块（`set_daylight`）不需要改着色器，
只是让所有分区失效一次。

## 应用内下载游戏版本

```
选版本 ──► 镜像取 version_manifest_v2.json
             │  decode → 版本列表（release / snapshot 分开）
             ▼
          取 <version>.json ──► client.jar URL + assetIndex URL
             │  镜像重写：piston-meta / piston-data → 镜像根
             ▼
          取 assetIndex ──► objects{path → hash,size}
             │  只挑 port 需要的：blockstates / models / block+item 贴图 /
             │  语言 / sounds.json（音效与着色器可选）
             ▼
          按 hash 并发取文件（8 路），逐个校验 SHA-1
             │  镜像对象 URL：resources.download.minecraft.net → 镜像/assets
             ▼
          StoreZipWriter 打成 assets/minecraft/... 的 zip（不压缩）
             ▼
          hz_port_jar（与手动导入完全同一条路径）→ .hzp → Library
```

三点设计取舍：

* **镜像只是 URL 重写**，不是第二套 schema。BMCLAPI 在同路径下服务，所以版本信息和每个
  对象 URL 都能机械改写；失败时再回落到官方源，一次失败请求的代价换一个不会卡住的界面。
* **对象是内容寻址的**，所以失败按对象重试、按 SHA-1 校验；坏一个贴图不会变成一个坏包。
* **zip 用 store 不压缩**：下载已经花掉网络，几百 MB 的 PNG 压缩率只有几个百分点，
  却要付出一次完整 CPU 往返。固定时间戳让两次运行字节一致，符合引擎的确定性要求。

## 顶点缓冲分区上传

原来是「所有可见分区的顶点拼成一条流，整条拷给 GPU」：每次重建都要 memcpy 2.6 MiB，
即使只改了一个方块。

现在是按分区各自持有 GPU 缓冲：

```
rebuild_meshes()            只重烘焙脏分区，不拼整帧
   ↓ 返回 revision（0 = 什么都没变）
visible_sections()          (x, y, z, vertex_count, generation) 有序列表
   ↓
Renderer.syncSections()     逐分区比较 generation
   ├─ 变了 → hz_game_section_vertices → 上传该分区
   └─ 没变 → 沿用上一帧的缓冲
   ↓
drawList 逐缓冲 drawPrimitives
```

**generation** 是每次烘焙递增的序号。渲染器只认这个号，因此：
- 空闲帧上传 **0 字节**；
- 改一个方块只上传它所在分区（以及跨边界时受影响的邻居）。

`hz_game_rebuild_frame` / `hz_game_frame_data` 仍然保留：它们是基准测试、
首帧回退和「需要一张完整快照」的场景用的。两条路径共享同一份分区网格，
`section_vertices_add_up_to_the_flat_frame` 断言两者字节数一致。

## 声音

引擎只决定「播什么、多大声」，不碰音频设备：

```
Game::play_sound(事件, x, y, z)
   ├─ 事件名 → 变体表，按权重轮换（不用 RNG，行为可测）
   ├─ 距离衰减 1 - d/16，超出不排队
   └─ 压入有界队列（64）
Swift 每 tick 调 hz_game_next_sound 取空队列 → AVAudioPlayerNode
```

这样 `hz-game` 保持无平台依赖（Windows 上可测）。Ogg Vorbis 的解码放在独立 crate
`hz-vorbis`（vendored `stb_vorbis` + 纯 std 的 build.rs），因为它需要 C 编译器，
不该拖累引擎本身。Swift 侧只负责把 PCM 变成 `AVAudioPCMBuffer`。详见 [SOUND.md](SOUND.md)。

## 着色器包

端口阶段解析 GLSL 源码的**接口**（只在花括号深度 0 收集声明，剥离注释与字符串），
校验阶段间 `out`/`in` 的名字与类型是否配对、uniform 是否在引擎能力内，然后编译成
`SHD1` 描述符入包。运行时 `Registry.shaders` 解出这些信息供界面展示。

渲染器不执行自定义着色器：GLSL→MSL 的翻译是独立的一步，见 [SHADERS.md](SHADERS.md)。
界面会明确写出「接口超出引擎能力，不会被执行」，而不是假装加载成功。

## 关键不变量

| 不变量 | 谁保证 | 怎么验证 |
|---|---|---|
| 空气永远是方块 id 0 | `Registry::build` 强制把 air 换到 0 号位 | `registry::tests::builds_from_pack` |
| 顶点流长度 = 面数 × 6 × 16 | `Frame::push_quad` | `game::tests::world_and_frame` |
| 每个面的贴图下标都在全局表内 | `Registry::build` 合并时改写 | `global_texture_table_merges_and_dedupes` |
| 射线命中的法线指向射线来向 | `Game::raycast` | `raycast_hits_the_pad_and_reports_the_face` |
| 增量烘焙 == 一次性全量烘焙 | `MeshCache` + `invalidate_around` | `edit_invalidates_..._matches_full_bake`（逐字节比较顶点流） |
| 空闲帧不产生任何工作 | `MeshCache::needs_work` | `idle_rebuild_reports_no_change` |
| 动画帧推进只重烘焙相关分区 | `SectionMesh::animated` | `animated_texture_changes_uvs_as_ticks_advance` |
| 火把亮度按 1/格 衰减，墙能挡住 | `light::compute` 桶排序 BFS | `torch_lights_a_room_with_falloff` |
| 天空光竖井不衰减、玻璃透光、石头不透 | 天空光的「向下零成本」规则 + `is_opaque` | `skylight_keeps_its_level_down_a_shaft`、`glass_transmits_...` |
| 夜间只压天空光，方块光不变 | `LightField::brightness` | `daylight_scales_sky_but_not_block_light` |
| AO 让角落真的更暗 | `face_colors` + `vertex_ao` | `ambient_occlusion_darkens_the_inner_corner` |
| 声音在 16 格外不排队、8 格处半音量 | `SoundCatalog::attenuation` | `sounds_attenuate_with_distance_and_queue` |
| 声音变体轮换且不重复 RNG | `SoundCatalog::request` 游标 | `requests_rotate_through_variants` |
| 播放队列有界、先进先出 | `SoundQueue` | `queue_is_bounded_and_fifo` |
| 只有被编辑的分区重烘焙 | `SectionMesh.generation` | `per_section_path_only_changes_the_edited_section` |
| 空闲帧不做任何工作 | `rebuild_meshes` 返回 0 | `idle_section_path_reports_no_work` |
| 分区字节之和 == 整帧字节 | `visible` 与 `assemble` 共用投影 | `section_vertices_add_up_to_the_flat_frame` |
| 真实 Vorbis 文件能解码且归一化 | `hz_vorbis::decode` + stb_vorbis | `decodes_a_real_vorbis_file`（夹具 6.10 s / 2ch / 44.1 kHz） |
| 着色器接口只在深度 0 收集 | `parse_stage` | `parses_the_interface_not_the_body` |
| 阶段 `out`/`in` 名字与类型必须配对 | `shader::compile` | `mismatched_stage_interface_is_reported` |
| 未知 uniform 变成需求项而非静默失败 | `PROVIDED_UNIFORMS` 对照 | `an_unknown_uniform_becomes_a_requirement` |
| `.hzp` 条目偏移不越界 | `Pack::read` 逐条检查 | `pack::tests::roundtrip` |
| ZIP 条目 CRC 必须匹配 | `Zip::read_index` | `zipfile` + 脚本端哈希比对 |
| 存档能原样读回 | `.hzw` 版本号 + 尺寸校验 | `world_io::tests::save_load_roundtrip` |
| 引擎 ABI 与头文件一致 | `hz_api_version()` | `EngineABITests.testAPIVersionMatchesHeader` |

## 错误处理

Rust 侧统一 `Result<_, String>`；FFI 把消息塞进线程局部缓冲，返回 1。
Swift 侧 `throw EngineError.*`，最终由 `AppModel.lastError` 弹一个 alert。
**没有 `unwrap` 跨越 FFI 边界**：`hz_game_frame_data` 在容量不足时返回 0 而不是越界写。

## 线程模型

| 工作 | 线程 |
|---|---|
| 导入/编译 | 私有串行队列 `dev.playhorizon.pad.engine` |
| 模拟 tick、重建帧、顶点拷贝 | 主线程（`@MainActor GameSession`） |
| Metal 编码 | MetalKit 的绘制回调 |
| 共享状态 | `Renderer` 内一把 `NSLock` 保护待上传顶点 |

一次会话一个 `HzGame *`，`deinit` 释放；`GameSession.stop()` 幂等，切到后台即停。