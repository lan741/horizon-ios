# 端口管线：jar 里的什么被编译，什么没有

## 被编译（设备内，`hz_port`）

| jar 路径 | 处理 | 产物（`.hzp` 内） |
|---|---|---|
| `assets/<ns>/blockstates/*.json` | `variants` 与 `multipart` 都解析；`x`/`y` 归一为 90° 倍数，`uvlock`、`weight` 保留 | `blockstates/<ns>:<id>`，格式 `BS1` |
| `assets/<ns>/models/**/*.json` | 递归合并 `parent`（上限 16 层），`textures` 解析 `#` 别名，`elements` 每个面烘成四边形并记录贴图槽，元素 `rotation` 绕 `origin` 烘焙进顶点 | `models/<ns>:<path>`，格式 `MD2` |
| `assets/<ns>/textures/**/*.png` | 校验 PNG 魔数后原样保留；被模型引用时编号进全局贴图表 | `textures/<ns>:<path>` |
| `assets/<ns>/lang/*.json` | 转成二进制字符串表（u32 数量 + `(key, value)` 的 u16 长度前缀） | `lang/<code>` |
| `data/**/*.json` | 重新序列化为规范 JSON（键序稳定、去 BOM），解析失败的按原字节保留 | `data/<相对路径>` |
| `*.png.mcmeta` | 解析 `animation`（frametime、frames、逐帧 time），编译成 `AN1` 描述符；PNG 不切分 | `anim/<ns>:<path>`，格式 `AN1` |
| `*.mixins.json` | 原样收进描述符表，供引擎查钩子站点 | `mixins/<path>` |
| `*.class` | **不执行、不产出**；解析后进入报告 | 仅出现在 `jvm` 统计与 `manifest` 的 `java_classes` |

最后还会写一条 `manifest`：格式版本、引擎 ABI、源 jar 的 SHA-1/SHA-256、覆盖度、条目计数。
引擎加载前会先读它——ABI 不匹配直接拒绝。

## 有意不编译的（写在这里免得被当成 bug）

| 内容 | 原因 | 谁来补 |
|---|---|---|
| `display` / `gui` 变换 | 手持与 GUI 的矩阵属于渲染器策略，烘进模型会限制引擎 | 渲染器按 `CameraUniforms` 统一处理 |
| 贴图动画的插值（`interpolate: true`） | 需要两帧之间按小数时间混合，得加一个混合 uniform | 待办；当前是逐帧跳变 |
| 物品模型 `overrides` 谓词 | 依赖运行时状态（耐久、附魔、NBT） | 引擎的 item 渲染层 |
| `sounds.json` 的音频流 | 需要 Ogg Vorbis 解码 + `AVAudioEngine`；留接口未实现 | 待办，格式已预留 `kind::SOUND` |
| 粒子/实体模型（`.json` 之外的 `.geo.json`） | 属于行为层 | 离线移植 |
| 着色器包 `.hshader` | 需要 Metal 着色器翻译器（GLSL→MSL） | 待办，`kind::SHADER` 已预留 |
| **Java 代码** | iOS 不能执行 Java（见 [IOS_CONSTRAINTS.md](IOS_CONSTRAINTS.md)） | `hz stubs` 生成骨架 + 人工/代理移植 |

## 覆盖度怎么算

```
carried = jar 中 blockstate + model + texture + lang + data + sound + mixin 的条目数
ported  = .hzp 中上述种类的条目数（不含 manifest）
coverage = min(ported, carried) / carried
```

刻意容易掉分：如果 ZIP 解析失败、或某类全部解析失败，`ported` 接近 0，覆盖度就是 0，
而不是「没检查就算通过」。空 jar 或纯代码 jar 覆盖度为 0。

报告字段（`*.report.json`）：

```json
{
  "engine_api": 1,
  "minecraft_version": "26.3",
  "jar_sha1": "...", "jar_sha256": "...",
  "jar_bytes": 265207, "jar_entries": 16, "pack_entries": 10,
  "coverage": 1.0,
  "counts": { "blockstate": 2, "model": 1, "lang": 2, "data": 3, "mixin": 1, "java_class": 1 },
  "java_classes": 1,
  "jvm": {
    "classes": 4201, "methods": 61830, "bytecode_bytes": 9123456,
    "call_edges": 402913, "class_file_versions": { "61": 4201 },
    "hot_targets": [ { "owner": "net/minecraft/...", "method": "...", "call_sites": 812 } ]
  },
  "unported": ["net/minecraft/world/level/Level.class", "..."],
  "warnings": [], "failures": []
}
```

## 行为层：从字节码到 Rust

`hz-class` 做三件事：

1. **常量池 + 方法表**：类名、父类、接口、每个方法的描述符、`Code` 长度、`max_stack`/`max_locals`。
2. **调用图**：走完整条字节码找 `invokevirtual/invokespecial/invokestatic/invokeinterface`
   的 `(owner, method)`。指令长度表覆盖到 Java 21，包括 `tableswitch`/`lookupswitch` 的 4 字节对齐、
   `wide`、`invokedynamic`。未知 opcode 会让本轮遍历停止并把该类计入解析失败，而不是错位解析后给出假数据。
3. **stub 生成**：每个非构造方法一个 `pub fn name(a0: i32, a1: Obj) -> Obj { todo!() }`，
   上方注释带原始 JVM 描述符、字节码字节数、前 16 个调用目标。

这样「还剩多少工作量」是可数的：方法数、字节码总量、被调用最多的目标。
移植一个类就是把 `todo!()` 换成真实逻辑，然后从 `unported` 列表里划掉。

## 幂等与可重复

- 同一 jar 两次编译产出的 `.hzp` **字节相同**（容器无时间戳，条目按 jar 中央目录顺序）。
- `manifest` 里的 `source_jar_sha256` 让「这份包到底由哪个 jar 编译」可追溯。
- `.hzw` 存档带版本号，加载时校验；不匹配就报错，不做静默降级。