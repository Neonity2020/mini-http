# 从零用 Rust 写一个 HTTP/1.1 服务器

这份教程和仓库里的 `mini-http` 源码一一对应。读完之后，你应当能回答：

- TCP 字节流如何变成一个个 HTTP 请求？
- 为什么解析 HTTP 时“限制长度”和“判断消息边界”比“读一行”更重要？
- keep-alive、`Content-Length`、HEAD、`100-continue` 分别如何工作？
- 怎样写出一个不依赖框架、但测试齐全的小型服务器？
- 这个教学实现距离生产环境还差什么？

> 运行环境：Rust 1.75+，不依赖任何第三方 crate。

---

## 目录

1. [目标与非目标](#1-目标与非目标)
2. [HTTP 在 TCP 上长什么样](#2-http-在-tcp-上长什么样)
3. [项目结构与请求生命周期](#3-项目结构与请求生命周期)
4. [监听、并发与连接上限](#4-监听并发与连接上限)
5. [解析请求：从字节到 `Request`](#5-解析请求从字节到-request)
6. [构造响应：从 `Response` 到字节](#6-构造响应从-response-到字节)
7. [路由与参数](#7-路由与参数)
8. [业务处理器](#8-业务处理器)
9. [一个连接的完整生命周期](#9-一个连接的完整生命周期)
10. [测试与调试](#10-测试与调试)
11. [并发、限制与安全边界](#11-并发限制与安全边界)
12. [练习](#12-练习)
13. [从教学服务器到生产环境](#13-从教学服务器到生产环境)

---

## 1. 目标与非目标

### 1.1 我们实现什么

| 能力 | 实现位置 |
| --- | --- |
| TCP 监听、每个连接一个线程 | `src/server.rs` |
| HTTP/1.1 / HTTP/1.0 请求解析 | `src/http/request.rs` |
| 请求行、请求头、`Content-Length` body | `src/http/request.rs` |
| keep-alive 与连接关闭规则 | `src/http/request.rs`、`src/server.rs` |
| HEAD 自动复用 GET | `src/router.rs`、`src/http/response.rs` |
| `Expect: 100-continue` | `src/http/request.rs`、`src/server.rs` |
| 16 KiB 请求行+头限制、1 MiB body 限制 | `src/server.rs`、`src/http/request.rs` |
| 路径参数、查询参数、百分号解码 | `src/router.rs`、`src/http/uri.rs` |
| 404 / 405 / `Allow` 头 | `src/router.rs` |
| 响应头注入防护 | `src/http/response.rs` |
| 单元测试 + 真实 TCP 集成测试 | 各模块与 `tests/integration.rs` |

### 1.2 我们故意不实现什么

这不是一个可以直接放到公网的生产服务器：

| 未实现 | 原因 / 生产替代 |
| --- | --- |
| TLS | 用反向代理（Caddy/Nginx）或 `rustls` |
| HTTP/2、HTTP/3 | 用 `hyper` / `axum` |
| chunked 请求体 | 教学实现只接受 `Content-Length` |
| async I/O、线程池 | 每连接一线程，简单直观但扩展性有限 |
| 静态文件、压缩、认证、会话 | 属于应用层或中间件 |
| 优雅退出、配置热加载、tracing | 生产基础设施 |
| handler panic 隔离 | 生产框架通常提供 catch/500 |
| 流式上传 | 当前 body 整体读入内存 |

明确“不做什么”，代码才能保持可读。教程里会在相应位置解释这些取舍。

---

## 2. HTTP 在 TCP 上长什么样

TCP 是**字节流**。一次 `read` 可能只拿到半行，也可能同时拿到两个请求。HTTP 服务器必须自己决定：一个请求从哪里开始、到哪里结束。

一个最小请求：

```http
GET /api/health HTTP/1.1\r\n
Host: localhost\r\n
User-Agent: curl/8.7.1\r\n
\r\n
```

对应响应：

```http
HTTP/1.1 200 OK\r\n
Server: mini-http/0.1\r\n
Content-Length: 59\r\n
Content-Type: application/json; charset=utf-8\r\n
Connection: keep-alive\r\n
\r\n
{"status":"ok","service":"mini-http","protocol":"HTTP/1.1"}
```

需要记住的几件事：

1. 每行以 `CRLF`（`\r\n`）结束，最后一个空行表示请求头结束。
2. 响应 body 的长度由 `Content-Length` 确定（本例没有 chunked）。
3. `Connection` 决定这个 TCP 连接能不能继续复用。
4. 请求和响应都可能没有 body，例如 GET、HEAD、204。
5. 客户端可能把多个请求连续写入同一个连接，这叫 pipelining。我们的服务器串行读取、串行响应。

### `100 Continue`

当客户端要发送较大的 body 时，可能先只发请求头：

```http
POST /api/echo HTTP/1.1
Host: localhost
Content-Length: 1048576
Expect: 100-continue

```

它在等服务器说“可以继续发”。服务器验证头部和大小限制后，先写一个临时响应：

```http
HTTP/1.1 100 Continue

```

然后客户端才发送 body。我们的 `read_request` 通过一个回调在读取 body 之前触发这一步，详见 5.4。

---

## 3. 项目结构与请求生命周期

```text
rust-http-server/
├── src/
│   ├── main.rs              # 命令行参数、启动
│   ├── lib.rs               # 模块导出
│   ├── app.rs               # 业务 handler
│   ├── router.rs            # 方法与路径匹配
│   ├── server.rs            # accept、线程、keep-alive 循环
│   └── http/
│       ├── method.rs        # Method 枚举
│       ├── status.rs        # 状态码 / reason phrase
│       ├── uri.rs           # 查询参数与百分号解码
│       ├── request.rs       # 请求解析、限制、错误映射
│       └── response.rs      # 响应序列化
└── tests/
    └── integration.rs       # 真实 TCP 集成测试
```

真实的调用链：

```text
main
 └─ Server::bind
     └─ Server::run
         └─ TcpListener::accept
             └─ thread::spawn
                 └─ serve_connection
                     ├─ read_request
                     ├─ Router::dispatch
                     └─ Response::write_http
```

模块之间尽量保持单向依赖：

- `server` 知道 `http` 和 `router`
- `router` 知道 `http`
- `http` 不知道业务，也不知道路由
- `app` 只负责业务 handler

这种分层让“协议正确性”和“业务行为”可以分别测试。

---

## 4. 监听、并发与连接上限

入口非常简单：解析地址、绑定、进入 `run`。

```rust
let server = Server::bind(address.as_str(), app::router(), ServerConfig::default())
    .map_err(|error| format!("cannot bind {address}: {error}"))?;

println!("mini-http listening on http://{}", server.local_addr()?);
server.run()?;
```

`ServerConfig::default()` 中的关键限制：

```rust
max_header_bytes: 16 * 1024,
max_body_bytes: 1024 * 1024,
max_requests_per_connection: 100,
max_connections: 256,
read_timeout: Duration::from_secs(10),
write_timeout: Duration::from_secs(10),
```

### 4.1 每连接一个线程

```rust
let (stream, peer) = listener.accept()?;
thread::Builder::new()
    .name(format!("mini-http-{peer}"))
    .spawn(move || {
        serve_connection(stream, router, config);
    })?;
```

这就是 thread-per-connection 模型：

- 优点：阻塞 I/O 写起来和同步函数一样，解析、路由、handler 都不需要 `async`。
- 缺点：线程有栈和调度成本；连接数一高就会耗尽内存或线程资源。
- 适用：学习、局域网工具、低并发内部服务。
- 不适用：公网高并发。生产环境通常使用 Tokio + 任务，或事件驱动模型。

### 4.2 连接上限

只限制请求大小还不够，还要限制同时在线的连接数：

```rust
let previous = active.fetch_add(1, Ordering::Relaxed);
if previous >= config.max_connections {
    active.fetch_sub(1, Ordering::Relaxed);
    reject_overloaded(stream); // 503
    continue;
}

let guard = ConnectionGuard(Arc::clone(&active));
let spawned = thread::Builder::new().spawn(move || {
    let _guard = guard;
    serve_connection(stream, router, config);
});
```

几个细节：

- 计数在 `accept` 之后立即增加；超过 256 时直接写 503 并关闭。
- `ConnectionGuard` 的 `Drop` 负责减计数，线程正常结束、panic、写失败都会回到 0。
- 如果 `spawn` 失败，闭包被丢弃，guard 也随之释放。
- 503 本身也有 1 秒写超时，避免恶意连接把 accept 线程卡住。

### 4.3 超时和 `TCP_NODELAY`

```rust
let _ = stream.set_nodelay(true);
let _ = stream.set_read_timeout(Some(config.read_timeout));
let _ = stream.set_write_timeout(Some(config.write_timeout));
```

- `TCP_NODELAY` 关闭 Nagle 算法，减少小响应的延迟，适合请求/响应式流量。
- 10 秒读超时覆盖**单次**阻塞读，包括 keep-alive 空闲。没有它，只需建立连接后不发送数据，就能长期占住一个线程。
- 注意：超时不是“整个请求必须在 10 秒内完成”。如果客户端每隔 9 秒发一个字节，每次 `read` 都可能重新计时；要限制总处理时间，需要额外的请求级 deadline。
- 超时也不是完整的 DoS 防护：256 个慢连接仍能占满上限。生产环境还需要反向代理、连接队列、限流和异步 I/O。

### 4.4 为什么用 `BufReader` 和 `try_clone`

```rust
let read_half = stream.try_clone()?;
let mut reader = BufReader::new(read_half);
let mut writer = stream;
```

读写需要同时存在于一个循环里。`TcpStream` 的 `try_clone` 复制的是底层 socket 的句柄，不是数据；两个句柄共享同一个连接。`BufReader` 减少系统调用，也能在解析完请求头后继续缓冲下一批字节。

> 注意：不要在 `BufReader` 和原始 `TcpStream` 上混读。这里所有读取都通过 `reader`。

---

## 5. 解析请求：从字节到 `Request`

`src/http/request.rs` 是项目里最重要的文件。解析的核心挑战不是“匹配字符串”，而是：

1. **不能无限分配内存**
2. **必须精确知道消息何时结束**
3. **遇到歧义要拒绝，而不是猜测**

### 5.1 有上限地读一行

标准库的 `BufRead::read_line` 会一直读到换行，恶意输入可以让它分配巨大字符串。我们使用 `take(limit + 2)` 包一层：

```rust
let mut line = Vec::new();
let read = reader
    .by_ref()
    .take(limit.saturating_add(2) as u64)
    .read_until(b'\n', &mut line)?;

if read == 0 {
    return Ok(None); // 干净 EOF
}
if !line.ends_with(b"\n") {
    return Err(/* 太长或行未结束 */);
}
line.pop(); // \n
if line.ends_with(b"\r") {
    line.pop(); // \r
}
```

为什么是 `limit + 2`？

- 需要容纳内容 + `\r\n`；
- 再多读 1 个字节就能判断“是否超出限制”；
- 超限后直接关闭连接，不再尝试寻找下一行。

错误区分：

- 首次读取就 EOF：`Ok(None)`，表示客户端正常关闭。
- 读到一半 EOF：`400 Bad Request`。
- 行过长：请求行返回 `414`，头行返回 `431`。
- 行中出现裸 `\r`：拒绝，避免不同代理对换行的理解不一致。
- 作为调试容错，解析器也接受只用 `\n` 结束的行；正常 HTTP 客户端应发送 `\r\n`。

### 5.2 请求行

```http
GET /hello?name=Ada HTTP/1.1
```

只允许三个字段，并且用单个空格分隔：

```rust
let mut parts = request_line.split(' ');
let method_token = parts.next().filter(|part| !part.is_empty())?;
let target = parts.next().filter(|part| !part.is_empty())?;
let version_token = parts.next().filter(|part| !part.is_empty())?;
if parts.next().is_some() {
    return Err(RequestError::BadRequest(
        "request line must contain exactly three fields",
    ));
}
```

方法处理有一个重要区别：

- 语法合法但服务器未实现的方法，例如 `FETCH` → `501 Not Implemented`
- 语法非法的方法，例如 `GE@T` → `400 Bad Request`
- 只支持 origin-form（以 `/` 开头）；`GET http://host/ HTTP/1.1` 会被拒绝
- HTTP 版本不是 1.0/1.1，但以 `HTTP/` 开头 → `505`

请求目标还会检查：

- 不能包含 `#`（fragment 不应发给服务器）
- 不能包含空格、控制字符或反斜杠
- 路径和查询字符串在第一个 `?` 处分开

### 5.3 请求头

头名大小写不敏感，但值保留原样。`Headers` 用 `Vec` 而不是 `HashMap`：

```rust
pub struct Headers {
    entries: Vec<(String, String)>,
}
```

这样做有两个原因：

1. 保留同名头的顺序和重复项；判断 `Connection: keep-alive, close` 更方便。
2. 对安全敏感的头（例如 `Content-Length`）可以明确检测重复，而不是悄悄覆盖。

解析时：

- 头名转小写保存
- 拒绝空头名、非法 token、没有冒号的行
- 拒绝 obsolete line folding（下一行以空格/Tab 开头）
- 拒绝头值中的 CR/LF/NUL 等控制字符（允许 HTAB）
- 请求行与所有头合计不超过 16 KiB
- 头数量不超过 100
- HTTP/1.1 必须恰好有一个非空 `Host`
- 拒绝多个 `Host` 或带逗号的 `Host`

`Host` 的严格性不是形式主义。虚拟主机、绝对 URI、缓存键和路由都可能依赖它；歧义会带来安全问题。

### 5.4 body 的边界：`Content-Length` 与请求走私

HTTP/1.1 有两种常见的 body 帧定界方式：

- `Content-Length: N`
- `Transfer-Encoding: chunked`

我们只支持第一种。对第二种和其他组合采取“拒绝优先”：

```rust
let content_lengths: Vec<&str> = headers.get_all("content-length").collect();
let has_transfer_encoding = headers.get("transfer-encoding").is_some();

if has_transfer_encoding {
    if !content_lengths.is_empty() {
        return Err(RequestError::BadRequest(
            "Transfer-Encoding and Content-Length cannot be combined",
        ));
    }
    return Err(RequestError::UnsupportedTransferEncoding);
}
if content_lengths.len() > 1 {
    return Err(RequestError::BadRequest(
        "multiple Content-Length headers are not allowed",
    ));
}
```

这几行是在防 **HTTP request smuggling**：如果前置代理和后端服务器对“到底按 CL 还是 TE 算 body 长度”理解不同，攻击者可以把第二个请求藏进 body 里。对于教学服务器，最安全的策略是不接受歧义输入。

接下来：

```rust
if content_length > max_body_bytes as u64 {
    return Err(RequestError::BodyTooLarge); // 413，尚未分配
}

let mut body = vec![0; content_length as usize];
reader.read_exact(&mut body)?;
```

- 先检查上限，再分配，避免声明 100 GiB 就分配 100 GiB。
- `read_exact` 保证读取恰好 N 字节；提前 EOF 被视为不完整请求。
- body 最大 1 MiB，因此可以安全地整体放进 `Vec<u8>`。
- 生产服务器若支持大文件上传，需要流式 body、背压和磁盘落盘。

### 5.5 `Expect: 100-continue`

解析器不在读取 body 时直接访问 socket 写端，而是接收一个回调：

```rust
pub fn read_request<R: BufRead, F: FnOnce() -> io::Result<()>>(
    reader: &mut R,
    max_header_bytes: usize,
    max_body_bytes: usize,
    on_continue: F,
) -> Result<Option<Request>, RequestError>
```

只有同时满足以下条件才会调用：

- 请求头已通过校验
- 版本是 HTTP/1.1
- 值是 `100-continue`（大小写不敏感）
- body 大小没有超过限制

服务器传入的回调：

```rust
|| {
    writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
    writer.flush()
}
```

未知的 `Expect`、重复的 `Expect`、HTTP/1.0 的 `100-continue` 都会返回 `417 Expectation Failed`。集成测试会验证真的先在网络上收到 `100 Continue`，再收到最终响应。

### 5.6 keep-alive 规则

解析完头和 body 后，决定这个连接还能不能继续用：

```rust
let mut keep_alive = version == Version::Http11;
if headers.contains_token("connection", "close") {
    keep_alive = false;
} else if version == Version::Http10
    && headers.contains_token("connection", "keep-alive")
{
    keep_alive = true;
}
```

| 版本 | 默认 | 显式 `close` | 显式 `keep-alive` |
| --- | --- | --- | --- |
| HTTP/1.1 | 复用 | 关闭 | 复用 |
| HTTP/1.0 | 关闭 | 关闭 | 复用 |

`close` 优先。`contains_token` 会把多个 `Connection` 头和逗号分隔的 token 都考虑进去。

### 5.7 错误映射

`RequestError::client_status()` 决定给客户端什么响应：

| 错误 | 状态码 |
| --- | --- |
| 格式错误、缺少 Host、重复 CL、CL+TE | 400 Bad Request |
| 请求体超限 | 413 Payload Too Large |
| 请求行超长 | 414 URI Too Long |
| 未知 Expect | 417 Expectation Failed |
| 头部超限 | 431 Request Header Fields Too Large |
| 未知方法、chunked | 501 Not Implemented |
| HTTP 版本不支持 | 505 HTTP Version Not Supported |

协议错误发生后，连接会被关闭。原因是解析器已经不知道下一个字节属于哪个请求了，继续复用连接可能造成“响应错位”。

---

## 6. 构造响应：从 `Response` 到字节

`Response` 只保存三样东西：

```rust
pub struct Response {
    status: StatusCode,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}
```

业务代码通过构造函数创建：

```rust
Response::text(StatusCode::Ok, "hello\n")
Response::html(StatusCode::Ok, HOME_PAGE)
Response::json(StatusCode::Ok, r#"{"status":"ok"}"#)
Response::bytes(StatusCode::Ok, "application/octet-stream", bytes)
Response::empty(StatusCode::NoContent)
```

### 6.1 自动生成帧定界头

序列化时，服务器负责：

- 状态行：`HTTP/1.1 200 OK`
- `Server: mini-http/0.1`
- `Content-Length`
- `Connection: keep-alive` 或 `close`
- 最后的空行

业务代码**不能**覆盖 `Content-Length`、`Connection`、`Transfer-Encoding`、`Server`：

```rust
assert!(
    !matches!(
        lower_name.as_str(),
        "content-length" | "connection" | "transfer-encoding" | "server"
    ),
    "header {name} is managed by the server"
);
```

这不是过度限制。两个互相矛盾的 `Content-Length` 会让客户端或中间代理对响应边界产生歧义。

### 6.2 防 CRLF 注入

如果允许把用户输入直接塞进头值：

```rust
response.with_header("X-Name", user_input)
```

而 `user_input` 是 `evil\r\nSet-Cookie: admin=1`，就会产生响应头注入。`with_header` 会在调用点 panic：

```rust
assert!(is_token(&name), "invalid response header name");
assert!(
    is_valid_header_value(&value),
    "invalid response header value"
);
```

这里选择 fail-fast，而不是静默清洗，因为这类错误通常意味着调用方对输入来源判断有误。

### 6.3 HEAD 与 204

`HEAD` 的语义是“与 GET 相同的头，但不发送 body”。响应层负责：

```rust
if allows_body {
    writeln!(head, "Content-Length: {}\r", self.body.len())?;
}
// ...
if allows_body && !head_request {
    writer.write_all(&self.body)?;
}
```

所以 `HEAD /api/health` 仍然返回真实的 `Content-Length: 59`，只是后面没有 JSON。

`204 No Content` 则完全不同：它不允许 body，也不会生成 `Content-Length`：

```rust
pub const fn allows_body(self) -> bool {
    !matches!(self, Self::NoContent)
}
```

### 6.4 状态行版本

响应使用请求的协议版本：

```rust
response.write_http(&mut writer, request.version, is_head, keep_alive)
```

对 HTTP/1.0 客户端回 `HTTP/1.0 ...`，对 HTTP/1.1 回 `HTTP/1.1 ...`。同时，HTTP/1.0 的 keep-alive 会显式写 `Connection: keep-alive`。

当前实现没有发送 `Date` 头。HTTP/1.1 origin server 通常应发送它；生产实现需要加入 RFC 7231 日期格式。这是一个刻意留下的练习。

---

## 7. 路由与参数

`Router` 保存“方法 + 路径模式 + handler”：

```rust
pub type Handler = fn(&Request, &Params) -> Response;

struct Route {
    method: Method,
    segments: Vec<Segment>,
    handler: Handler,
}

enum Segment {
    Static(String),
    Param(String),
}
```

路由模式在启动时解析，非法模式直接 panic：

- 必须以 `/` 开头
- `/` 表示根路径
- 不能有空的路径段（例如 `/a//b`）
- 参数段以 `:` 开头，例如 `/users/:id`
- 参数名只能包含字母、数字、下划线
- 同一模式中参数名不能重复

### 7.1 路径先解码，再匹配

请求的 `path` 保留百分号编码；路由匹配前按 `/` 分段，再对每个段解码：

```rust
path[1..]
    .split('/')
    .map(|segment| percent_decode(segment, false))
    .collect()
```

这样：

- `/users/%E4%B8%96%E7%95%8C` 匹配 `/users/:id`，参数是 `世界`
- `/hello%2Fworld` 会解码成一个段 `hello/world`，而不是两个段
- 非 UTF-8 或非法 `%ZZ` → `400`

第 3 点很重要：百分号解码可能把 `%2F` 变成斜杠，也可能产生 `..`、NUL 等字符。**路由层只负责解码，绝不意味着参数可以安全地拼进文件路径或 SQL。** 业务层必须校验。

查询参数规则不同：`+` 在 query 中表示空格，在 path 中表示字面量 `+`。`src/http/uri.rs` 用 `plus_as_space` 区分这两种情况。

### 7.2 匹配与 405

对每条路由：

```rust
let Some(params) = route.matches(&path_segments) else {
    continue;
};
path_matched = true;
// ...
if route.method == request.method {
    return (route.handler)(request, &params);
}
```

遍历完后：

- 路径匹配但方法不匹配 → `405 Method Not Allowed`，带 `Allow: GET, HEAD`
- 路径完全不存在 → `404 Not Found`

`Allow` 有固定顺序，便于测试和缓存。GET 路由会自动把 HEAD 加入允许列表。

### 7.3 HEAD 回退

如果没有显式注册 HEAD 路由，但有 GET：

```rust
if request.method == Method::Head
    && route.method == Method::Get
    && head_fallback.is_none()
{
    head_fallback = Some((route, params));
}
```

先继续扫描，确保显式 HEAD 路由优先；找不到时才使用 GET 的结果。真正的 body 抑制在响应序列化层完成。

### 7.4 为什么不用正则

这个项目只需要静态段和一个 `:param`。手写分段匹配有几个好处：

- 无依赖、可预测
- 启动时就能 panic 掉非法模式
- 匹配复杂度 O(路由数 × 路径段数)
- 读起来就是教程本身

生产路由器会使用更高效的前缀树，并支持通配符、优先级、中间件、嵌套路由等。

---

## 8. 业务处理器

`src/app.rs` 把所有业务集中在一个函数里：

```rust
pub fn router() -> Router {
    Router::new()
        .get("/", home)
        .get("/hello", hello)
        .get("/api/health", health)
        .get("/api/users/:id", user)
        .post("/api/echo", echo)
        .get("/favicon.ico", favicon)
}
```

### 8.1 查询参数

```rust
match query_param(request.query.as_deref(), "name") {
    Ok(Some(name)) if !name.is_empty() => {
        Response::text(StatusCode::Ok, format!("Hello, {name}!\n"))
    }
    Ok(_) => Response::text(
        StatusCode::BadRequest,
        "missing query parameter: ?name=...\n",
    ),
    Err(_) => Response::text(
        StatusCode::BadRequest,
        "query parameter is not valid percent-encoded UTF-8\n",
    ),
}
```

`query_param` 返回 `Result<Option<String>, DecodeError>`：

- `Ok(None)`：没有这个参数
- `Ok(Some(...))`：解码成功
- `Err(...)`：百分号编码非法

重复参数采用“第一个生效”。如果想支持多个值，可以改用 `query_pairs`。

### 8.2 路径参数

```rust
let id = params.get("id").unwrap_or_default();
let Ok(id) = id.parse::<u64>() else {
    return Response::json(
        StatusCode::BadRequest,
        r#"{"error":"id must be an unsigned integer"}"#,
    );
};
let body = format!(r#"{{"id":{id},"name":"user-{id}"}}"#);
```

这里手工拼 JSON 是安全的，因为 `id` 已经被解析成 `u64`，不可能包含引号或控制字符。**如果参数是任意字符串，就必须使用 JSON 转义或 `serde_json`。** 手工拼接不是可以随意复制的模板。

### 8.3 回显 body

```rust
let content_type = request
    .header("content-type")
    .unwrap_or("application/octet-stream");

Response::bytes(StatusCode::Ok, content_type, request.body.clone())
    .with_header("X-Body-Bytes", request.body.len().to_string())
```

- 保留客户端 `Content-Type`，方便演示“二进制安全回显”
- `Content-Length` 由响应层自动计算
- body 已经受 1 MiB 上限约束

### 8.4 为什么 handler 是函数指针

当前设计不支持带状态的闭包：

```rust
pub type Handler = fn(&Request, &Params) -> Response;
```

优点：`Router` 自动 `Send + Sync`，无生命周期烦恼，教程更聚焦 HTTP。

如果要做内存 CRUD，需要把 `Handler` 换成：

```rust
type Handler = Arc<dyn Fn(&Request, &Params) -> Response + Send + Sync>;
```

然后捕获一个 `Arc<Mutex<AppState>>`。这是练习之一。

---

## 9. 一个连接的完整生命周期

`serve_connection` 是本项目的主循环。伪代码：

```text
设置超时
while 请求数 < 每连接上限:
    request = read_request()
    如果 EOF: 结束
    如果协议错误: 写错误响应, 关闭
    response = router.dispatch(request)
    keep_alive = request.keep_alive && 未达上限
    write_response(response, request.version, HEAD?, keep_alive)
    如果 !keep_alive: 结束
```

关键决策：

### 9.1 何时关闭

以下情况发送响应后关闭：

- 请求带 `Connection: close`
- HTTP/1.0 且没有请求 keep-alive
- 达到 `max_requests_per_connection`（默认 100）
- 协议解析失败
- 响应写失败
- 超时

这避免了两个常见问题：连接永远不释放，以及解析失败后继续复用连接导致请求边界错位。

### 9.2 请求超时

首次请求读取在 10 秒内没有任何进展（单次 `read` 超时）：

```rust
if served == 0 && error.kind() == TimedOut {
    response = Response::text(StatusCode::RequestTimeout, ...);
    response.write_http(..., false, false)?;
}
```

发送 `408 Request Timeout`。如果已经服务过请求，keep-alive 空闲超时则直接关闭，不再额外写响应，避免客户端误以为这是上一个请求的响应。

### 9.3 pipelining

客户端可以连续发送：

```http
GET /a HTTP/1.1
Host: x

GET /b HTTP/1.1
Host: x
Connection: close

```

`BufReader` 会缓冲第二个请求。主循环写完第一个响应后，继续调用 `read_request`，自然消费第二个。我们的实现是串行处理，不并行，也不会打乱响应顺序。

### 9.4 日志

```text
127.0.0.1:61719 "GET /api/health" -> 200 (59 bytes, 25us)
```

记录 peer、方法、原始 target、状态码、body 字节数和 handler 耗时。生产环境应换成结构化日志（tracing），记录请求 ID、延迟分布，并避免把敏感 query 参数写进日志。

---

## 10. 测试与调试

### 10.1 运行全部检查

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

当前测试包括：

- 请求行、头、query、body
- HTTP/1.0 / 1.1 的 keep-alive 规则
- 缺失 Host、重复 CL、CL+TE、非法方法/版本
- 请求头和 body 上限
- 百分号解码与非法 UTF-8
- 响应序列化、HEAD、204、头注入防护
- 路由参数、404、405、HEAD 回退
- 真实 TCP：keep-alive 多请求、POST 回显、HEAD、405、100-continue、请求走私拒绝

### 10.2 为什么集成测试不启动 `Server::run`

`run` 是无限 accept 循环，没有内置 shutdown。集成测试直接绑定一个临时端口、`accept` 一个连接，然后调用 `serve_connection`：

```rust
let listener = TcpListener::bind("127.0.0.1:0")?;
let address = listener.local_addr()?;
thread::spawn(move || {
    let (stream, _) = listener.accept().unwrap();
    serve_connection(stream, Arc::new(app::router()), ServerConfig::default());
});
```

这样测试的是真实 socket 上的字节，而不是 mock 出来的函数调用；同时避免为测试引入一个假的全局关闭标志。

生产版本需要 `shutdown`：通常由 `tokio::select!`、`CancellationToken` 或信号处理驱动 accept 循环退出，并等待在途请求结束。

### 10.3 用 curl 观察协议

```bash
curl -v http://127.0.0.1:7878/api/health
```

```text
> GET /api/health HTTP/1.1
> Host: 127.0.0.1:7878
> User-Agent: curl/...
> Accept: */*
>
< HTTP/1.1 200 OK
< Server: mini-http/0.1
< Content-Length: 59
< Content-Type: application/json; charset=utf-8
< Connection: keep-alive
```

HEAD：

```bash
curl -I http://127.0.0.1:7878/api/health
```

看到 `Content-Length: 59`，但终端没有 59 字节 body，就说明 HEAD 路径正确。

405：

```bash
curl -i -X POST http://127.0.0.1:7878/api/health
```

```text
HTTP/1.1 405 Method Not Allowed
Allow: GET, HEAD
```

100-continue（显式让 curl 等待临时响应；某些 curl 版本对较大 body 也会自动添加）：

```bash
dd if=/dev/zero of=/tmp/payload.bin bs=1024 count=2
curl -v -H 'Expect: 100-continue' -X POST --data-binary @/tmp/payload.bin \
  -H 'Content-Type: application/octet-stream' \
  http://127.0.0.1:7878/api/echo
```

在详细输出中可以看到 `100 Continue`。

用 `nc` 手工发请求：

```bash
printf 'GET /api/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n' \
  | nc 127.0.0.1 7878
```

### 10.4 常见调试问题

| 现象 | 原因 |
| --- | --- |
| `curl` 卡住 | 忘了最后空行；或 keep-alive 下客户端在等更多数据 |
| 405 而不是 404 | 路径存在，方法不对；看 `Allow` |
| 411 不存在 | 本项目没有强制 Content-Length；空 body 合法 |
| 417 | `Expect` 不是 HTTP/1.1 的 `100-continue` |
| 431 | 请求行+头总字节超过 16 KiB 或头数超过 100 |
| 501 | 方法或 Transfer-Encoding 未实现 |
| 505 | HTTP/2 之类的版本直接打到了这个明文服务器 |

---

## 11. 并发、限制与安全边界

### 11.1 硬限制一览

| 限制 | 默认值 | 作用 |
| --- | --- | --- |
| 请求行+头总量 | 16 KiB | 防头部内存膨胀 |
| 单个请求行 | 8 KiB | 防超长 URI |
| 单个头行 | 8 KiB | 防单行膨胀 |
| 请求头数量 | 100 | 防大量小头 |
| body | 1 MiB | 整体内存可控 |
| 每连接请求数 | 100 | 防连接无限复用 |
| 同时连接数 | 256 | 防线程耗尽 |
| 读/写超时 | 10s | 防慢连接 |

### 11.2 已经处理的风险

**请求走私**

- 拒绝 CL + TE
- 拒绝重复 CL
- 只接受一种明确的消息边界

**响应头注入**

- 头名必须是 HTTP token
- 头值拒绝 CR/LF/NUL 和控制字符
- 禁止业务覆盖帧定界头

**空闲连接 / 停滞请求**

- 单次读写超时
- 头总上限
- 每连接请求数上限
- 提醒：没有请求级总 deadline，持续慢速发送仍可延长单个请求的占用时间；连接数上限只能限制影响范围

**资源耗尽**

- body 上限在分配前检查
- 连接数上限
- 错误请求后关闭连接

**路径参数误用**

- 框架明确文档化“解码不等于安全”
- 示例 handler 对 `id` 做整数校验

### 11.3 仍然不足的地方

- **thread-per-connection**：256 个慢连接就能占满服务器；且线程栈有成本。
- **没有请求级总超时**：10 秒是单次 socket 读写超时，持续慢速发送仍能延长请求处理时间。
- **无 TLS**：所有内容明文传输，不能用于公网。
- **无认证/授权**：不要把管理接口直接暴露。
- **无请求 panic 隔离**：handler panic 会终止连接线程。生产服务器应捕获 panic 并返回 500。
- **无流式 body**：大文件上传不适用。
- **无 `Date` 头**：不符合 HTTP/1.1 origin server 的完整要求。
- **日志阻塞**：stdout 是同步写入，高并发下可能成为瓶颈。
- **无优雅退出**：Ctrl+C 会直接结束进程，在途响应可能被截断。
- **仅 UTF-8**：请求行和头必须是合法 UTF-8；某些历史客户端使用 Latin-1 的场景会被拒绝。

如果要面公网，最省事且最安全的做法是：

```text
Client ──TLS──► Caddy / Nginx ──HTTP──► 你的 Rust 服务
```

把 TLS、限流、连接队列、慢请求防护交给成熟反向代理，应用只绑定内网地址。

---

## 12. 练习

建议按顺序做，每次只加一个能力并补测试。

### 练习 1：`GET /headers`

返回请求头（注意排序和转义）：

```text
Host: localhost
User-Agent: curl/...
```

提示：`request.headers.iter()`。需要把每行拼成纯文本，或使用 JSON 序列化。不要反射 `Cookie` 等敏感头到公网响应。

### 练习 2：内存 CRUD

实现：

- `GET /api/items`
- `POST /api/items`
- `GET /api/items/:id`
- `DELETE /api/items/:id`

需要改 `Handler` 类型，让路由能捕获 `Arc<Mutex<HashMap<...>>>`。提示：

```rust
type Handler =
    Arc<dyn Fn(&Request, &Params) -> Response + Send + Sync>;
```

并为每个 `Route` 保存一个 `Arc<Handler>`。注意不要在持锁时执行 I/O（本项目的响应在 handler 返回后才写，天然比较容易避免）。

### 练习 3：静态文件

实现 `GET /static/*path` 时，必须：

1. 解码后拒绝包含 `..` 的路径段
2. 用 `std::fs::canonicalize` 得到真实路径
3. 确认真实路径仍在静态目录的 canonical 路径下
4. 只允许普通文件
5. 设置正确的 `Content-Type`，并处理 `HEAD`

即便如此，生产环境更推荐使用成熟静态文件服务器或 `tower-http::services::ServeDir`。注意路径校验和打开文件之间可能存在的 TOCTOU 问题。

### 练习 4：chunked 请求体

实现一个增量状态机：

```text
读 size 行（十六进制，忽略扩展）
→ 读 size 字节 + CRLF
→ 重复，直到 size == 0
→ 读 trailers 直到空行
```

需要严格限制：

- 每个 chunk 大小
- 总解码大小
- chunk 行长度
- trailers 数量

并写测试覆盖：chunk 分裂、非法十六进制、超限、缺少 CRLF、chunk 扩展。

### 练习 5：优雅退出

目标：Ctrl+C 或测试调用 shutdown 后：

- 不再 accept 新连接
- 等待在途连接完成（或超时）
- 关闭 listener

提示：`TcpListener::set_nonblocking(true)` + `AtomicBool`，或改用 Tokio 的 `CancellationToken`。为退出逻辑加测试比“手动 Ctrl+C 试试”可靠得多。

### 练习 6：线程池或 Tokio

把每连接一线程替换成固定大小线程池，或者用 Tokio 重写：

```text
TcpListener -> tokio::spawn(read/parse/dispatch/write)
```

思考：阻塞文件 I/O、CPU 密集 handler、连接数上限、优雅退出分别怎么处理？同步代码和 async 代码的边界在哪里？

### 练习 7：`Date` 头

实现 RFC 7231 的 IMF-fixdate：

```text
Sun, 06 Nov 1994 08:49:37 GMT
```

可以用 `time` / `httpdate` crate，也可以练习从 Unix timestamp 手工换算。加测试覆盖闰年、月末和时区。

### 练习 8：用 `serde_json`

把手工 JSON 替换为：

```rust
#[derive(serde::Serialize)]
struct User { id: u64, name: String }
```

对比手工拼接的边界：数字类型安全，但字符串、嵌套结构和错误响应都需要统一处理。

### 练习 9：端到端 TLS

用 `rustls` 包一层 `TcpStream`，加载自签名证书，用 `curl -k` 测试。然后思考：

- 证书轮换怎么做？
- ALPN 如何协商？
- 为什么很多团队仍然让 Caddy/Nginx 终止 TLS？

---

## 13. 从教学服务器到生产环境

真正上线时，你通常不会继续扩展这个项目，而是切换到成熟组件：

| 需求 | 推荐方案 |
| --- | --- |
| async HTTP/1.1 + HTTP/2 | `hyper` |
| 路由、提取器、中间件 | `axum` |
| 结构化日志、追踪 | `tracing` + `tracing-subscriber` |
| JSON | `serde` / `serde_json` |
| 配置 | `figment` / `config` / 环境变量 |
| 优雅退出 | `tokio::signal` + `CancellationToken` |
| 限流、超时、压缩 | `tower` / `tower-http` |
| TLS | 反向代理，或 `rustls` + `tokio-rustls` |
| 测试 | `tower::ServiceExt`、`reqwest`、真实监听端口 |

一个常见的生产分层：

```text
Internet
  │
  ▼
Caddy / Nginx        TLS、HTTP/2、限流、连接队列、静态文件、压缩
  │
  ▼
Rust 应用            业务路由、鉴权、数据库、观测
  │
  ├─ PostgreSQL
  ├─ Redis
  └─ 上游服务
```

### 上线前的检查清单

- [ ] 绑定内网地址，TLS 由可信入口终止
- [ ] 连接、请求头、body、并发都有上限
- [ ] 读/写/空闲超时
- [ ] 解析错误不会复用连接
- [ ] 响应头不反射未转义的用户输入
- [ ] 参数经过业务校验，不直接拼路径/SQL/命令
- [ ] panic 会被捕获并返回 500（或至少记录）
- [ ] 结构化访问日志，不泄露 token/cookie/密码
- [ ] 优雅退出，等待在途请求
- [ ] 压力测试和慢连接测试
- [ ] 依赖审计与最小化
- [ ] 有健康检查和 metrics

### 这个项目真正教给你的

不是“如何手写一个替代 axum 的框架”，而是：

- HTTP 是**有边界的字节协议**，每条边界都值得认真处理
- 安全问题的根源常常是“两个组件对同一段字节的理解不同”
- 限制、超时和错误策略是协议实现的一部分，不是上线前才补的装饰
- 一个清晰的分层能让协议测试和业务测试互不干扰

把这些直觉带到生产框架里，你读文档、调中间件、排查请求走私和连接问题时，会快很多。

---

## 附录：源码速查

| 想了解 | 看 |
| --- | --- |
| 请求如何解析 | `src/http/request.rs` 的 `read_request` |
| 有界读行 | `src/http/request.rs` 的 `read_line` |
| keep-alive 判定 | `src/http/request.rs` 末尾 |
| 响应如何写回 socket | `src/http/response.rs` 的 `write_http` |
| 连接循环 | `src/server.rs` 的 `serve_connection` |
| 并发与上限 | `src/server.rs` 的 `Server::run` |
| 404 / 405 / HEAD 回退 | `src/router.rs` 的 `dispatch` |
| 百分号解码与 query | `src/http/uri.rs` |
| 示例 handler | `src/app.rs` |
| 真实 TCP 测试 | `tests/integration.rs` |
