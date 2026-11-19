# mini-http

一个**不依赖任何第三方 crate**、从 TCP 字节流开始手写的 Rust HTTP/1.1 教学服务器。

它不只是“能跑”：请求行/请求头解析、`Content-Length` 帧定界、keep-alive、HEAD、路由、百分号解码、`100-continue`、连接数上限、超时和多线程并发都有明确实现，并配有单元测试与真实 TCP 集成测试。

> 完整原理讲解：[TUTORIAL.md](TUTORIAL.md)

## 快速开始

需要 Rust 1.75 或更高版本。

```bash
cargo run
```

默认监听 `http://127.0.0.1:7878`，打开浏览器即可看到演示页。

```bash
# 健康检查
curl -i http://127.0.0.1:7878/api/health

# 查询参数（支持百分号编码）
curl -i "http://127.0.0.1:7878/hello?name=%E4%B8%96%E7%95%8C"

# 路径参数
curl -i http://127.0.0.1:7878/api/users/42

# 回显请求体
curl -i -X POST \
  --data-binary 'hello' \
  -H 'Content-Type: text/plain' \
  http://127.0.0.1:7878/api/echo

# HEAD 自动复用 GET 路由；响应有 Content-Length，但没有 body
curl -I http://127.0.0.1:7878/api/health
```

自定义监听地址：

```bash
cargo run -- 0.0.0.0:3000
MINI_HTTP_ADDR=127.0.0.1:9000 cargo run
cargo run -- 127.0.0.1:0        # 让系统选择空闲端口
```

```bash
cargo run -- --help
```

## 路由

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/` | HTML 演示页 |
| GET | `/hello?name=Ada` | 查询参数与百分号解码 |
| GET | `/api/health` | JSON 健康检查 |
| GET | `/api/users/:id` | 路径参数；`id` 必须是无符号整数 |
| POST | `/api/echo` | 回显请求体，保留请求的 `Content-Type` |
| GET | `/favicon.ico` | 返回 `204 No Content` |

同一路径存在但方法不匹配时返回 `405`，并带上 `Allow`（GET 路由会自动包含 HEAD）：

```text
HTTP/1.1 405 Method Not Allowed
Allow: GET, HEAD
```

## 已实现

- HTTP/1.0 与 HTTP/1.1 请求行、请求头
- `Content-Length` 请求体，最大 1 MiB
- keep-alive、HTTP/1.0 兼容规则、每连接请求数上限
- `HEAD` 自动复用 `GET`，保留正确的 `Content-Length` 但不发 body
- `Expect: 100-continue` 临时响应
- 大小写不敏感的请求头、重复头保留
- 16 KiB 请求行+请求头上限、100 个头上限、行长度上限
- 明确的 `400/413/414/417/431/501/505` 错误映射
- 拒绝重复 `Content-Length`、`Content-Length + Transfer-Encoding`
- 路径参数、查询参数、UTF-8 百分号解码
- 每连接一个线程；并发连接上限（超限返回 503）
- 读/写超时：空闲或长时间没有任何读进展的连接会被断开
- 响应头 CRLF 注入防护、帧定界头由服务器独占
- 真实 TCP 集成测试：keep-alive、HEAD、405、100-continue、请求走私防护

## 故意不实现

这是一个教学项目，不是生产服务器：

- 没有 TLS、HTTP/2、WebSocket
- 不支持 `Transfer-Encoding: chunked`
- 没有异步 I/O / epoll / io_uring
- 没有线程池；每个连接一个线程，适合学习但不适合 C10K
- 没有静态文件、压缩、缓存、认证、会话
- 没有 graceful shutdown（Ctrl+C 直接结束进程）
- 没有 `Date` 头、访问日志轮转、tracing
- handler panic 会终止该连接线程，不会自动转成 500

教程最后一章会说明这些能力在生产环境中如何补齐，以及何时应该直接使用 `axum` / `hyper`。

## 项目结构

```text
rust-http-server/
├── Cargo.toml
├── README.md
├── TUTORIAL.md
├── src/
│   ├── main.rs              # 参数解析、启动服务器
│   ├── lib.rs               # 模块导出
│   ├── app.rs               # 业务路由与 handler
│   ├── router.rs            # 方法 + 路径匹配、参数捕获
│   ├── server.rs            # accept 循环、连接线程、限制
│   └── http/
│       ├── mod.rs
│       ├── method.rs        # HTTP 方法
│       ├── status.rs        # 状态码与 reason phrase
│       ├── uri.rs           # 查询参数、百分号解码
│       ├── request.rs       # 请求解析与帧定界
│       └── response.rs      # 响应序列化
└── tests/
    └── integration.rs       # 真实 TCP 请求/响应测试
```

请求的生命周期：

```text
TcpListener::accept
        │
        ▼
thread::spawn ──► serve_connection
                       │
                       ├─ read_request ──► Request
                       │      （限制、CL/TE、Host、100-continue）
                       │
                       ├─ Router::dispatch ──► Response
                       │      （404 / 405 / handler）
                       │
                       └─ Response::write_http
                              （Content-Length、HEAD、Connection）
```

## 测试与检查

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

测试覆盖：

- 请求行、头、请求体、HTTP/1.0 keep-alive
- 缺失 Host、重复 Content-Length、CL+TE、非法方法/版本
- 请求头与请求体上限
- HEAD 响应体抑制、204 无 body
- 路由参数、百分号解码、404/405/Allow
- 真实 TCP 上的 keep-alive 多请求、POST 回显、HEAD、100-continue

## 安全边界

默认只绑定 `127.0.0.1`。如果你改成 `0.0.0.0`，请只在可信网络中运行；它没有 TLS，也没有身份认证。

已经处理的常见问题：

- 请求走私：拒绝 `Content-Length` 与 `Transfer-Encoding` 同时出现，拒绝重复 `Content-Length`
- 响应头注入：拒绝头名/头值中的非法字符，禁止业务代码覆盖帧定界头
- 慢连接：读/写超时 + 每连接最多 100 个请求（注意超时按单次读写计算）
- 内存耗尽：请求行+请求头 16 KiB、请求体 1 MiB、连接数 256 的硬上限
- 路径参数误用：框架会做百分号解码；业务代码仍必须校验（例如本例的 `id` 只接受数字）

仍然要注意：

- 每连接一线程可被大量并发连接耗尽；上限只能缓解
- 请求体整体读入内存；大文件上传需要流式处理
- 没有请求处理 panic 隔离
- 日志直接写 stdout，可能成为吞吐瓶颈

## 继续练习

教程中附有练习和提示，包括：内存 CRUD、静态文件服务、chunked 解析、优雅退出、线程池、Tokio 重写、`Date` 头和 TLS。

## License

MIT
