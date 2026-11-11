//! MiniMax 语音识别：`POST {base_url}/v1/speech_to_text`
//!
//! - `multipart/form-data`：`model` / `file` / `response_format` / `timestamp_level` / `stream`
//! - 可选请求头 `language`（空 = 混合语言识别）
//! - 鉴权 `Authorization: Bearer <API_KEY>`

use std::path::Path;
use std::time::Duration;

use vox_core::config::SttConfig;
use vox_core::error::{Error, Result};

use crate::sse::SseDecoder;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Segment {
    pub id: i64,
    pub start: f64,
    pub end: f64,
    #[serde(default)]
    pub speaker: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AsrResult {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub n_speakers: Option<i64>,
    #[serde(default)]
    pub segments: Vec<Segment>,
    #[serde(default)]
    pub trace_id: Option<String>,
}

impl AsrResult {
    pub fn transcript(&self) -> &str {
        &self.text
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
struct AsrJson {
    #[serde(default)]
    text: String,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    n_speakers: Option<i64>,
    #[serde(default)]
    segments: Vec<Segment>,
    #[serde(default)]
    trace_id: Option<String>,
}

/// 流式增量事件：`data: {"index":0,"delta":"实际上","finish":false}`
#[derive(Debug, Clone, serde::Deserialize)]
struct AsrStreamEvent {
    /// 事件序号（协议要求按 index 顺序拼接 delta）
    #[allow(dead_code)]
    #[serde(default)]
    index: i64,
    #[serde(default)]
    delta: String,
    #[serde(default)]
    finish: bool,
    #[serde(default)]
    duration: Option<f64>,
}

pub struct AsrClient {
    http: reqwest::Client,
    cfg: SttConfig,
}

impl AsrClient {
    pub fn new(cfg: SttConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs.max(1)))
            .build()
            .map_err(|e| Error::Http(e.to_string()))?;
        Ok(Self { http, cfg })
    }

    pub fn config(&self) -> &SttConfig {
        &self.cfg
    }

    /// 转写一个音频文件。
    pub async fn transcribe_file(&self, path: &Path) -> Result<AsrResult> {
        let data = std::fs::read(path)?;
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "audio.wav".into());
        let mime = mime_for(&name);
        self.transcribe_bytes(&name, mime, data).await
    }

    /// 转写内存中的音频字节。
    pub async fn transcribe_bytes(
        &self,
        filename: &str,
        mime: &str,
        data: Vec<u8>,
    ) -> Result<AsrResult> {
        let key = self.cfg.api_key()?;
        let part = reqwest::multipart::Part::bytes(data)
            .file_name(filename.to_string())
            .mime_str(mime)
            .map_err(|e| Error::Other(format!("构造 multipart 失败: {e}")))?;

        let mut form = reqwest::multipart::Form::new()
            .text("model", self.cfg.model.clone())
            .part("file", part)
            .text("response_format", self.cfg.response_format.clone());
        if self.cfg.stream {
            form = form.text("stream", "true");
        }

        let mut req = self.http.post(self.cfg.url()).bearer_auth(key).multipart(form);
        let lang = self.cfg.language.trim();
        if !lang.is_empty() {
            req = req.header("language", lang);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| Error::Http(format!("ASR 请求失败: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Api {
                status: status.as_u16(),
                body: truncate(&body, 600),
            });
        }

        if self.cfg.stream {
            self.read_stream(resp).await
        } else {
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| Error::Http(format!("读取 ASR 响应失败: {e}")))?;
            let parsed: AsrJson = serde_json::from_slice(&bytes)
                .map_err(|e| Error::Parse(format!("解析 ASR 响应失败: {e}")))?;
            Ok(AsrResult {
                text: parsed.text,
                duration: parsed.duration,
                n_speakers: parsed.n_speakers,
                segments: parsed.segments,
                trace_id: parsed.trace_id,
            })
        }
    }

    async fn read_stream(&self, resp: reqwest::Response) -> Result<AsrResult> {
        let mut decoder = SseDecoder::new();
        let mut text = String::new();
        let mut duration = None;
        let mut stream = resp.bytes_stream();
        use futures_util::StreamExt;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| Error::Http(format!("读取 ASR 流失败: {e}")))?;
            let text_chunk = String::from_utf8_lossy(&chunk).to_string();
            for payload in decoder.push(&text_chunk) {
                if let Some(ev) = Self::apply_stream_event(&payload, &mut text) {
                    duration = Some(ev);
                }
            }
        }
        for payload in decoder.flush() {
            if let Some(ev) = Self::apply_stream_event(&payload, &mut text) {
                duration = Some(ev);
            }
        }

        Ok(AsrResult {
            text,
            duration,
            ..Default::default()
        })
    }

    fn apply_stream_event(payload: &str, text: &mut String) -> Option<f64> {
        let ev: AsrStreamEvent = match serde_json::from_str(payload) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(payload, error = %e, "无法解析 ASR 流事件");
                return None;
            }
        };
        text.push_str(&ev.delta);
        if ev.finish {
            return ev.duration;
        }
        None
    }
}

fn mime_for(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".wav") {
        "audio/wav"
    } else if lower.ends_with(".mp3") {
        "audio/mpeg"
    } else if lower.ends_with(".flac") {
        "audio/flac"
    } else if lower.ends_with(".m4a") || lower.ends_with(".alac") {
        "audio/mp4"
    } else if lower.ends_with(".aac") {
        "audio/aac"
    } else if lower.ends_with(".ogg") {
        "audio/ogg"
    } else if lower.ends_with(".opus") {
        "audio/opus"
    } else if lower.ends_with(".aiff") {
        "audio/aiff"
    } else {
        "application/octet-stream"
    }
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

    #[test]
    fn mime_mapping() {
        assert_eq!(mime_for("a.wav"), "audio/wav");
        assert_eq!(mime_for("a.MP3"), "audio/mpeg");
        assert_eq!(mime_for("a.bin"), "application/octet-stream");
    }

    #[test]
    fn url_join() {
        let cfg = SttConfig::default();
        assert_eq!(cfg.url(), "https://api.minimax.cn/v1/speech_to_text");
    }

    #[test]
    fn parses_json_response() {
        let raw = r#"{"text":"你好","duration":1.5,"trace_id":"abc"}"#;
        let p: AsrJson = serde_json::from_str(raw).unwrap();
        assert_eq!(p.text, "你好");
        assert_eq!(p.duration, Some(1.5));
    }

    #[test]
    fn parses_verbose_response() {
        let raw = r#"{"text":"a b","duration":2.0,"n_speakers":2,
            "segments":[{"id":0,"start":0.1,"end":1.0,"speaker":"S1","text":"a"}]}"#;
        let p: AsrJson = serde_json::from_str(raw).unwrap();
        assert_eq!(p.n_speakers, Some(2));
        assert_eq!(p.segments.len(), 1);
        assert_eq!(p.segments[0].speaker.as_deref(), Some("S1"));
    }

    #[test]
    fn accumulates_stream_deltas() {
        let mut text = String::new();
        let mut dur = None;
        for p in [
            r#"{"index":0,"delta":"实际上","finish":false}"#,
            r#"{"index":1,"delta":"还是","finish":false}"#,
            r#"{"index":2,"delta":"","finish":true,"duration":26.325}"#,
        ] {
            if let Some(d) = AsrClient::apply_stream_event(p, &mut text) {
                dur = Some(d);
            }
        }
        assert_eq!(text, "实际上还是");
        assert_eq!(dur, Some(26.325));
    }
}
