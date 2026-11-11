//! 极简 prompt 模板渲染。
//!
//! 支持两种语法：
//! - `{{name}}` —— 变量替换
//! - `{{#name}}...{{/name}}` —— 条件区段：值为空时整段丢弃

/// 渲染模板所需的上下文。
#[derive(Debug, Clone, Default)]
pub struct PromptContext {
    pub transcript: String,
    pub target_language: Option<String>,
    pub selection: Option<String>,
    pub app_name: Option<String>,
    /// 需要模型正确使用的术语/专有名词。
    pub dictionary_terms: Vec<String>,
    /// 光标**之前**的文字（问答的上下文）。
    pub context_before: Option<String>,
    /// 光标**之后**的文字。
    pub context_after: Option<String>,
}

impl PromptContext {
    /// 取占位符的值。返回 `String` 而非 `&str`，因为 `dictionary` 需要拼接出来的临时值。
    fn value(&self, name: &str) -> Option<String> {
        match name {
            "transcript" => Some(self.transcript.clone()),
            "target_language" => self.target_language.clone(),
            "selection" => self.selection.clone(),
            "app_name" => self.app_name.clone(),
            "context_before" => self.context_before.clone(),
            "context_after" => self.context_after.clone(),
            "dictionary" => {
                if self.dictionary_terms.is_empty() {
                    None
                } else {
                    Some(self.dictionary_terms.join("、"))
                }
            }
            _ => None,
        }
    }
}

/// 渲染模板。
pub fn render(template: &str, ctx: &PromptContext) -> String {
    let with_sections = render_sections(template, ctx);
    substitute(&with_sections, ctx)
}

/// 处理 `{{#name}}...{{/name}}` 区段。
///
/// 值为空（None 或全是空白）时整段丢弃，否则保留内容并去掉标签。
fn render_sections(input: &str, ctx: &PromptContext) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    loop {
        // 1) 找开标签 "{{#"
        let Some(open) = rest.find("{{#") else {
            out.push_str(rest);
            break;
        };
        // 2) 开标签的结束位置：区段名后面的 "}}"
        let Some(name_end_rel) = rest[open + 3..].find("}}") else {
            out.push_str(rest);
            break;
        };
        let name_end = open + 3 + name_end_rel;
        let name = rest[open + 3..name_end].trim().to_string();
        let content_start = name_end + 2; // 跳过开标签的 "}}"

        // 3) 找对应的闭合标签 "{{/name}}"
        let close_tag = format!("{{{{/{}}}}}", name);
        let Some(close_rel) = rest[content_start..].find(&close_tag) else {
            // 找不到闭合标签，原样输出剩余部分
            out.push_str(rest);
            break;
        };
        let close_start = content_start + close_rel;
        let inner = &rest[content_start..close_start];

        out.push_str(&rest[..open]);
        let present = ctx.value(&name).map(|v| !v.trim().is_empty()).unwrap_or(false);
        if present {
            out.push_str(inner);
        }
        rest = &rest[close_start + close_tag.len()..];
    }
    out
}

/// 处理 `{{name}}` 变量替换。
fn substitute(input: &str, ctx: &PromptContext) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    loop {
        let Some(start) = rest.find("{{") else {
            out.push_str(rest);
            break;
        };
        let Some(end_rel) = rest[start..].find("}}") else {
            out.push_str(rest);
            break;
        };
        let end = start + end_rel;
        let name = rest[start + 2..end].trim();

        out.push_str(&rest[..start]);
        if name.starts_with('#') || name.starts_with('/') {
            // 区段标记不应出现在这里，原样保留
            out.push_str(&rest[start..end + 2]);
        } else if let Some(v) = ctx.value(name) {
            out.push_str(&v);
        } else {
            out.push_str(&rest[start..end + 2]);
        }
        rest = &rest[end + 2..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_plain_variables() {
        let ctx = PromptContext {
            transcript: "你好世界".into(),
            ..Default::default()
        };
        let out = render("转写：{{transcript}}", &ctx);
        assert_eq!(out, "转写：你好世界");
    }

    #[test]
    fn unknown_variable_is_left_intact() {
        let ctx = PromptContext::default();
        assert_eq!(render("{{unknown}}", &ctx), "{{unknown}}");
    }

    #[test]
    fn section_included_when_value_present() {
        let ctx = PromptContext {
            transcript: "t".into(),
            selection: Some("选中的文字".into()),
            ..Default::default()
        };
        let out = render("A{{#selection}}[{{selection}}]{{/selection}}B", &ctx);
        assert_eq!(out, "A[选中的文字]B");
    }

    #[test]
    fn section_dropped_when_value_absent() {
        let ctx = PromptContext {
            transcript: "t".into(),
            ..Default::default()
        };
        let out = render("A{{#selection}}[{{selection}}]{{/selection}}B", &ctx);
        assert_eq!(out, "AB");
    }

    #[test]
    fn section_dropped_when_blank() {
        let ctx = PromptContext {
            transcript: "t".into(),
            selection: Some("   ".into()),
            ..Default::default()
        };
        let out = render("A{{#selection}}[{{selection}}]{{/selection}}B", &ctx);
        assert_eq!(out, "AB");
    }

    #[test]
    fn multiple_sections_in_order() {
        let ctx = PromptContext {
            transcript: "问题".into(),
            selection: Some("上下文".into()),
            app_name: Some("TextEdit".into()),
            ..Default::default()
        };
        let out = render(
            "{{#selection}}S={{selection}}{{/selection}}|{{#app_name}}A={{app_name}}{{/app_name}}|{{#nope}}X{{/nope}}|Q={{transcript}}",
            &ctx,
        );
        assert_eq!(out, "S=上下文|A=TextEdit||Q=问题");
    }

    #[test]
    fn dictionary_section_lists_terms() {
        let ctx = PromptContext {
            transcript: "t".into(),
            dictionary_terms: vec!["Rust".into(), "Vox".into()],
            ..Default::default()
        };
        let out = render("{{#dictionary}}术语：{{dictionary}}{{/dictionary}}", &ctx);
        assert_eq!(out, "术语：Rust、Vox");
    }

    #[test]
    fn real_default_prompts_render() {
        let ctx = PromptContext {
            transcript: "今天天气不错".into(),
            target_language: Some("en-US".into()),
            ..Default::default()
        };
        let out = render(vox_core::config::DEFAULT_TRANSLATE_PROMPT, &ctx);
        assert!(out.contains("en-US"));
        assert!(out.contains("今天天气不错"));
    }
}
