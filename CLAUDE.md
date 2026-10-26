# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

单二进制 DDG 风格 bang 搜索引擎：`GET /?q=vb hello` 查 `user.json` 里的 bang 并 302 跳转，否则 303 跳 Google。完整的行为约定、HTTP 契约、配置项和每条约定的取舍理由都在 [README.md](README.md)，改行为前先读它。

## 常用命令

```sh
cargo fmt --check                     # 格式化检查（CI 卡这一关）
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked                   # 35 个测试，全内联
cargo test --locked trim_is_not_str_trim   # 按名字跑单个测试
cargo test --locked core::tests       # 按模块跑
cargo build --release --locked        # 量二进制度数用这个
PORT=3411 cargo run --locked          # 本地起服务，0.0.0.0:3411
```

除 `cargo fmt` 外都带 `--locked`（`fmt` 不接受这个参数，CI 里也是裸跑），与 Dockerfile 一致。edition 2024、`rust-version = "1.99"`，CI 只跑浮动 `stable`，不设 MSRV 门。

打一个活着的 router 起 smoke test：`PORT=3411 ./target/debug/vbang`，然后用 `curl -sS -o /dev/null -w '%{http_code} -> %{redirect_url}\n'` 看状态码和跳转目标。加 `--no-location`，否则这台机器上的 curl 会自动跟随跳转，输出一堆无关的目标站点响应。

## 架构

三个模块，分层很硬：

- **`core.rs`** 是整个产品去掉 socket 后的全部：`resolve(q, bangs) -> Decision`、`split_bang`、`trim_ws` 是纯函数，`load_bangs` 是唯一的 IO（启动时读一次文件）。不从 HTTP 里借任何东西，只有 `axum::http::StatusCode` 作为返回值传出去。
- **`url.rs`** 放两个百分号编码器，`uri_component` 和 `encode_url`。
- **`main.rs`** 是薄薄的 HTTP 外壳：绑端口、解 `Query`、把 `Decision` 翻成响应。

两个结构上的决定值得记下来：

- **`Decision` 枚举把 `StatusCode` 和 URL 绑在一起**，因为 302/303 的分歧点在 `core::resolve` 里决定，而不是散在 handler 里。新增跳转分支时沿用这个形状。
- **bang 表启动时加载一次**，包成 `Arc<[Bang]>` 只读共享给所有请求。没有热重载，改 `user.json` 必须重启；进程毫秒级重启，所以加载失败直接 `std::process::exit(2)` 而不是降级运行。

`user.json` 用相对路径读，二进制从当前工作目录找它。容器里 `WORKDIR /app` 已放好，本地测试直接跑仓库根目录即可。

## 不能弄坏的东西

这些回归都表现为「输出悄悄变了」，不会编译失败，靠测试兜着。改这几处前先读对应函数的注释，里面写明了为什么必须这样。

- **两个编码器不可互换。** `uri_component` 安全集窄，Google 回退用它，`&`/`=`/`#` 全部变 `%XX`；`encode_url` 安全集宽，bang 路径用它，`#` `$` `&` `?` `=` `:` `@` 原样保留。bang 模板的替换是**刻意不编码**的——这是约定，`&`/`=` 必须能穿透成目标站点的真实参数。不要把搜索词在替换前编码掉。
- **本项目的 trim 集合不等于 `str::trim`。** 要删 U+FEFF、要保留 U+0085：`str::trim` 走 Unicode `White_Space`，它不收 U+FEFF、却收 U+0085，两个方向都和我们要的相反。`trim_ws` 是按码点写死的 25 个码点，测试逐条钉住。
- **词字符只有 ASCII 的 `[A-Za-z0-9_]`，边界出现在下一个字节不存在、或者不是词字符的地方。** `vb你好` 必须命中 bang：`is_word_char` 测试的是单个字节，`你` 的每个字节都不在这个集合里，所以 `b` 后面就是边界。匹配器是一个逐字节的 `is_word_char` 循环，因此没有引入 `regex` 依赖——`regex` crate 默认 Unicode 模式下 `你` 是词字符，同一个输入会静默掉进 Google 回退。
- **bang 别名最长 8 个词字符。** 随仓库发布的 `user.json` 里最长的一条是 `flathub`（7 字符），上限小于 7 它就成了永远匹配不上的死 bang。
- **路由必须用 `any` 注册。** 用 `get` 的话 POST/PUT 会变成 405 带 `Allow` 头；这里有测试断言不带 `Allow`。
- **`q` 必须是 `Option<String>`。** 用 `String` 的话缺失键会被 `Query` extractor 变成 400，而正确的语义是 404 JSON——缺键和空串在这里是同一种「没给查询词」，`search` 里的 `q.filter(|q| !q.is_empty())` 把两者送进同一个 404 分支。
- **生产代码没有 panic 路径**：release profile 是 `panic = "abort"`，靠的就是不存在 `unwrap`/`expect`/`panic!`（`#[cfg(test)]` 除外），唯一的刻意失败走 `std::process::exit(2)`。别在生产路径上引入 unwrap。
- **`Location` 头绝不能含裸控制字符**（HTTP 禁止 CR/LF，这也是防头注入的地方）。两条跳转路径各有一个编码器保证这一点——bang 路径的 `encode_url`、回退路径的 `uri_component`，测试在两个分支上都断言这个不变量而不是相信它。
- **日志只打搜索词，不打跳转目标**，某个 bang 可能指向付费或保密地址。别在 handler 里加打印 URL 的日志。

## 测试

- 没有 `tests/` 目录、没有集成测试、没有 `lib.rs`——这是一个纯 binary crate，全部 34 个测试内联在各模块的 `#[cfg(test)] mod tests` 里。
- `core::tests` 用 `bang_case` / `search_case` 两个辅助函数断言 `resolve` 的完整输出（URL 加状态码）。加 bang 解析行为时往里加 case 即可。
- `main::tests` 用 `tower::ServiceExt::oneshot` 直接打 router，不起端口、不占 socket。构造请求见 `call()`。
- `url::tests` 的向量表把两个编码器的输出逐字节钉住。改编码器前先读明白新安全集的边界再写断言，不要凭直觉猜——`>` 和 `\` 分别落在 `encode_url` 安全集的两个缝隙里（`>` 夹在 0x3d 与 0x3f..=0x5b 之间，`\` 夹在 0x5b 与 0x5d 之间），读成连续区间就会错。
- 约定的每一条例外都有一条测试钉住它（未编码替换、只替换第一个占位符、`$` 字面量、无占位符模板丢弃查询词）。这些测试的存在意味着改动会立刻失败，属于预期。

## CI 与发布

`.github/workflows/build.yml`：`check`（fmt / clippy / test，即上面的三关）通过后 `build` 用 buildx 编 `linux/amd64` 并推送镜像。推送目标、构建缓存位置和所需 secrets 见 README 的「构建与发布」，那节是唯一的事实来源。

只改 `*.md`、`.gitignore`、`.dockerignore` 时整个工作流被 `paths-ignore` 跳过——文档改动不需要等 CI。
