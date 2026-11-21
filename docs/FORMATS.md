# 二进制格式

所有整数小端。所有字符串是 `u16` 长度前缀的 UTF-8。三个格式都有魔数，读的时候先验魔数。

## `.hzp` — 资源包

```
offset 0    "HZP1"
+4          u16 version        (= 1)
+6          u16 reserved       (= 0)
+8          u32 entry_count
+12         u32 index_bytes
+16         index[entry_count]
+16+index   blob
```

索引项：

```
u8   kind            1 blockstates  2 model  3 lang  4 texture  5 data
                     6 mixin        7 manifest  8 sound  9 shader
u16  name_len
     name            "blockstates/minecraft:stone"
u8[32] sha256        条目内容的 SHA-256
u32  offset          blob 内偏移
u32  length
```

加载时 `offset + length` 越界即拒绝整包。条目按写入顺序排列，没有时间戳，因此同一输入两次编译字节一致。

## `BS1` — 方块状态表

```
"BS1"
u32 state_count
per state:
  u8  rot_x / 90        (0..3)
  u8  rot_y / 90        (0..3)
  u8  flags             bit0 uvlock, bit1 multipart 条目
  u8  weight            variants 的权重（1..255）
  u16 prop_count
      (str16 name, str16 value) × prop_count
  str16 model_id        "minecraft:block/stone"
```

`multipart` 条目把条件 JSON 塞进唯一的 `("$when", <json>)` 属性对，由引擎判断是否显示。
解析阶段会把这条 `$when`/`$model` 元属性摘出来，不留在属性表里。

## `MD2` — 烘焙后的模型

```
"MD2"
u16 texture_count
    (str16 key, str16 resolved_texture) × texture_count   键按字典序
u16 quad_count
per quad:
  u8  face          0 up 1 down 2 north 3 south 4 west 5 east
  u8  cullface      0 = 不剔除，其余为 face+1
  i16 tint          -1 = 无 tintindex
  u8  rotation      面 UV 旋转角度（0/90/180/270）
  u16 texture_slot  该模型 texture 表里的下标；65535 = 无贴图
  f32[12]           4 个角点 × (x, y, z)，方块局部 0..1
  f32[8]            4 个角点 × (u, v)
```

角点已包含元素级 `rotation`（绕 `origin`、绕指定轴的烘焙），已从 0..16 缩放到 0..1。
顺序为逆时针（从外侧看），渲染器用 `.counterClockwise` + 背面剔除。

`texture_slot` 是**模型内**下标。加载时 `Registry` 把所有模型的解析结果合并成一张全局表
`texture_ids`（首次出现即编号、重复去重），把每个 `Quad::texture` 改写成全局下标，
渲染器据此上传 `texture2d_array`。没有贴图的面保留 65535，着色器回落到顶点色。

`MD1` 是本格式的上一个版本（无 `texture_slot`），引擎只读 `MD2`，遇到旧包会明确报错。

## `AN1` — 贴图动画描述符

`.png.mcmeta` 里的 `animation` 段编译而成。贴图本身不切分（切分要解码再编码 PNG），
引擎在烘焙 UV 时按帧压缩 V：

```
"AN1" | u16 frametime(ticks) | u16 frame_count | u16 indices[frame_count]
"AN2" | u16 frametime(ticks) | u16 frame_count | u16 flags | u16 indices[frame_count]
```

`AN2` 多一个 `flags`，bit0 = `interpolate`。`AN1` 仍可读（插值关闭），
所以插值出现之前编译的包不用重导。

- 插值不是模糊：顶点额外携带「下一帧的 V 偏移」和「混合系数」，着色器采样两次再 `mix`。
  nearest 采样保证像素仍然硬边，交叉淡入负责过渡。
- `frametime` 默认 1 tick（20 帧/秒）
- `frames` 里的 `{"index":i,"time":t}` 会把该帧**重复 t 次**，这样运行时只需要一个统一 frametime
- 没有 `frames` 时（原版按 PNG 高度推断帧数），这里只记一帧：宁可按静态显示，也不猜错
- `frame_at(tick) = indices[(tick / frametime) % frame_count]`
- 烘焙时 `v' = v / frame_count + frame / frame_count`
- 插值时 `blend = (tick % frametime) / frametime`，`v_delta = (next - current) / frame_count`；
  `next` 是帧序列里的下一个（不是 `current + 1`，因为帧序可以是任意排列）

密钥性质：帧推进只会把**用到该贴图的分区**标脏（`SectionMesh::animated` 记录了这个集合），
不会触发全场景重烘焙。

## `SND1` — 声音目录

`sounds.json` 编译而成。Ogg 载荷本身另存为 `sounds/<id>` 条目，逐字节保留。

```
"SND1" | u16 event_count
per event:
  str16 name | u16 variant_count
  per variant:
    str16 resource_id   （即包里 sounds/<id> 的 id）
    u8   weight
    u8   flags          bit0 流式, bit1 有字幕
    f32  volume
    f32  pitch
```

## `SHD1` / shader MSL — 着色器包

`.`hzp` 里一个包对应两条条目：

| 条目名 | kind | 内容 |
|---|---|---|
| `shaders/<name>` | 9 | `SHD1` 接口描述符（阶段、输入输出、uniform、缺口） |
| `shaders-msl/<name>` | 11 | 导入期翻译好的 Metal 源码（UTF-8），没有则说明被拒绝 |

运行时用 kind 9 解释「这个包要什么」，用 kind 11 直接交给 `device.makeLibrary`。

## `SHD1` — 着色器包描述符

GLSL 源码**不**入包（翻译器还没做）；入包的是校验过的接口描述。

```
"SHD1" | u16 pack_count
per pack: str16 name | u16 stage_count
  per stage: str16 kind(vertex|fragment)
             u16 in | (str16 type, str16 name)*
             u16 out | (str16 type, str16 name)*
             u16 uniforms | (str16 type, str16 name)*
then: u16 requires_count | str16 each   （引擎不提供的 uniform 名）
```

## `.hzw` — 存档

```
"HZW1"
u16 version      (= 1)
u32 section_count
per section:
  i32 x, i32 y, i32 z            区块坐标（世界坐标 / 16 向下取整）
  u8  bits                       0 = 稠密数组，4/8/16 = 调色板位宽
  u16 palette_len
      u16 palette[palette_len]
  u32 word_count
      u64 words[word_count]
  u32 dense_len
      u16 dense[dense_len]
trailer:
  f32 x, f32 y, f32 z            玩家位置
  f32 yaw, f32 pitch
  u64 ticks
```

`bits == 0` 时使用 `dense`（16384 个 u16），否则用调色板 + 打包字。
两种形态读回时都校验数量，任何不匹配都是错误，不做部分加载。

## 顶点流（`hz_game_frame_data`）

每顶点 16 个 `f32` = 64 字节（16 字节对齐），无索引，每面两个三角形（6 顶点）：

```
px py pz | u v | nx ny nz | r g b a | texture | v_delta | blend | 0
```

- `texture` 是 float 形态的全局贴图下标；65535 表示这面没有贴图
- `v_delta` / `blend` 给插值动画用：着色器取 `uv + (0, v_delta)` 的第二个样本再按 `blend` 混合。
  非动画面这两个值是 0，第二个样本落在同一像素上，等于多一次取样而已
- 末尾 1 个 float 是填充，保证 `stride = 64`
- Metal 侧 `MTLVertexDescriptor` 的 5 个 attribute 与之对应（位置/UV/法线/颜色/贴图下标）

采样用 `nearest`（方块贴图是像素画），`MTLSamplerAddressMode.clampToEdge`，
alpha < 0.35 直接 `discard_fragment()`——这是树叶、玻璃、草方的镂空行为。

## 为什么不用压缩

`.hzp` 不压缩是刻意的：

- 解压一次要 CPU，而 iPad 上的导入是一次性、播放是多次；
- 单文件可直接按名索引，无需解压整个包；
- 体积换来的启动速度明显（贴图本来已经是 PNG 压缩态）。

`.hzw` 同样不压缩：区块数据在内存里就是这个形状，存档等于直接落盘。