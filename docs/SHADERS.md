# 着色器包

## 现状：接口校验 + 子集翻译

两件事都做了，边界划得很清楚：

1. **接口校验**（`shader::compile`）：阶段链接、`#version`、uniform 能力。
2. **子集翻译**（`glsl::translate`）：把 supported 的包翻成 Metal Shading Language。
   超出子集的构造**拒绝并给出行号**，不生成半吊子代码。

完整的 GLSL→Metal 是 SPIRV-Cross 的量级；这里的子集覆盖资源包级着色器真正用到的语法，
遇到不认识的构造就停下来说清楚，这比"翻译出能过编译但渲染错误的代码"要安全得多。

```bash
hz shader-translate shaderpack.zip out.metal
```

```
shaders/terrain: translated (1357 bytes of MSL, 46 lines)
shaders/terrain: note: line 7: texture() maps to slice 0 of the engine's texture array
```

### 支持的子集

| 类别 | 覆盖 |
|---|---|
| 声明 | `in` / `out` / `uniform`，含 `layout(location = n)`；`vec2/3/4`、`mat4`、`float`、`int`、`bool`、`sampler2D` |
| 语句 | 声明+初始化、赋值（含 swizzle 目标）、`if`/`else`、`discard`、表达式语句 |
| 表达式 | 字面量（整数自动变浮点）、标识符、swizzle、构造器、算术/比较/逻辑运算 |
| 内建函数 | `abs acos asin atan ceil clamp cos cross distance dot exp exp2 floor fract length log log2 max min mix mod normalize pow reflect refract round sign sin smoothstep sqrt step tan inversesqrt` |
| 纹理 | `texture(sampler, uv)` / `texture2D(...)` → `hz_tex.sample(hz_smp, uv, 0)` |
| 内建输出 | `gl_Position` → `out.position`；`gl_FragColor` 或声明的 `out vec4` → `hz_colour` |

**拒绝**（带 `stage:line`）：循环、自定义函数、结构体、数组/索引、`?:` 三元、
预处理器指令（含 `#define`）、表外内建、矩阵非 `mat4`、以及包需要引擎未提供的 uniform。

### 生成代码的形状

```metal
struct HzUniforms { float4x4 mvp; float4 fogColor; float time; /* … */ };
struct HzVaryings { float4 position [[position]]; float2 v_uv [[user(locn1)]]; … };
struct HzVertexIn { float3 position [[attribute(0)]]; float2 uv [[attribute(1)]]; … };

vertex   HzVaryings hz_vertex(HzVertexIn in [[stage_in]], constant HzUniforms& u [[buffer(1)]]);
fragment float4     hz_fragment(HzVaryings in [[stage_in]], constant HzUniforms& u [[buffer(1)]],
                                texture2d_array<float> hz_tex [[texture(0)]], sampler hz_smp [[sampler(0)]]);
```

标识符在**生成时**改名（输入 `in.x`、uniform `u.x`、varying `out.x`/`in.x`），
不是事后做字符串替换——后者会把 `uv` 误改成 `v_uv` 里的一部分。

### 谁来验证生成的 MSL

本仓库在 Linux/Windows 上都**没有 Metal 编译器**，所以：

* 本地测试断言结构（属性序号、varying 绑定、`sample` 调用、改名正确）与拒绝行为；
* **macOS CI 用 `xcrun -sdk macosx metal -c` 真正编译生成结果**（`engine/tests/data/shader_pack.zip`）。

这是这条链上唯一能证明"生成的是合法 MSL"的地方，所以 CI 里那一步不是可选的。

### 已经接上渲染管线

```
导入期                                   运行期
shaders/*.vsh|.fsh                       hz_game_shader_msl(n) → String
   ↓ shader::compile（接口校验）              ↓
   ↓ glsl::translate（子集翻译）          device.makeLibrary(source:options:)
   ↓                                      ↓ MTLRenderPipelineState
.hzp: shaders/<name>  (SHD1 描述符)       ↓
      shaders-msl/<name> (Metal 源码)  → encoder.setRenderPipelineState(pack)
                                          encoder.set*Bytes(HzUniforms, index: 1)
                                          encoder.setFragmentTexture(…, index: 0)
                                          encoder.setFragmentSamplerState(…, index: 0)
```

三个必须对齐的点都对齐了：

| 对齐项 | 做法 |
|---|---|
| 顶点属性 | 翻译器按**名字**绑到引擎固定槽位（`position`=0、`uv`=1、`normal`=2、`color`=3、`texture`=4），不是按声明顺序——顺序错了会读到错误偏移，画面是花的但不会报错 |
| uniform 布局 | 翻译器生成的 `HzUniforms` 与 Swift `ShaderUniforms` 字段逐一对齐（mvp / cameraPosition / fogColor / fogParams / time / daylight） |
| 贴图与采样器 | 固定 `[[texture(0)]]` / `[[sampler(0)]]`，与内建管线共用同一片 `texture2d_array` |

失败即回退，绝不留黑屏：

1. `device.makeLibrary` 失败 → 记日志，继续用内建管线；
2. 找不到 `hz_vertex`/`hz_fragment` → 同上；
3. `makeRenderPipelineState` 失败 → 同上。

`hz info` 会直接报出这条链是否完整：

```
shader packs: 1 (1 with translated Metal source)
terrain — vertex+fragment
```

包的校验结果也写在 `manifest.shader_packs` 里，所以「这个包有没有可执行着色器」是可查的。

`hz shader-check <pack.zip>` 与导入流程做的是同一件事：

```
== shaders/world
   version: 330
   vertex: 2 in, 2 out, 2 uniforms
   fragment: 2 in, 1 out, 2 uniforms
   supported: no
   needs: sunDirection

1 pack(s): 0 usable, 1 need more than the engine provides
```

## 校验了什么

| 检查 | 说明 |
|---|---|
| `#version` | 必须存在；< 120 提示过旧；检测到 GLSL ES 方言会警告（引擎面向桌面 GLSL） |
| 阶段接口 | 顶点阶段的 `out` 与片元阶段的 `in` **按名字和类型**配对——这正是 GLSL 链接的方式；类型不一致或缺输出都会点名 |
| 声明位置 | 只在花括号深度 0 收集声明，`main()` 里的局部变量不会被误当成接口 |
| uniform | 只允许引擎提供的集合：`mvp`、`cameraPosition`、`fogColor`、`fogParams`、`time`、`daylight`、`textureAtlas`、`textureArray`；其余进入 `requires` |
| 注释/字符串 | 先剥离，`// in vec3 fake;` 不会污染接口 |
| 内建 | 用了 `gl_VertexID`/`gl_FragCoord` 会提示「翻译时需要重映射」 |

包里的每个 `shaders/` 目录（或 `shaders/<名字>/`）编译成一个 `SHD1` 描述符，
运行时 `Registry.shaders` 读出来给界面显示：名字、阶段、缺口、是否可用。
**渲染器不执行它们**，界面上写明了这一点——这比暗中忽略要诚实。

## 为什么选子集翻译而不是 SPIRV-Cross

| 路线 | 代价 | 取舍 |
|---|---|---|
| 内置 SPIRV-Cross + glslang | 两个大依赖 + C++ 桥接 | 工业级正确性，但彻底破坏「零三方依赖」 |
| **自己写 GLSL→MSL 子集**（已采用） | 约 1200 行 Rust，无依赖 | 覆盖资源包级着色器；不支持的构造明确报错 |
| 只支持手写的 MSL 包 | 定义 `.hshader` 直接写 Metal | 最可靠，但生态为零 |

子集路线的前提是"拒绝要准确"：宁可少翻译，也不能翻译出渲染错误的代码。
`crates/hz-port/src/glsl.rs` 的测试里有 8 个拒绝用例专门守这条底线。

## 数据流

```
jar/zip 里的 shaders/*.vsh|.fsh
   → hz_port::shader 解析接口 + 校验
   → SHD1 描述符 → kind::SHADER 条目
   → Registry::shaders → hz_game_shader_* → Swift 界面
```

`SHD1` 格式见 [FORMATS.md](FORMATS.md)。
