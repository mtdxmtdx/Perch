//! 转发模式：`perch.exe --hook <agent> --port <端口> --token <令牌>`
//! Codex 只支持命令型 hook，由它启动本程序；从标准输入读事件 JSON，POST 给正在运行的桌宠后立即退出。
//! 不启动窗口、不经过单实例检查；桌宠没运行时静默退出，绝不影响 Codex。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

const MAX_INPUT: u64 = 8 * 1024 * 1024;

fn arg_after(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

/// 命令行里有 --hook 时执行转发并返回退出码；否则返回 None，照常启动桌宠
pub fn run_from_args(args: &[String]) -> Option<i32> {
    let agent = arg_after(args, "--hook")?;
    let port: u16 = arg_after(args, "--port").and_then(|p| p.parse().ok()).unwrap_or(47821);
    let token = arg_after(args, "--token").unwrap_or_default();
    let mut body = Vec::new();
    let _ = std::io::stdin().take(MAX_INPUT).read_to_end(&mut body);
    if !body.is_empty() {
        let _ = post(port, &agent, &token, &body);
    }
    // 不往标准输出写任何东西：Codex 会把输出当作 hook 的决定来解析
    Some(0)
}

fn post(port: u16, agent: &str, token: &str, body: &[u8]) -> std::io::Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(300))?;
    s.set_write_timeout(Some(Duration::from_millis(800)))?;
    s.set_read_timeout(Some(Duration::from_millis(800)))?;
    let head = format!(
        "POST /v1/agent-event/{agent} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nX-Perch-Token: {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes())?;
    s.write_all(body)?;
    // 读一下响应，确保服务端已经收下再退出
    let mut buf = [0u8; 64];
    let _ = s.read(&mut buf);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_args_and_posts() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            let mut buf = vec![0u8; 4096];
            let n = c.read(&mut buf).unwrap();
            let _ = c.write_all(b"HTTP/1.1 204 No Content\r\n\r\n");
            String::from_utf8_lossy(&buf[..n]).to_string()
        });
        post(port, "codex", "tok", br#"{"hook_event_name":"Stop"}"#).unwrap();
        let req = h.join().unwrap();
        assert!(req.starts_with("POST /v1/agent-event/codex HTTP/1.1"));
        assert!(req.contains("X-Perch-Token: tok"));
        assert!(req.ends_with(r#"{"hook_event_name":"Stop"}"#));
        // 没有 --hook：照常启动
        assert!(run_from_args(&["perch.exe".into()]).is_none());
        // 端口上没人：静默失败
        assert!(post(1, "codex", "t", b"{}").is_err());
    }
}
