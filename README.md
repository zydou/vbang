# vbang

单二进制的 DDG 风格 bang 搜索引擎。

`GET /?q=vb hello` → 在 `user.json` 里查 bang `b` → 302 跳转到 bing；没有 bang 就 303 跳到 Google。整个服务只有一条路由、一个端口、一份配置文件，职责只有一个：吐出一个 `Location` 头。二进制从不发起任何出站请求——真正去抓网页的是浏览器。

下面逐节说明：[行为约定](#行为约定) 记下每一条约定以及为什么成立，[实现取舍](#实现取舍) 记下哪些看起来像 bug 的行为是刻意接受的。

## 运行

用 Docker（镜像已构建好时）：

```sh
docker run --rm -p 3000:3000 ghcr.io/zydou/vbang:latest
```

自己构建：

```sh
docker build -t vbang .
docker run --rm -p 3000:3000 vbang
```

本地开发：

```sh
cargo run --locked        # 默认监听 0.0.0.0:3000
PORT=3411 cargo run --locked
```

## 行为约定

`/` 是唯一的路由，服务里没有静态目录、没有首页、没有样式表。

| 请求 | 响应 |
| --- | --- |
| `GET /?q=vb hello` | `302 Found`，`Location` 指向 bang 目标 |
| `GET /?q=hello` | `303 See Other`，`Location` 指向 Google |
| `GET /?q=` | `404`，`{"error":"No query parameter provided"}` |
| `GET /` | 同上，缺 `q` 与空 `q` 一视同仁 |
| `GET /?q=a&q=b` | `400`，重复键（axum 在进 handler 前拒绝） |
| `GET /index.html`、`/user.json`、`/nope` | `404`，无响应体 |
| `HEAD /` | `200 OK`，空响应体；不带 `q` 也是 200 |
| `POST` / `PUT` / `DELETE` / `PATCH` / `OPTIONS` | `404`，不带 `Allow` 头 |

几个容易被问到的点：

- **为什么 bang 是 302 而回退是 303。** 这是个约定：命中 bang 一律 `302 Found`，任何回退一律 `303 See Other`。状态码不写在 handler 里——它和跳转目标一起绑在 `Decision` 上，在 `core::resolve` 里定下来，handler 照搬即可。两条路径各有测试钉住各自的状态码。
- **为什么 `HEAD` 无条件 200。** 它是存活探针，不是搜索，所以 `q` 会 404 的情况在这里无所谓。
- **为什么其他方法是 404 而不是 405。** 路由用 `any` 注册，只有 GET 有实义，没有可枚举的方法集合，也就不该带 `Allow` 头。
- **未识别的查询参数会被忽略**（`?q=hello&foo=bar` 正常跳转），只有重复键会报错。

实测样例：

```sh
curl -sS -o /dev/null -w '%{http_code} -> %{redirect_url}\n' 'http://localhost:3000/?q=vb+hello+world'
# 302 -> https://cn.bing.com/search?ensearch=1&q=hello%20world

curl -sS -o /dev/null -w '%{http_code} -> %{redirect_url}\n' 'http://localhost:3000/?q=hello+world'
# 303 -> https://www.google.com/search?newwindow=1&q=hello%20world

curl -sS 'http://localhost:3000/'
# {"error":"No query parameter provided"}
```

## 配置

**`PORT`**：监听端口，默认 `3000`。总是绑定 `0.0.0.0`。取值不合法（空、非数字）时静默回落到 3000，不报错。

**`user.json`**：bang 表，就是产品本身。启动时一次性读入并校验，之后只读共享，没有热重载。格式是对象数组：

```json
[
  { "t": "b", "u": "https://cn.bing.com/search?ensearch=1&q={{{s}}}" }
]
```

- `t` 是 bang 别名。匹配规则：首字符 `v` 或 `V`，随后 1 到 8 个词字符（`[A-Za-z0-9_]`），再接一个词边界。首字母大小写不敏感，别名**大小写敏感**。
- `u` 是目标 URL 模板，`{{{s}}}` 是搜索词的占位符。
- 查找是 first-match、大小写敏感（命中第一条同名条目即停）。加载器已拒绝重复的 `t`，所以表里不可能真的出现同名条目。
- 只替换**第一个** `{{{s}}}`；模板里没有占位符时搜索词被静默丢弃。
- 替换是**字面拼接，不做 URI 编码**，`$` 在搜索词里一律按字面处理（见[实现取舍](#实现取舍)）。

加载失败直接退出，退出码 `2`，不会带着一张坏表继续跑。会被拒绝的情况：文件读不到、不是合法 JSON、`t` 或 `u` 为空串、`t` 重复。

一个值得留意的约束：路径是相对路径 `user.json`，二进制从**当前工作目录**读它。容器里 `WORKDIR /app` 已经放好，本地跑 `cargo run` 时在仓库根目录即可；换个目录直接跑二进制而没有 `user.json` 就会以退出码 2 结束。

## 实现取舍

### 已知但接受的行为

- **bang 路径不做 URI 编码。** `q=vy a&b=c` 落到 `youtube.com/results?search_query=a&b=c`——`&` 和 `=` 原样穿透，在目标站点上变成了真实参数。收尾的 `encode_url` 收紧空格、非 ASCII、控制字符，以及 `"` `<` `>` `\` `^` `` ` `` `{` `|` `}` 这九个可打印标点（完整的 `%XX` 序列原样通过），`&`、`=`、`#`、`?`、`$` 原样通过。同一条查询走 Google 回退时会被完整编码（`&q=a%26b%3Dc`），两条路径的编码强度不同是刻意的设计，不是遗漏。
- **搜索词里的 `$` 就是 `$`。** 替换不做 `$` 序列展开：`vy $$` 落到 `search_query=$$`。夹具 bang `x`（模板 `https://example.org/p/{{{s}}}/mid/{{{s}}}/end`，只在 `core::tests` 里，不在发布的 `user.json` 里）说明模板里第二个 `{{{s}}}` 不会被替换：`vx $&` 落到 `p/$&/mid/%7B%7B%7Bs%7D%7D%7D/end`，`encode_url` 把残留的占位符编码掉。

### 其他取舍

- **bang 别名最长 8 个词字符。** 随仓库发布的 `user.json` 里最长的一条是 `flathub`（7 个字符），上限取 8 是为了让它能匹配上——表里有的 bang 就必须能用；这个取值只影响一个可观察的用法（`vflathub <q>`），任何其他的 7 或 8 字符前缀都不在表里，都会落到 Google 回退。连续 9 个以上词字符时整条不匹配而不是截断：第 9 个字符还是词字符，边界不存在。
- **去空白用的是固定的 25 个码点：** U+0009..U+000D、U+0020、U+00A0、U+1680、U+2000..U+200A、U+2028、U+2029、U+202F、U+205F、U+3000、U+FEFF。不用 `str::trim`：它按 Unicode `White_Space` 走，既不收 U+FEFF、又收 U+0085，两头都不对。两个方向都有测试钉住。
- **词字符是 `[A-Za-z0-9_]`，只认 ASCII；边界存在当且仅当下一个字节不存在或不是词字符。** 所以 `vb你好` 里 `b` 后面就是边界，bang 命中，搜索词拿到 `你好`。逐字节判断靠的是这个集合 ASCII-only：非 ASCII 字符的每个字节都不在其中。集合若认 Unicode 字母，`你` 反而会算词字符，同一个输入会静默掉进 Google 回退。匹配器因此是一个短循环，不需要引入正则依赖。CJK 查询经常不带空格，这是个真实用例。
- **重复查询键返回 400，而且在任意方法上都是 400。** `Query` extractor 在进 handler 之前跑，所以就算这个方法本应回 404，重复键也先变成 400。请求形状错了是客户端的问题，不是服务端故障。
- **服务不提供任何静态资源。** 没有首页、没有样式表，仓库自身的文件也不对外；`/` 就是搜索路由本身，所以它不在这份名单里。GET 打到未知路径是裸 404、无响应体；其他方法会先落进 handler 再判 404，于是带着一个 axum 自带的 `not found` 文本体——`Allow` 头在任何情况下都不会出现。
- **`load_bangs` 对空字段和重复键直接拒绝**，而不是容忍一个永远不会生效的条目。

### 日志

三条日志，全部走 stderr，时间戳由容器运行时加：

- 启动：`listening on 0.0.0.0:3000, 51 bangs`
- 配置错误：`error: ...`，随后退出码 2
- 每次搜索：`query: <q>`

日志只打搜索词，**不打印跳转目标**——某个 bang 可能指向付费或保密地址。存活探针（`HEAD`）和那些返回 404 的方法都不会产生日志行。

镜像里没有 tzdata，所以时间戳始终是 UTC RFC3339，`TZ` 环境变量会被忽略。没有 CA 证书包也无所谓：二进制从不发出站 TLS 请求。

## 构建与发布

镜像是两阶段构建：`rust:1-slim-bookworm` 编出 musl 静态二进制，最终层是 `FROM scratch`——只有二进制、`user.json` 和 `LICENSE`，没有 distro 的时区库、CA 包、passwd 之类。以 `65534:65534` 运行，避免 `docker run` 时落到 root。编译阶段只拷编译真正读的文件：`COPY src src` 加 `COPY Cargo.toml Cargo.lock ./`。这不是「`COPY . .` 会因时间戳抖动失效」——BuildKit 的 tarsum 序列化时明确丢弃 mtime，整仓 mtime 重写都不会让 COPY 层失效，唯一敏感的元数据是 `mode`；真正的问题是 `user.json` 是运行期数据，却被 `COPY . .` 带进了编译阶段，`cargo build` 依赖那一层，改一条 bang 就要重编一次（实测 47 秒）。拆开后改 `user.json` 只重跑最终层（0.8 秒）。这里也**不**走「先拷清单、`cargo fetch --locked`、再拷源码」那种经典拆法：`cargo fetch` 要求 manifest 声明的 target 文件真实存在，纯二进制 crate 只有 Cargo.toml 时会报 `no targets specified in the manifest`，只能先写一个占位 `src/main.rs` 绕过去；窄拷贝直接 `cargo build`、不跑 `fetch`，`src/main.rs` 在位、target 推断正常，那条错误踩不到。代价是编译阶段再也看不到 `user.json` 和 `LICENSE`——将来若有人用 `include_str!("../user.json")` 把数据嵌进二进制，构建会立刻报编译错、不会静默错，这是可接受的失败方式。两种写法产物逐字节相同，冷编译耗时一致，不省不亏。

Release profile 五个开关的取舍理由写在 `Cargo.toml` 的注释里：`opt-level = "z"`（配 LTO 后比 `s` 略小）、`codegen-units = 1`、`lto = "fat"`、`strip = "symbols"`，以及 `panic = "abort"`（代价是 panic 没有回溯——生产代码里没有任何 panic 路径，唯一的刻意失败走 `std::process::exit(2)`）。注释里刻意不记字节数：那是某一代工具链的一次快照，会整体过期，而结论不依赖它；想确认某个开关贵不贵，自己 `cargo build --release` 量一遍。这些配置放在 profile 里而不是 Dockerfile 里，是为了让本地的 release 构建和镜像共用同一份 profile——目标三元组不同：镜像是 musl 静态二进制，本地 `cargo build --release` 是 glibc 动态链接，两者不是同一份字节。

CI（`.github/workflows/build.yml`）两个 job：

1. **check**：`cargo fmt --check` → `cargo clippy --locked --all-targets -- -D warnings` → `cargo test --locked`，工具链是浮动的 `stable`。不钉 MSRV：没有消费者会在旧工具链上构建这个项目，镜像也用最新工具链，一条只跑 `stable` 的门就够了。
2. **build**：buildx 构建 `linux/amd64`，镜像 tag 和 OCI labels 交给 `docker/metadata-action` 生成，不在 workflow 里手写——`type=sha,prefix=,format=long` 给每次提交打完整 commit sha，`type=semver` 打 `v0.1.0` / `v0.1` / `v0` 三档，`type=ref,event=branch` 给分支名打 tag，`type=raw,value=latest,enable={{is_default_branch}}` 让 `latest` 只随默认分支前进。这个 `enable` 是刻意的：tag push 不推动 `latest`，所以 `v0.1.0` 永远指向它自己的那个 digest，不会等 tag push 结束才能部署，也不会被 main 后来的构建挪走。构建缓存走 `ghcr.io/zydou/vbang:build-cache`。

只推 GHCR，登录用自带的 `GITHUB_TOKEN`；`packages: write` 只挂在 `build` job 上，`check` 只有 `contents: read`。不需要额外 secret。推送只改 `*.md`、`.gitignore`、`.dockerignore` 时会跳过整个工作流。

## License

MIT，见 [LICENSE](LICENSE)。
