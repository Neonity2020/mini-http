use crate::http::{query_param, Request, Response, StatusCode};
use crate::router::{Params, Router};

const HOME_PAGE: &str = r#"<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>mini-http · Rust HTTP server</title>
  <style>
    :root { color-scheme: dark; --bg:#10141b; --panel:#171d27; --text:#edf1f7; --muted:#9aa7ba; --accent:#ffb454; --line:#2a3444; }
    * { box-sizing: border-box; }
    body { margin:0; min-height:100vh; background:radial-gradient(circle at 15% 10%, #243147 0, transparent 38%), var(--bg); color:var(--text); font:16px/1.65 ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif; }
    main { width:min(760px, calc(100% - 40px)); margin:0 auto; padding:72px 0 80px; }
    .eyebrow { margin:0; color:var(--accent); font:600 12px/1.4 ui-monospace, SFMono-Regular, Menlo, monospace; letter-spacing:.18em; text-transform:uppercase; }
    h1 { margin:10px 0 12px; font-size:clamp(42px, 8vw, 72px); line-height:1; letter-spacing:-.055em; }
    h2 { margin:38px 0 12px; font-size:20px; }
    .lead { max-width:620px; margin:0; color:var(--muted); font-size:18px; }
    .status { display:inline-flex; align-items:center; gap:9px; margin-top:24px; padding:7px 12px; border:1px solid var(--line); border-radius:999px; color:#b9e6c4; background:#142019; font-size:13px; }
    .dot { width:8px; height:8px; border-radius:50%; background:#63d47f; box-shadow:0 0 12px #63d47f; }
    table { width:100%; border-collapse:collapse; overflow:hidden; border:1px solid var(--line); border-radius:12px; background:var(--panel); font-size:14px; }
    th, td { padding:11px 14px; text-align:left; border-bottom:1px solid var(--line); }
    th { color:var(--muted); font-size:12px; letter-spacing:.08em; text-transform:uppercase; }
    tr:last-child td { border-bottom:0; }
    code, pre { font-family:ui-monospace, SFMono-Regular, Menlo, monospace; }
    td code { color:var(--accent); }
    form { display:flex; gap:10px; margin-top:14px; }
    input { flex:1; min-width:0; padding:11px 13px; border:1px solid var(--line); border-radius:9px; background:#0d1117; color:var(--text); font:inherit; }
    input:focus { outline:2px solid #4c7dbe; outline-offset:1px; }
    button { padding:11px 17px; border:0; border-radius:9px; background:var(--accent); color:#18120a; font:inherit; font-weight:700; font-size:14px; cursor:pointer; }
    pre { overflow:auto; margin:0; padding:18px; border:1px solid var(--line); border-radius:12px; background:#0b0f15; color:#c8d4e4; font-size:13px; line-height:1.7; }
    .note { margin-top:34px; padding-top:18px; border-top:1px solid var(--line); color:var(--muted); font-size:13px; }
    a { color:var(--accent); }
  </style>
</head>
<body>
  <main>
    <p class="eyebrow">Rust · TCP · HTTP/1.1</p>
    <h1>mini-http</h1>
    <p class="lead">一个不依赖第三方 crate、从 TCP 字节流开始实现的 HTTP 服务器。代码本身就是教程：解析请求、构造响应、路由、keep-alive 与并发边界。</p>
    <div class="status"><span class="dot"></span> server is running</div>

    <h2>路由</h2>
    <table>
      <tr><th>方法</th><th>路径</th><th>作用</th></tr>
      <tr><td>GET</td><td><code>/</code></td><td>这个页面</td></tr>
      <tr><td>GET</td><td><code>/hello?name=Ada</code></td><td>查询参数示例</td></tr>
      <tr><td>GET</td><td><code>/api/health</code></td><td>健康检查 JSON</td></tr>
      <tr><td>GET</td><td><code>/api/users/:id</code></td><td>路径参数示例</td></tr>
      <tr><td>POST</td><td><code>/api/echo</code></td><td>回显请求体</td></tr>
    </table>

    <h2>试试查询参数</h2>
    <form action="/hello" method="get">
      <input name="name" value="Rust" aria-label="名字">
      <button type="submit">发送 GET</button>
    </form>

    <h2>用 curl</h2>
    <pre><code>curl -i http://127.0.0.1:7878/api/health
curl -i "http://127.0.0.1:7878/hello?name=Rust"
curl -i http://127.0.0.1:7878/api/users/42
curl -i -X POST --data-binary 'hello' \
  -H 'Content-Type: text/plain' \
  http://127.0.0.1:7878/api/echo</code></pre>

    <p class="note">默认监听 127.0.0.1:7878。完整原理、限制与练习见项目根目录的 TUTORIAL.md。</p>
  </main>
</body>
</html>
"#;

const HEALTH_JSON: &str = r#"{"status":"ok","service":"mini-http","protocol":"HTTP/1.1"}"#;

/// Build the application's route table.
pub fn router() -> Router {
    Router::new()
        .get("/", home)
        .get("/hello", hello)
        .get("/api/health", health)
        .get("/api/users/:id", user)
        .post("/api/echo", echo)
        .get("/favicon.ico", favicon)
}

fn home(_: &Request, _: &Params) -> Response {
    Response::html(StatusCode::Ok, HOME_PAGE).with_header("Cache-Control", "no-store")
}

fn hello(request: &Request, _: &Params) -> Response {
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
}

fn health(_: &Request, _: &Params) -> Response {
    Response::json(StatusCode::Ok, HEALTH_JSON).with_header("Cache-Control", "no-store")
}

fn user(_: &Request, params: &Params) -> Response {
    let id = params.get("id").unwrap_or_default();
    let Ok(id) = id.parse::<u64>() else {
        return Response::json(
            StatusCode::BadRequest,
            r#"{"error":"id must be an unsigned integer"}"#,
        );
    };

    let body = format!(r#"{{"id":{id},"name":"user-{id}"}}"#);
    Response::json(StatusCode::Ok, body)
}

fn echo(request: &Request, _: &Params) -> Response {
    let content_type = request
        .header("content-type")
        .unwrap_or("application/octet-stream");

    Response::bytes(StatusCode::Ok, content_type, request.body.clone())
        .with_header("X-Body-Bytes", request.body.len().to_string())
        .with_header("Cache-Control", "no-store")
}

fn favicon(_: &Request, _: &Params) -> Response {
    Response::empty(StatusCode::NoContent)
}
