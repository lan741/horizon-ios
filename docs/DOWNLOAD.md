# 应用内下载游戏版本

从镜像取一个版本，在设备上编译成 `.hzp`。整个过程不经过任何自有服务器。

## 支持的两个源

| 源 | 清单地址 | 说明 |
|---|---|---|
| BMCLAPI | `bmclapi2.bangbang93.com/mc/game/version_manifest_v2.json` | 国内直连，通常明显更快 |
| 官方 | `piston-meta.mojang.com/mc/game/version_manifest_v2.json` | 海外网络更稳 |

镜像通过 **URL 重写**实现，不是另一套 schema：

```
https://piston-meta.mojang.com/...   → https://bmclapi2.bangbang93.com/...
https://piston-data.mojang.com/...   → https://bmclapi2.bangbang93.com/...
https://resources.download.minecraft.net/<hh>/<hash>
                                     → https://bmclapi2.bangbang93.com/assets/<hh>/<hash>
```

最后一条尤其容易写错：对象在镜像上位于 `/assets` 之下，不是镜像根。测试里有一条断言
专门守这个行为。

## 取哪些文件

| 类别 | 取 | 不取 |
|---|---|---|
| 方块 | `blockstates/`、`models/`、`textures/block/` | — |
| 物品 | `textures/item/` | — |
| 语言 | `lang/en_us.json`、`lang/zh_cn.json` | 其它语言 |
| 声音 | `sounds.json` | `sounds/**/*.ogg`（可选，体积最大） |
| 着色器 | — | `shaders/`（可选） |
| 其它 | — | 实体贴图、画、皮肤、字体之外的一切 |

下载完每个对象都按 **SHA-1** 校验（哈希即路径），坏一个重试一个，不会产出坏包。

## 体积参考（26.3）

| 项 | 大小 |
|---|---|
| `client.jar` | 41.5 MiB |
| 资源索引 JSON | 597 KiB |
| 默认选择（方块/物品贴图 + 模型 + 语言 + 目录） | 数十 MiB，视版本而定 |
| 打开音效 | 再增加上百 MiB |

界面在开始前显示文件数与总量，不让人盲点。

## 断点续传

第一版每次失败都从头再来（工作目录是 UUID 命名的临时目录，出错就整个删掉）。
用户的原话是「总是下载中断就要重新下」——那是设计缺陷，不是运气不好。

现在的规则：

| 项 | 行为 |
|---|---|
| 工作目录 | **每个版本一个固定目录**（`Caches/horizonpad/downloads/<version>/`），失败不删 |
| 资源对象 | 按哈希存盘，已存在就跳过；写入走 `.part` 再原子改名，崩溃不会留下“看起来完整”的半截文件 |
| `client.jar` | 用 `Range: bytes=<已有字节>-` 续传，边收边追加，中断后从断点继续 |
| 对象哈希不符 | 视为传输损坏，重试最多 3 次（每次退避 0.4s × n） |
| 单个对象失败 | 记录后继续处理其它对象，不放弃整批 |
| 服务端忽略 Range（返回 200） | 判定为“必须重来”，截断重写，而不是把整份数据追加到半截文件后面 |
| 已完成 | 把 `.part` **移入正式路径**；"数据在盘上"和"文件在目标位置"是两件不同的事 |

界面上会显示「已缓存 N 个文件 · X MB」和「清理下载缓存」，所以续传是可见的，
不需要用户猜。

### 验证方式

`scripts/test-resume.py` 起一个本地 HTTP 服务器（支持 Range），让编译出的 Swift 代码：

1. 取一份 512 KiB 的负载；
2. 把 `.part` 截断到 200000 字节，模拟连接中断；
3. 再次请求，断言发出的是 `Range: bytes=200000-`、收到 206、最终文件 512 KiB 且
   **逐字节相同**。

```
after interruption: part file has 200000 bytes, Range sent = ["-"]
after resume: complete file has 524288 bytes, Range sent = ["bytes=200000-"]
  ok   the range starts exactly where the file stopped (bytes=200000-)
  ok   content matches byte for byte
ALL PASSED
```

`scripts/swift_catalog_tests.py` 里还有 6 条 `ResumePlanner` 断言（含“服务端忽略 Range”
和“缓存比预期还大”两个容易写错的分支）与 8 条对象存储断言。

## 打包与编译

下载结果用 `StoreZipWriter` 打成 `assets/minecraft/...` 的 zip，然后调用
`hz_port_jar`——**和手动导入走完全相同的代码路径**，所以两条路产出的 `.hzp` 结构一致。

## 验证

`scripts/verify-download-chain.ps1` 做三件事，都不需要网络：

1. 用本机 Swift 工具链编译并运行 `MirrorCatalog` 的 34 条断言（清单解析、镜像重写、
   资源筛选、zip 写入、CRC32 校验值）；
2. 把 Swift 写出的 zip 交给 `hz unzip`，确认 3 个条目 0 失败；
3. 交给 `hz port`，确认能产出 `.hzp`。

第 2、3 步是关键：它们证明 **Swift 端写出的字节确实是引擎能吃的容器**，而不只是
"单元测试通过"。
