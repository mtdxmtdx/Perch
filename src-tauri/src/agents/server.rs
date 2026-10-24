//! 本地事件服务：只监听 127.0.0.1，校验令牌（防止网页之类的本机其他来源乱发事件）。

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tauri::AppHandle;

#[derive(Clone)]
struct Ctx {
    app: AppHandle,
    token: String,
}

async fn event(State(ctx): State<Ctx>, Path(agent): Path<String>, headers: HeaderMap, body: Json<Value>) -> StatusCode {
    // 也认改名前的头，没来得及修复的旧 hook 照常能用
    let ok = ["x-perch-token", "x-table-pet-token"]
        .iter()
        .any(|h| headers.get(*h).and_then(|v| v.to_str().ok()) == Some(ctx.token.as_str()));
    if !ok {
        return StatusCode::UNAUTHORIZED;
    }
    super::handle(&ctx.app, &agent, body.0);
    // 2xx 且没有正文：Claude Code 当作成功，不做任何决定
    StatusCode::NO_CONTENT
}

async fn health() -> Json<Value> {
    Json(json!({ "app": "perch" }))
}

/// 从配置的端口开始，被占用就往后试几个；返回实际监听的端口
pub async fn start(app: AppHandle, port: u16, token: String) -> Result<u16, String> {
    let mut last_err = String::new();
    for p in port..port.saturating_add(10) {
        match tokio::net::TcpListener::bind(("127.0.0.1", p)).await {
            Ok(listener) => {
                let router = Router::new()
                    .route("/v1/agent-event/{agent}", post(event))
                    .route("/health", get(health))
                    // PostToolUse 会带上工具输出，可能很大
                    .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
                    .with_state(Ctx { app, token });
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = axum::serve(listener, router).await {
                        eprintln!("[agents] 事件服务退出：{e}");
                    }
                });
                return Ok(p);
            }
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(format!("端口 {port} 起的 10 个端口都被占用了：{last_err}"))
}
