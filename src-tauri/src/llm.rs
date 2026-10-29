//! OpenAI 兼容的 chat/completions 客户端：流式（SSE）和一次性两种调用。

use crate::config::{self, Provider};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Clone, Debug, Serialize)]
pub struct ChatMessage {
    pub role: String,
    /// 带 tool_calls 的助手消息可以没有正文，按规范发 null
    pub content: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// 思考模型在调用工具的那一轮里返回的思考过程，同一轮内要原样传回（DeepSeek、通义等的要求）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON 字符串
    pub arguments: String,
}

/// 一次流式调用的结果
#[derive(Debug, Default)]
pub struct StreamOut {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    /// 通过 reasoning_content 字段返回的思考过程（需要在同一轮里传回）
    pub reasoning_content: String,
    /// 服务拒绝了推理参数，已去掉参数重发
    pub reasoning_dropped: bool,
}

/// 推理强度设置
#[derive(Clone, Debug)]
pub struct Reasoning {
    /// off / low / medium / high
    pub effort: String,
    /// openai / openrouter / qwen / deepseek
    pub style: String,
}

/// 按服务的参数格式生成推理参数；不认识的组合返回 None（什么都不发）
pub fn reasoning_params(r: &Reasoning) -> Option<serde_json::Map<String, Value>> {
    let e = r.effort.as_str();
    if !matches!(e, "off" | "low" | "medium" | "high") {
        return None;
    }
    let v = match r.style.as_str() {
        "openrouter" if e == "off" => json!({ "reasoning": { "enabled": false } }),
        "openrouter" => json!({ "reasoning": { "effort": e } }),
        // DeepSeek：默认开思考、强度 high；只认 low / high / max
        "deepseek" if e == "off" => json!({ "thinking": { "type": "disabled" } }),
        "deepseek" => {
            let effort = match e { "low" => "low", "medium" => "high", _ => "max" };
            json!({ "thinking": { "type": "enabled" }, "reasoning_effort": effort })
        }
        "qwen" if e == "off" => json!({ "enable_thinking": false }),
        "qwen" => {
            let budget = match e { "low" => 1024, "medium" => 4096, _ => 16384 };
            json!({ "enable_thinking": true, "thinking_budget": budget })
        }
        _ if e == "off" => json!({ "reasoning_effort": "none" }),
        _ => json!({ "reasoning_effort": e }),
    };
    v.as_object().cloned()
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<Part>),
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Clone, Debug, Serialize)]
pub struct ImageUrl {
    pub url: String,
}

impl ChatMessage {
    pub fn text(role: &str, text: impl Into<String>) -> Self {
        Self { role: role.into(), content: Some(Content::Text(text.into())), tool_calls: None, tool_call_id: None, reasoning_content: None }
    }

    pub fn parts(role: &str, parts: Vec<Part>) -> Self {
        Self { role: role.into(), content: Some(Content::Parts(parts)), tool_calls: None, tool_call_id: None, reasoning_content: None }
    }

    pub fn assistant_tool_calls(text: &str, calls: Vec<ToolCall>) -> Self {
        let content = (!text.is_empty()).then(|| Content::Text(text.into()));
        Self { role: "assistant".into(), content, tool_calls: Some(calls), tool_call_id: None, reasoning_content: None }
    }

    /// 同一轮里的思考过程，非空才带上
    pub fn with_reasoning(mut self, reasoning: &str) -> Self {
        if !reasoning.is_empty() {
            self.reasoning_content = Some(reasoning.to_string());
        }
        self
    }

    pub fn tool_result(call_id: &str, text: impl Into<String>) -> Self {
        Self { role: "tool".into(), content: Some(Content::Text(text.into())), tool_calls: None, tool_call_id: Some(call_id.into()), reasoning_content: None }
    }
}

/// 调用一个 provider 所需的全部信息
#[derive(Clone, Debug)]
pub struct Endpoint {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    /// 只有主对话会带推理参数；后台任务（总结、记忆、看图）不带，保持快和省
    pub reasoning: Option<Reasoning>,
}

impl Endpoint {
    pub fn from_provider(p: &Provider) -> Self {
        Self {
            base_url: p.base_url.trim_end_matches('/').to_string(),
            model: p.model.clone(),
            api_key: config::get_api_key(&p.id),
            reasoning: None,
        }
    }

    /// 主对话用：带上推理强度（unsupported 为 true 表示之前已确认这个模型不接受，就不发了）
    pub fn for_chat(p: &Provider, unsupported: bool) -> Self {
        let mut ep = Self::from_provider(p);
        if !unsupported && !p.reasoning_effort.is_empty() {
            ep.reasoning = Some(Reasoning { effort: p.reasoning_effort.clone(), style: p.reasoning_style.clone() });
        }
        ep
    }

    fn post(&self, http: &reqwest::Client, path: &str) -> reqwest::RequestBuilder {
        let req = http.post(format!("{}{}", self.base_url, path));
        match &self.api_key {
            Some(k) => req.bearer_auth(k),
            None => req,
        }
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .build()
        .expect("构建 HTTP 客户端失败")
}

/// 从错误响应体里取出可读的错误信息
async fn error_text(resp: reqwest::Response) -> String {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(300).collect());
    format!("HTTP {}: {}", status.as_u16(), msg)
}

fn request_body(ep: &Endpoint, messages: &[ChatMessage], temperature: Option<f64>, stream: bool, tools: Option<&Value>) -> Value {
    request_body_with(ep, messages, temperature, stream, tools, true)
}

fn request_body_with(
    ep: &Endpoint,
    messages: &[ChatMessage],
    temperature: Option<f64>,
    stream: bool,
    tools: Option<&Value>,
    with_reasoning: bool,
) -> Value {
    let mut body = json!({ "model": ep.model, "messages": messages, "stream": stream });
    if with_reasoning {
        if let Some(params) = ep.reasoning.as_ref().and_then(reasoning_params) {
            if let Some(obj) = body.as_object_mut() {
                obj.extend(params);
            }
        }
    }
    if let Some(t) = temperature {
        body["temperature"] = json!(t);
    }
    if let Some(tools) = tools {
        body["tools"] = tools.clone();
    }
    body
}

/// 把流式返回的 tool_calls 片段按 index 拼起来
fn merge_tool_delta(calls: &mut Vec<ToolCall>, delta: &Value) {
    let Some(arr) = delta.as_array() else { return };
    for d in arr {
        let idx = d.get("index").and_then(Value::as_u64).unwrap_or(calls.len() as u64) as usize;
        while calls.len() <= idx {
            calls.push(ToolCall { kind: "function".into(), ..Default::default() });
        }
        let c = &mut calls[idx];
        if let Some(id) = d.get("id").and_then(Value::as_str) {
            c.id.push_str(id);
        }
        if let Some(name) = d.pointer("/function/name").and_then(Value::as_str) {
            c.function.name.push_str(name);
        }
        if let Some(args) = d.pointer("/function/arguments").and_then(Value::as_str) {
            c.function.arguments.push_str(args);
        }
    }
}

/// 丢掉没有函数名的残缺调用；缺 id 的补一个（有的服务只在第一个片段给 id，有的根本不给）
fn finish(mut out: StreamOut) -> StreamOut {
    out.tool_calls.retain(|c| !c.function.name.is_empty());
    for (i, c) in out.tool_calls.iter_mut().enumerate() {
        if c.id.is_empty() {
            c.id = format!("call_{i}");
        }
    }
    out
}

pub async fn wait_cancel(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// 流式对话。每收到一段文字就调用 on_delta；cancel 置位后尽快返回已收到的内容。
pub async fn chat_stream(
    ep: &Endpoint,
    messages: &[ChatMessage],
    temperature: Option<f64>,
    tools: Option<&Value>,
    cancel: &AtomicBool,
    mut on_delta: impl FnMut(&str),
) -> Result<StreamOut, String> {
    let has_reasoning = ep.reasoning.as_ref().and_then(reasoning_params).is_some();
    let mut out = StreamOut::default();
    let mut with_reasoning = has_reasoning;
    let resp = loop {
        let body = request_body_with(ep, messages, temperature, true, tools, with_reasoning);
        let send = ep.post(&http(), "/chat/completions").json(&body).send();
        // 模型迟迟不返回响应头时也要能停下
        let resp = tokio::select! {
            r = send => r.map_err(|e| format!("请求失败: {e}"))?,
            _ = wait_cancel(cancel) => return Ok(StreamOut::default()),
        };
        let code = resp.status().as_u16();
        if resp.status().is_success() {
            break resp;
        }
        // 兼容：不支持推理参数的服务会报 400/422，去掉参数重发一次，不影响正常对话
        if with_reasoning && (code == 400 || code == 422) {
            let first = error_text(resp).await;
            eprintln!("[llm] 服务不接受推理参数，去掉后重试：{first}");
            with_reasoning = false;
            out.reasoning_dropped = true;
            continue;
        }
        return Err(error_text(resp).await);
    };

    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        // 定时醒来检查取消标志，模型迟迟不出字时也能停下
        let next = match tokio::time::timeout(Duration::from_millis(200), stream.next()).await {
            Err(_) => continue,
            Ok(None) => break,
            Ok(Some(chunk)) => chunk.map_err(|e| format!("读取响应失败: {e}"))?,
        };
        buf.extend_from_slice(&next);
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let Some(data) = line.trim().strip_prefix("data:") else { continue };
            let data = data.trim();
            if data == "[DONE]" {
                return Ok(finish(out));
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
            if let Some(err) = v.pointer("/error/message").and_then(Value::as_str) {
                return Err(err.to_string());
            }
            if let Some(delta) = v.pointer("/choices/0/delta/content").and_then(Value::as_str) {
                if !delta.is_empty() {
                    out.content.push_str(delta);
                    on_delta(delta);
                }
            }
            if let Some(tc) = v.pointer("/choices/0/delta/tool_calls") {
                merge_tool_delta(&mut out.tool_calls, tc);
            }
            if let Some(r) = v.pointer("/choices/0/delta/reasoning_content").and_then(Value::as_str) {
                out.reasoning_content.push_str(r);
            }
        }
    }
    Ok(finish(out))
}

/// 一次性对话，给辅助任务（总结、记忆、看图转述）用
pub async fn chat_once(ep: &Endpoint, messages: &[ChatMessage], temperature: Option<f64>) -> Result<String, String> {
    let resp = ep
        .post(&http(), "/chat/completions")
        .timeout(Duration::from_secs(180))
        .json(&request_body(ep, messages, temperature, false, None))
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(error_text(resp).await);
    }
    let v: Value = resp.json().await.map_err(|e| format!("解析响应失败: {e}"))?;
    v.pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "响应里没有 choices[0].message.content".into())
}

/// OpenAI 兼容的 /embeddings：每条文本一个向量，顺序和输入一致
pub async fn embed(ep: &Endpoint, inputs: &[String]) -> Result<Vec<Vec<f32>>, String> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    let resp = ep
        .post(&http(), "/embeddings")
        .timeout(Duration::from_secs(30))
        .json(&serde_json::json!({ "model": ep.model, "input": inputs }))
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(error_text(resp).await);
    }
    let v: Value = resp.json().await.map_err(|e| format!("解析响应失败: {e}"))?;
    parse_embeddings(&v, inputs.len())
}

fn parse_embeddings(v: &Value, n: usize) -> Result<Vec<Vec<f32>>, String> {
    let data = v.get("data").and_then(Value::as_array).ok_or("响应里没有 data")?;
    let mut out: Vec<Option<Vec<f32>>> = vec![None; n];
    for (i, item) in data.iter().enumerate() {
        // 按 index 放回原位，没有 index 就按顺序
        let idx = item.get("index").and_then(Value::as_u64).map(|x| x as usize).unwrap_or(i);
        let vec: Vec<f32> = item
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or("响应里的 embedding 不是数组")?
            .iter()
            .filter_map(|x| x.as_f64().map(|f| f as f32))
            .collect();
        if idx < n && !vec.is_empty() {
            out[idx] = Some(vec);
        }
    }
    out.into_iter().collect::<Option<Vec<_>>>().ok_or_else(|| "返回的向量条数不对".into())
}

/// 设置页的"测试连接"。先查 /models（不花钱，能验证地址和 Key，顺便核对模型名）；
/// 服务不提供这个接口时，对话模型退回发一条极短的消息，生图模型就只能实际画一张才能验证。
pub async fn test_connection(ep: &Endpoint, chat_capable: bool) -> Result<String, String> {
    let mut req = http().get(format!("{}/models", ep.base_url)).timeout(Duration::from_secs(20));
    if let Some(k) = &ep.api_key {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await.map_err(|e| format!("连不上 {}：{e}", ep.base_url))?;
    let status = resp.status();
    if status.is_success() {
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        let ids: Vec<&str> = v
            .get("data")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|m| m.get("id").and_then(Value::as_str)).collect())
            .unwrap_or_default();
        return Ok(if ids.is_empty() || ids.contains(&ep.model.as_str()) {
            format!("连接成功，Key 有效{}", if ids.is_empty() { "" } else { "，模型名正确" })
        } else {
            format!("连接成功，Key 有效；但模型列表里没有「{}」，请确认模型名（有的服务列表不全，能正常用就不用管）", ep.model)
        });
    }
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(error_text(resp).await);
    }
    if !chat_capable {
        return Err(format!(
            "这个服务没有模型列表接口（HTTP {}），无法免费验证。生图模型可以直接用 /draw 画一张试试",
            status.as_u16()
        ));
    }
    let reply = chat_once(ep, &[ChatMessage::text("user", "只回复两个字：在的")], Some(0.0)).await?;
    Ok(format!("连接成功，模型回复：{}", reply.chars().take(40).collect::<String>()))
}

/// 粗略估算 token 数：ASCII 约 4 字符一个，其余（中文等）约 1 字一个
pub fn estimate_tokens(s: &str) -> usize {
    let (ascii, other) = s.chars().fold((0usize, 0usize), |(a, o), c| if c.is_ascii() { (a + 1, o) } else { (a, o + 1) });
    ascii / 4 + other + 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_serializes_like_openai() {
        let m = ChatMessage::parts(
            "user",
            vec![
                Part::Text { text: "看图".into() },
                Part::ImageUrl { image_url: ImageUrl { url: "data:image/png;base64,AA".into() } },
            ],
        );
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["content"][0]["type"], "text");
        assert_eq!(v["content"][1]["type"], "image_url");
        assert_eq!(v["content"][1]["image_url"]["url"], "data:image/png;base64,AA");
        assert_eq!(serde_json::to_value(ChatMessage::text("system", "x")).unwrap()["content"], "x");
    }

    #[test]
    fn tool_call_fragments_merge() {
        let mut calls = Vec::new();
        merge_tool_delta(&mut calls, &json!([{ "index": 0, "id": "c1", "function": { "name": "read_", "arguments": "{\"pa" } }]));
        merge_tool_delta(&mut calls, &json!([{ "index": 0, "function": { "name": "file", "arguments": "th\":1}" } }]));
        merge_tool_delta(&mut calls, &json!([{ "index": 1, "function": { "name": "edit_file", "arguments": "{}" } }]));
        let out = finish(StreamOut { tool_calls: calls, ..Default::default() });
        assert_eq!(out.tool_calls[0].function.name, "read_file");
        assert_eq!(out.tool_calls[0].function.arguments, "{\"path\":1}");
        assert_eq!(out.tool_calls[1].id, "call_1");
        let m = serde_json::to_value(ChatMessage::assistant_tool_calls("", out.tool_calls)).unwrap();
        assert!(m["content"].is_null());
        assert_eq!(m["tool_calls"][0]["type"], "function");
        assert_eq!(serde_json::to_value(ChatMessage::tool_result("c1", "ok")).unwrap()["tool_call_id"], "c1");
    }

    #[test]
    fn reasoning_params_per_style() {
        let r = |e: &str, s: &str| reasoning_params(&Reasoning { effort: e.into(), style: s.into() }).map(Value::Object);
        assert_eq!(r("high", "openai"), Some(json!({ "reasoning_effort": "high" })));
        assert_eq!(r("off", "openai"), Some(json!({ "reasoning_effort": "none" })));
        assert_eq!(r("low", "openrouter"), Some(json!({ "reasoning": { "effort": "low" } })));
        assert_eq!(r("off", "qwen"), Some(json!({ "enable_thinking": false })));
        assert_eq!(r("off", "deepseek"), Some(json!({ "thinking": { "type": "disabled" } })));
        assert_eq!(r("low", "deepseek"), Some(json!({ "thinking": { "type": "enabled" }, "reasoning_effort": "low" })));
        assert_eq!(r("medium", "deepseek"), Some(json!({ "thinking": { "type": "enabled" }, "reasoning_effort": "high" })));
        assert_eq!(r("high", "deepseek"), Some(json!({ "thinking": { "type": "enabled" }, "reasoning_effort": "max" })));
        assert_eq!(r("medium", "qwen"), Some(json!({ "enable_thinking": true, "thinking_budget": 4096 })));
        // 不设置、乱填都不发参数
        assert_eq!(r("", "openai"), None);
        assert_eq!(r("max", "openai"), None);
        // 不认识的格式按 OpenAI 处理
        assert_eq!(r("low", "whatever"), Some(json!({ "reasoning_effort": "low" })));
    }

    #[test]
    fn reasoning_only_in_chat_requests() {
        let p = Provider { id: "x".into(), model: "m".into(), reasoning_effort: "high".into(), ..Default::default() };
        let msgs = [ChatMessage::text("user", "hi")];
        let chat = Endpoint::for_chat(&p, false);
        assert_eq!(request_body(&chat, &msgs, None, true, None)["reasoning_effort"], "high");
        assert!(request_body(&Endpoint::for_chat(&p, true), &msgs, None, true, None).get("reasoning_effort").is_none());
        assert!(request_body(&Endpoint::from_provider(&p), &msgs, None, false, None).get("reasoning_effort").is_none());
        let m = serde_json::to_value(ChatMessage::assistant_tool_calls("", vec![]).with_reasoning("想一想")).unwrap();
        assert_eq!(m["reasoning_content"], "想一想");
        assert!(serde_json::to_value(ChatMessage::text("user", "x")).unwrap().get("reasoning_content").is_none());
    }

    #[test]
    fn token_estimate_is_reasonable() {
        assert!(estimate_tokens("你好世界") >= 4);
        assert!(estimate_tokens(&"a".repeat(400)) < 120);
    }

    #[test]
    fn embeddings_follow_index() {
        let v = serde_json::json!({ "data": [
            { "index": 1, "embedding": [0.5, 0.25] },
            { "index": 0, "embedding": [1.0, 2.0] }
        ]});
        assert_eq!(parse_embeddings(&v, 2).unwrap(), vec![vec![1.0, 2.0], vec![0.5, 0.25]]);
        assert!(parse_embeddings(&v, 3).is_err());
        assert!(parse_embeddings(&serde_json::json!({}), 1).is_err());
    }
}
