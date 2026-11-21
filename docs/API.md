# C ABI 参考（`hz_ffi.h`）

约定：

- 返回 `int32_t` 的函数：`0` 成功，非 `0` 失败，错误文本在 `hz_last_error()`。
- 返回 `char *` 的函数：调用方拥有，必须用 `hz_string_free()` 释放；失败返回 `NULL`。
- 返回句柄的函数：失败返回 `NULL`，用对应 `*_free` 释放，且只释放一次。
- 指针参数为 `NULL` 时不会崩溃，会走错误路径并返回 0/失败码。

## 版本与错误

```c
uint32_t    hz_api_version(void);   // 当前为 1
const char *hz_last_error(void);    // 线程局部，无需释放，下次错误覆盖
void        hz_string_free(char *);
```

Swift 侧在加载资源包前比较 `hz_api_version()` 与 `HZ_API_VERSION`，不同即拒绝。

## 编译

```c
typedef void (*HzProgress)(void *ctx, const char *stage,
                           uint32_t done, uint32_t total);

int32_t hz_port_jar(const char *jar,
                    const char *out_hzp,
                    const char *out_report,
                    const char *minecraft_version,
                    HzProgress cb, void *ctx);
```

`stage` 依次为 `read, blockstates, models, textures, lang, data, mixins, classes, write`。
回调在调用线程上同步触发，可以频繁触发；Swift 侧把每次调用 `Task { @MainActor ... }` 转发。

```c
char *hz_pack_summary(const char *pack_path);
// {"entries":10,"kinds":{"blockstates":2,...},"manifest_sha256":"...","bytes":264317}
```

## 游戏

```c
typedef struct HzGame HzGame;

HzGame  *hz_game_new(const char *pack_path);   // NULL = 失败
void     hz_game_free(HzGame *);
void     hz_game_tick(HzGame *, float dt);     // dt 会被夹到 [0, 0.1]
uint32_t hz_game_rebuild_frame(HzGame *);   // 1 = 顶点缓冲变了，0 = 什么都没动
uint32_t hz_game_frame_quads(const HzGame *);
uint32_t hz_game_frame_data(const HzGame *, float *out, uint32_t cap_floats);
```

`hz_game_frame_data` 返回写入的 `float` 数量；容量不足时返回 `0` 且不写任何字节。

`hz_game_rebuild_frame` 现在是**增量**的：只有被编辑过或动画帧推进过的分区会重烘焙。
返回值告诉调用方要不要重新拷贝顶点——空闲帧返回 0，Swift 侧直接跳过拷贝。

```c
void     hz_game_set_player(HzGame *, float x, float y, float z, float yaw, float pitch);
uint32_t hz_game_player_pos(const HzGame *, float *out3);   // 返回 3 或 0
void     hz_game_set_block(HzGame *, int32_t x, int32_t y, int32_t z, uint32_t id);
uint32_t hz_game_stats(const HzGame *, uint32_t *out4);     // ticks, frames, quads, non_air
int32_t  hz_game_save(const HzGame *, const char *path);
int32_t  hz_game_load(HzGame *, const char *path);
```

`hz_game_set_player` 传送（清空速度）；逐 tick 调用它来驱动行走，引擎负责重力与碰撞。

### 拾取与交互

```c
uint32_t hz_game_raycast(const HzGame *, float ox, float oy, float oz,
                         float dx, float dy, float dz,
                         float max_distance, float *out7);
uint32_t hz_game_air_id(const HzGame *);
```

`out7` 依次是 `block.x/y/z`、`normal.x/y/z`、`distance`；命中返回 7，未命中返回 0。
法线就是「放置新方块」时该走的偏移：`adjacent = block + normal`。
起点所在的那个体素会被跳过（头埋在墙里时不会自己打自己）。

### 方块表

```c
uint32_t hz_game_block_id(const HzGame *, const char *name);   // 缺失返回 UINT32_MAX
uint32_t hz_game_block_count(const HzGame *);
uint32_t hz_game_block_name(const HzGame *, uint32_t index, char *out, uint32_t cap);
```

### 光照

```c
void     hz_game_set_daylight(HzGame *, float daylight);  // 0 夜 … 1 正午
float    hz_game_daylight(const HzGame *);
uint32_t hz_game_sky_at(const HzGame *, int32_t x, int32_t y, int32_t z);        // 0..15
uint32_t hz_game_block_light_at(const HzGame *, int32_t x, int32_t y, int32_t z); // 0..15
uint32_t hz_game_brightness_at(const HzGame *, int32_t x, int32_t y, int32_t z); // 0..15
```

`hz_game_set_daylight` 只改天空光通道，方块光不受影响；下一次 `hz_game_rebuild_frame`
会把受影响的分区重新烘焙。天空光与方块光都烘焙进顶点色，所以昼夜切换不需要改着色器。

### 贴图表

```c
uint32_t hz_game_texture_count(const HzGame *);
uint32_t hz_game_texture_name(const HzGame *, uint32_t index, char *out, uint32_t cap);
uint32_t hz_game_texture_png_len(const HzGame *, uint32_t index);
uint32_t hz_game_texture_png(const HzGame *, uint32_t index, uint8_t *out, uint32_t cap);
```

下标与顶点流里的 `texture` 字段同一套编号。PNG 原字节直接交给 `MTKTextureLoader`，
Swift 侧不需要知道 `.hzp` 的结构。某个 id 在包里没有对应 PNG 时长度为 0，
渲染器用洋红色占位，一眼能看出缺资源。

## 生命周期规则

1. `hz_game_new` 之后才会有有效句柄。
2. 一个句柄只能在**一个线程**上使用；Swift 侧固定在主线程（tick/重建）与引擎队列（导入），两者不交叉。
3. `hz_game_save` 可在 tick 之间任意时刻调用；`hz_game_load` 会替换整个世界与玩家。
4. `hz_game_free` 之后句柄立即失效，重复释放是未定义行为——Swift 的 `deinit` 保证只释放一次。
5. 贴图与方块表的指针只在句柄存活期间有效；Swift 在会话启动时一次性拷走。

## 与头文件同步

`engine/include/hz_ffi.h` 是唯一对外承诺。改签名必须同时改：

1. `engine/crates/hz-ffi/src/lib.rs`
2. `engine/include/hz_ffi.h`
3. `HzEngine.swift` 的调用点
4. 本文件

`EngineABITests.testAPIVersionMatchesHeader` 会在版本号不一致时失败。