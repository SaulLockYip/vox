//! Anthropic 兼容 Messages 端点：`POST {base_url}/v1/messages`
//!
//! 针对 MiniMax 做了两处适配：
//! - `thinking` 块只取 `text_delta`，忽略 `thinking_delta`
//! - `thinking.type=disabled` 对 `MiniMax-M3.1-Flash` 会返回 400，故默认用 `adaptive`

use std::time::Duration;

use vox_core::config::{LlmConfig, LlmOverrides, ThinkingMode};
use vox_core::error::{Error, Result};

use crate::sse::SseDecoder;

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub system: Option<String>,
    pub user: String,
    /// 本次请求的参数覆盖（模型 / thinking / effort）。
    ///
    /// 听写/翻译用它来「关思考求快」，Ask Anything 用它来「开思考但强度调低」。
    pub overrides: Option<LlmOverrides>,
}

impl LlmRequest {
    pub fn new(user: impl Into<String>) -> Self {
        Self {
            system: None,
            user: user.into(),
            overrides: None,
        }
    }

    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn with_overrides(mut self, overrides: LlmOverrides) -> Self {
        self.overrides = Some(overrides);
        self
    }
}

/// 工具定义（Anthropic Messages API 的 `tools` 格式）。
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

impl ToolDef {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }
}

/// 工具调用的最大轮数（防止模型无限循环调工具）。
const MAX_TOOL_ROUNDS: usize = 6;

pub struct LlmClient {
    http: reqwest::Client,
    cfg: LlmConfig,
}

impl LlmClient {
    pub fn new(cfg: LlmConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs.max(1)))
            .build()
            .map_err(|e| Error::Http(e.to_string()))?;
        Ok(Self { http, cfg })
    }

    pub fn config(&self) -> &LlmConfig {
        &self.cfg
    }

    fn body(&self, req: &LlmRequest, stream: bool) -> serde_json::Value {
        let messages = vec![serde_json::json!({
            "role": "user",
            "content": [{ "type": "text", "text": req.user }]
        })];
        self.body_with(req, stream, &messages, None)
    }

    /// 组装请求体（可带多轮 messages 和工具定义）。
    fn body_with(
        &self,
        req: &LlmRequest,
        stream: bool,
        messages: &[serde_json::Value],
        tools: Option<&[ToolDef]>,
    ) -> serde_json::Value {
        // 先算出生效的 model / thinking / effort（请求级覆盖优先）
        let ov = req.overrides.clone().unwrap_or_default();
        let model = ov
            .model
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| self.cfg.model.clone());
        let thinking = ov.thinking.unwrap_or(self.cfg.thinking);
        let effort = ov
            .effort
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| self.cfg.effort.clone());

        let mut body = serde_json::json!({
            "model": model,
            "max_tokens": self.cfg.max_tokens,
            "messages": messages,
            "stream": stream,
        });

        if let Some(system) = &req.system {
            body["system"] = serde_json::Value::String(system.clone());
        }
        match thinking {
            ThinkingMode::Adaptive => {
                body["thinking"] = serde_json::json!({ "type": "adaptive" });
            }
            ThinkingMode::Disabled => {
                body["thinking"] = serde_json::json!({ "type": "disabled" });
            }
            ThinkingMode::Omit => {}
        }
        if !effort.trim().is_empty() {
            body["output_config"] = serde_json::json!({ "effort": effort });
        }
        // service_tier=priority 会插队处理（更快，1.5x 价格）
        let tier = self.cfg.service_tier.trim();
        if !tier.is_empty() && tier != "standard" {
            body["service_tier"] = serde_json::Value::String(tier.to_string());
        }
        if let Some(tools) = tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::Value::Array(
                    tools
                        .iter()
                        .map(|t| {
                            serde_json::json!({
                                "name": t.name,
                                "description": t.description,
                                "input_schema": t.input_schema,
                            })
                        })
                        .collect(),
                );
            }
        }
        body
    }

    /// 发一次请求并解析 JSON 响应。
    async fn send(&self, body: &serde_json::Value) -> Result<serde_json::Value> {
        let key = self.cfg.api_key()?;
        let resp = self
            .http
            .post(self.cfg.url())
            .bearer_auth(key)
            .header("content-type", "application/json")
            .header("anthropic-version", "2023-06-01")
            .json(body)
            .send()
            .await
            .map_err(|e| Error::Http(format!("LLM 请求失败: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Api {
                status: status.as_u16(),
                body: truncate(&body, 800),
            });
        }

        resp.json()
            .await
            .map_err(|e| Error::Parse(format!("解析 LLM 响应失败: {e}")))
    }

    /// 一次性拿到完整回复。
    pub async fn complete(&self, req: &LlmRequest) -> Result<String> {
        let v = self.send(&self.body(req, false)).await?;
        Ok(extract_text(&v))
    }

    /// **带工具的完整对话**：模型要调工具就调，我们执行后把结果喂回去，
    /// 直到模型给出最终文本。
    ///
    /// `executor` 收到 (工具名, 输入 JSON)，返回工具输出（字符串）。
    pub async fn complete_with_tools(
        &self,
        req: &LlmRequest,
        tools: &[ToolDef],
        executor: &(dyn Fn(&str, &serde_json::Value) -> String + Send + Sync),
    ) -> Result<String> {
        let mut messages = vec![serde_json::json!({
            "role": "user",
            "content": [{ "type": "text", "text": req.user }]
        })];
        let mut text = String::new();

        for _ in 0..MAX_TOOL_ROUNDS {
            let body = self.body_with(req, false, &messages, Some(tools));
            let v = self.send(&body).await?;
            text = extract_text(&v);

            let content = v
                .get("content")
                .cloned()
                .unwrap_or_else(|| serde_json::Value::Array(vec![]));
            let tool_uses: Vec<serde_json::Value> = content
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();

            if tool_uses.is_empty() {
                return Ok(text);
            }

            // 原样回带 assistant 的 content（thinking / text / tool_use 都要带回去）
            messages.push(serde_json::json!({ "role": "assistant", "content": content }));

            let results: Vec<serde_json::Value> = tool_uses
                .iter()
                .map(|tu| {
                    let id = tu.get("id").and_then(|x| x.as_str()).unwrap_or_default();
                    let name = tu.get("name").and_then(|x| x.as_str()).unwrap_or_default();
                    let input = tu.get("input").cloned().unwrap_or(serde_json::Value::Null);
                    let out = executor(name, &input);
                    serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": out,
                    })
                })
                .collect();
            messages.push(serde_json::json!({ "role": "user", "content": results }));
        }

        Ok(text)
    }

    /// 流式获取回复，`on_delta` 每次收到增量文本被调用。
    pub async fn complete_stream<F>(&self, req: &LlmRequest, mut on_delta: F) -> Result<String>
    where
        F: FnMut(&str),
    {
        let key = self.cfg.api_key()?;
        let resp = self
            .http
            .post(self.cfg.url())
            .bearer_auth(key)
            .header("content-type", "application/json")
            .header("anthropic-version", "2023-06-01")
            .json(&self.body(req, true))
            .send()
            .await
            .map_err(|e| Error::Http(format!("LLM 请求失败: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Api {
                status: status.as_u16(),
                body: truncate(&body, 800),
            });
        }

        let mut decoder = SseDecoder::new();
        let mut full = String::new();
        let mut stream = resp.bytes_stream();
        use futures_util::StreamExt;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| Error::Http(format!("读取 LLM 流失败: {e}")))?;
            let text_chunk = String::from_utf8_lossy(&chunk).to_string();
            for payload in decoder.push(&text_chunk) {
                Self::apply_event(&payload, &mut full, &mut on_delta);
            }
        }
        for payload in decoder.flush() {
            Self::apply_event(&payload, &mut full, &mut on_delta);
        }
        Ok(full)
    }

    fn apply_event<F>(payload: &str, full: &mut String, on_delta: &mut F)
    where
        F: FnMut(&str),
    {
        let v: serde_json::Value = match serde_json::from_str(payload) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(payload, error = %e, "无法解析 LLM 流事件");
                return;
            }
        };

        if v.get("type").and_then(|t| t.as_str()) == Some("error") {
            let msg = v
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("未知流错误");
            tracing::error!(error = msg, "LLM 流返回错误事件");
            return;
        }

        if v.get("type").and_then(|t| t.as_str()) != Some("content_block_delta") {
            return;
        }
        let delta = v.get("delta").unwrap_or(&serde_json::Value::Null);
        if delta.get("type").and_then(|t| t.as_str()) == Some("text_delta") {
            if let Some(t) = delta.get("text").and_then(|t| t.as_str()) {
                full.push_str(t);
                on_delta(t);
            }
        }
    }
}

/// 从非流式响应里取出所有 text 内容块。
fn extract_text(v: &serde_json::Value) -> String {
    let mut out = String::new();
    if let Some(content) = v.get("content").and_then(|c| c.as_array()) {
        for block in content {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    out.push_str(t);
                }
            }
        }
    }
    if out.is_empty() {
        if let Some(t) = v.get("content").and_then(|c| c.as_str()) {
            out.push_str(t);
        }
    }
    out
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> LlmClient {
        LlmClient::new(LlmConfig::default()).unwrap()
    }

    #[test]
    fn body_contains_model_and_stream() {
        let c = client();
        let body = c.body(&LlmRequest::new("hi"), true);
        assert_eq!(body["model"], "MiniMax-M3");
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hi");
        // 通用档（润色）：关思考、不带 output_config
        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body.get("output_config").is_none());
        // priority 会插队
        assert_eq!(body["service_tier"], "priority");
    }

    #[test]
    fn overrides_change_model_thinking_effort() {
        let c = client();
        let req = LlmRequest::new("hi").with_overrides(LlmOverrides {
            model: Some("MiniMax-M3".into()),
            thinking: Some(ThinkingMode::Adaptive),
            effort: Some("low".into()),
        });
        let body = c.body(&req, false);
        assert_eq!(body["model"], "MiniMax-M3");
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "low");
    }

    #[test]
    fn body_includes_system_when_present() {
        let c = client();
        let body = c.body(&LlmRequest::new("hi").with_system("sys"), false);
        assert_eq!(body["system"], "sys");
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn extracts_only_text_blocks() {
        let v = serde_json::json!({
            "content": [
                {"type": "thinking", "thinking": "内部思考"},
                {"type": "text", "text": "最终答案"}
            ]
        });
        assert_eq!(extract_text(&v), "最终答案");
    }

    #[test]
    fn stream_skips_thinking_deltas() {
        let mut full = String::new();
        let mut seen = Vec::new();
        let mut on_delta = |t: &str| seen.push(t.to_string());

        LlmClient::apply_event(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"思考"}}"#,
            &mut full,
            &mut on_delta,
        );
        LlmClient::apply_event(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"你好"}}"#,
            &mut full,
            &mut on_delta,
        );
        LlmClient::apply_event(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"世界"}}"#,
            &mut full,
            &mut on_delta,
        );

        assert_eq!(full, "你好世界");
        assert_eq!(seen, vec!["你好", "世界"]);
    }

    #[test]
    fn url_join() {
        let cfg = LlmConfig::default();
        assert_eq!(cfg.url(), "https://api.minimax.cn/anthropic/v1/messages");
    }
}
