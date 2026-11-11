//! 极简 SSE 解码：把字节流切成 `data:` 负载。

#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: String,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一段文本，返回本次可解析出的 `data:` 负载（已去掉 `data:` 前缀）。
    pub fn push(&mut self, chunk: &str) -> Vec<String> {
        self.buffer.push_str(chunk);
        let mut out = Vec::new();
        loop {
            let Some(idx) = self.buffer.find('\n') else {
                break;
            };
            let line: String = self.buffer[..idx].to_string();
            self.buffer.drain(..idx + 1);
            let line = line.trim_end_matches('\r');
            if let Some(rest) = line.strip_prefix("data:") {
                let payload = rest.trim();
                if payload.is_empty() {
                    continue;
                }
                if payload == "[DONE]" {
                    continue;
                }
                out.push(payload.to_string());
            }
        }
        out
    }

    /// 冲刷尾部未以换行结束的内容（流结束时调用）。
    pub fn flush(&mut self) -> Vec<String> {
        let rest = std::mem::take(&mut self.buffer);
        let mut out = Vec::new();
        for line in rest.split('\n') {
            let line = line.trim_end_matches('\r');
            if let Some(rest) = line.strip_prefix("data:") {
                let payload = rest.trim();
                if !payload.is_empty() && payload != "[DONE]" {
                    out.push(payload.to_string());
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_data_lines_across_chunks() {
        let mut d = SseDecoder::new();
        assert!(d.push("data: {\"a\":").is_empty());
        let got = d.push("1}\ndata: {\"b\":2}\n");
        assert_eq!(got, vec!["{\"a\":1}", "{\"b\":2}"]);
    }

    #[test]
    fn ignores_event_lines_and_done() {
        let mut d = SseDecoder::new();
        let got = d.push("event: ping\ndata: [DONE]\ndata: x\n\n");
        assert_eq!(got, vec!["x"]);
    }

    #[test]
    fn flush_returns_tail() {
        let mut d = SseDecoder::new();
        assert!(d.push("data: tail").is_empty());
        assert_eq!(d.flush(), vec!["tail"]);
    }
}
