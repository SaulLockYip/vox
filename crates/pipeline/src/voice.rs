//! 三种模式的编排：ASR → prompt → LLM → 词典替换。

use std::path::Path;

use vox_core::config::Config;
use vox_core::error::Result;
use vox_core::mode::{Delivery, Mode};
use vox_providers::asr::AsrClient;
use vox_providers::llm::{LlmClient, LlmRequest};

use crate::dictionary::{apply_dictionary, DictionaryEntry};
use crate::template::{render, PromptContext};

/// 一次处理的产出。
#[derive(Debug, Clone)]
pub struct PipelineOutcome {
    /// ASR 原始转写
    pub raw_text: String,
    /// 经过 LLM 处理与词典替换后的最终文本
    pub final_text: String,
    /// 最终文本是否与原始转写不同
    pub refined: bool,
    /// 模型要求的「外部动作」（打开某个网址 / 应用）。
    pub action: Option<String>,
    /// 模型决定的**投递方式**：结果显示在面板，还是写进光标。
    ///
    /// 听写/翻译恒为 [`Delivery::Insert`]；Ask 由模型自己判断。
    pub delivery: Delivery,
}

/// 从模型输出里抽取「外部动作」标记，并把标记本身从正文里去掉。
///
/// 格式（一行一个）：
/// ```text
/// [[open: https://www.bilibili.com]]
/// [[open: Bilibili]]          ← 也可以是应用名
/// ```
///
/// 模型判断用户想打开某个东西时，除了给答案，再给一行动作标记：
/// 模型判断用户想打开某个东西时，除了给答案，再给一行动作标记。
pub fn extract_action(text: &str) -> (String, Option<String>) {
    let mut action: Option<String> = None;
    let mut kept: Vec<&str> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("[[open:") {
            if let Some(inner) = t
                .strip_prefix("[[open:")
                .and_then(|r| r.strip_suffix("]]"))
            {
                let v = inner.trim();
                if !v.is_empty() {
                    action = Some(v.to_string());
                }
                continue; // 这一行是标记，不进正文
            }
        }
        kept.push(line);
    }
    (kept.join("\n").trim_end().to_string(), action)
}

/// 从模型输出里抽取「投递方式」标记，并把标记本身从正文里去掉。
///
/// 格式：
/// ```text
/// [[deliver: reply]]   ← 显示在浮窗面板（默认）
/// [[deliver: insert]]  ← 插入到光标
/// [[deliver: replace]] ← 替换选区
/// ```
///
/// 投递方式由模型自己决定。
/// **模型没给标记时默认 `Reply`** —— 问答类结果应该显示出来，而不是直接塞进光标。
pub fn extract_delivery(text: &str) -> (String, Delivery) {
    let mut delivery = Delivery::default();
    let mut kept: Vec<&str> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("[[deliver:") {
            if let Some(inner) = t
                .strip_prefix("[[deliver:")
                .and_then(|r| r.strip_suffix("]]"))
            {
                if let Some(d) = Delivery::parse(inner.trim()) {
                    delivery = d;
                }
                continue; // 这一行是标记，不进正文
            }
        }
        kept.push(line);
    }
    (kept.join("\n").trim_end().to_string(), delivery)
}

/// 语音流水线。
pub struct VoicePipeline {
    asr: AsrClient,
    llm: LlmClient,
    cfg: Config,
}

impl VoicePipeline {
    pub fn new(cfg: Config) -> Result<Self> {
        let asr = AsrClient::new(cfg.stt.clone())?;
        let llm = LlmClient::new(cfg.llm.clone())?;
        Ok(Self { asr, llm, cfg })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// 只做语音识别。
    pub async fn transcribe(&self, audio_path: &Path) -> Result<String> {
        let r = self.asr.transcribe_file(audio_path).await?;
        Ok(r.text)
    }

    /// 完整流程：识别 → 生成 → 后处理。
    ///
    /// `dictionary` 为空时不会做本地替换；`ctx` 里可带选中文本与应用名。
    pub async fn process(
        &self,
        mode: Mode,
        audio_path: &Path,
        mut ctx: PromptContext,
        dictionary: &[DictionaryEntry],
    ) -> Result<PipelineOutcome> {
        let raw_text = self.transcribe(audio_path).await?;
        if raw_text.trim().is_empty() {
            return Ok(PipelineOutcome {
                raw_text,
                final_text: String::new(),
                refined: false,
                action: None,
                delivery: Delivery::Insert,
            });
        }

        ctx.transcript = raw_text.clone();
        if self.cfg.general.use_dictionary {
            ctx.dictionary_terms = dictionary
                .iter()
                .map(|e| e.term.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
        }

        let template = match mode {
            Mode::Dictation => &self.cfg.prompts.dictation,
            Mode::Translate => &self.cfg.prompts.translate,
            Mode::AskAnything => &self.cfg.prompts.ask_anything,
        };
        let prompt = render(template, &ctx);

        // 按模式套用 LLM 参数：Ask 用「开思考+低强度」，听写/翻译用通用档（关思考求快）
        let overrides = match mode {
            Mode::AskAnything => self.cfg.llm.ask.clone(),
            _ => vox_core::config::LlmOverrides::default(),
        };

        let req = LlmRequest::new(prompt).with_overrides(overrides);

        // Ask Anything 带**本地工具**：模型可以真的去查实时数据（curl 天气接口等），
        // 执行结果回喂给它，再由它给出最终答案。其它模式不加工具（要快）。
        let llm_text = if mode == Mode::AskAnything {
            let perm = crate::tools::ToolPerm::parse(&self.cfg.general.agent_tools);
            let tools = crate::tools::tool_defs(perm);
            if tools.is_empty() {
                self.llm.complete(&req).await
            } else {
                self.llm
                    .complete_with_tools(&req, &tools, &|name, input| {
                        if name != crate::tools::TOOL_RUN_COMMAND {
                            return format!("错误：未知工具 {name}");
                        }
                        let cmd = input
                            .get("command")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        crate::tools::execute(cmd, perm)
                    })
                    .await
            }
        } else {
            self.llm.complete(&req).await
        }
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "LLM 处理失败，回退为原始转写");
            raw_text.clone()
        });

        // 先抽「外部动作」与「投递方式」标记（要在词典替换之前，否则标记会被改写）
        let (llm_text, action) = extract_action(llm_text.trim());
        let (llm_text, delivery) = extract_delivery(&llm_text);
        // 听写/翻译的结果**永远**是写进光标；只有 Ask 由模型自己决定
        let delivery = match mode {
            Mode::AskAnything => delivery,
            _ => Delivery::Insert,
        };

        let final_text = if self.cfg.general.use_dictionary {
            apply_dictionary(llm_text.as_str(), dictionary)
        } else {
            llm_text.to_string()
        };

        let refined = final_text != raw_text;

        Ok(PipelineOutcome {
            raw_text,
            refined,
            final_text,
            action,
            delivery,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_open_marker_as_action() {
        let (text, action) = extract_action(
            "正在为你打开哔哩哔哩。\n[[open: https://www.bilibili.com]]",
        );
        assert_eq!(action.as_deref(), Some("https://www.bilibili.com"));
        assert!(!text.contains("[[open:"));
        assert!(text.contains("正在为你打开"));
    }

    #[test]
    fn no_marker_means_no_action() {
        let (text, action) = extract_action("普通回答，没有任何动作。");
        assert!(action.is_none());
        assert_eq!(text, "普通回答，没有任何动作。");
    }

    /// 模型没给投递标记 → 默认显示在面板（**不能**默认插进光标）。
    #[test]
    fn delivery_defaults_to_reply() {
        let (text, d) = extract_delivery("这是一个解释性的回答。");
        assert_eq!(d, Delivery::Reply);
        assert_eq!(text, "这是一个解释性的回答。");
    }

    #[test]
    fn delivery_insert_marker_is_extracted() {
        let (text, d) = extract_delivery("好的，这段话可以直接用。\n[[deliver: insert]]");
        assert_eq!(d, Delivery::Insert);
        assert!(!text.contains("[[deliver:"));
        assert!(text.contains("可以直接用"));
    }

    #[test]
    fn delivery_replace_marker_is_extracted() {
        let (text, d) = extract_delivery("改写后的句子。\n[[deliver: replace]]");
        assert_eq!(d, Delivery::Replace);
        assert_eq!(text, "改写后的句子。");
    }

    /// 认不出的标记值不能让正文被吃掉，也不能改变投递方式。
    #[test]
    fn unknown_delivery_marker_is_ignored() {
        let (text, d) = extract_delivery("正文。\n[[deliver: teleport]]");
        assert_eq!(d, Delivery::Reply);
        assert!(text.contains("正文"));
    }

    /// 两个标记（open + deliver）要能被分别抽走。
    #[test]
    fn action_and_delivery_coexist() {
        let raw = "正在打开。\n[[deliver: reply]]\n[[open: https://www.bilibili.com]]";
        let (t1, action) = extract_action(raw);
        let (t2, d) = extract_delivery(&t1);
        assert_eq!(action.as_deref(), Some("https://www.bilibili.com"));
        assert_eq!(d, Delivery::Reply);
        assert!(!t2.contains("[[open:"));
        assert!(!t2.contains("[[deliver:"));
    }

    #[test]
    fn marker_only_line_is_removed() {
        let (text, action) = extract_action("回答。\n\n[[open: Bilibili]]\n");
        assert_eq!(action.as_deref(), Some("Bilibili"));
        assert_eq!(text, "回答。");
    }

    #[test]
    fn mode_selects_template() {
        let cfg = Config::default();
        assert_eq!(cfg.prompts.dictation, vox_core::config::DEFAULT_DICTATION_PROMPT);
        assert_eq!(cfg.prompts.translate, vox_core::config::DEFAULT_TRANSLATE_PROMPT);
        assert_eq!(cfg.prompts.ask_anything, vox_core::config::DEFAULT_ASK_PROMPT);
    }

    #[test]
    fn translate_context_carries_target_language() {
        let ctx = PromptContext {
            transcript: "hello".into(),
            target_language: Some("en-US".into()),
            ..Default::default()
        };
        let out = render(&Config::default().prompts.translate, &ctx);
        assert!(out.contains("en-US"));
    }
}
