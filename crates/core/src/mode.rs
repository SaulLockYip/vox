use serde::{Deserialize, Serialize};

/// 三种核心模式：听写 / 翻译 / 问答。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Dictation,
    Translate,
    AskAnything,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Dictation => "dictation",
            Mode::Translate => "translate",
            Mode::AskAnything => "ask_anything",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Mode::Dictation => "听写",
            Mode::Translate => "翻译",
            Mode::AskAnything => "Ask Anything",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dictation" | "dictate" | "听写" => Some(Mode::Dictation),
            "translate" | "translation" | "翻译" => Some(Mode::Translate),
            "ask" | "ask_anything" | "askanything" => Some(Mode::AskAnything),
            _ => None,
        }
    }
}

pub const MODE_LABELS: [(&str, &str); 3] = [
    ("dictation", "听写"),
    ("translate", "翻译"),
    ("ask_anything", "Ask Anything"),
];

/// **投递方式**：结果最终怎么呈现给用户。
///
/// 结果的投递方式：显示在面板 / 写进光标 / 替换选区。
/// 关键点：**由模型自己判断** —— 是「解释一下」（显示面板）还是「写进文档」（插入光标）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// 显示在浮窗面板里（默认）。
    #[default]
    Reply,
    /// 插入到焦点应用的光标处。
    Insert,
    /// 替换焦点应用里选中的内容（选区仍在时会随粘贴一起被替换）。
    Replace,
}

impl Delivery {
    pub fn as_str(&self) -> &'static str {
        match self {
            Delivery::Reply => "reply",
            Delivery::Insert => "insert",
            Delivery::Replace => "replace",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Delivery::Reply => "显示在面板",
            Delivery::Insert => "插入到光标",
            Delivery::Replace => "替换选区",
        }
    }

    /// 解析模型给的标记（`[[deliver: insert]]` 里的 `insert`）。
    pub fn parse(s: &str) -> Option<Delivery> {
        match s.trim().to_ascii_lowercase().as_str() {
            "reply" | "panel" | "show" | "ui" => Some(Delivery::Reply),
            "insert" | "type" | "input" | "cursor" => Some(Delivery::Insert),
            "replace" | "selection" => Some(Delivery::Replace),
            _ => None,
        }
    }

    /// 是否要把文本写进焦点应用。
    pub fn writes_into_app(&self) -> bool {
        matches!(self, Delivery::Insert | Delivery::Replace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_defaults_to_reply() {
        assert_eq!(Delivery::default(), Delivery::Reply);
        // 默认**不**写进光标
        assert!(!Delivery::default().writes_into_app());
    }

    #[test]
    fn delivery_parses_marker_values() {
        assert_eq!(Delivery::parse("reply"), Some(Delivery::Reply));
        assert_eq!(Delivery::parse("INSERT"), Some(Delivery::Insert));
        assert_eq!(Delivery::parse(" replace "), Some(Delivery::Replace));
        assert_eq!(Delivery::parse("teleport"), None);
    }

    #[test]
    fn insert_and_replace_write_into_app() {
        assert!(Delivery::Insert.writes_into_app());
        assert!(Delivery::Replace.writes_into_app());
        assert!(!Delivery::Reply.writes_into_app());
    }
}

/// **主控热键**对应的功能名。
///
/// 它既是「听写」的开始键，也是**所有模式统一的结束键**：
/// - 没在录音时按它 → 开始听写
/// - 正在录音时按它 → 结束本次会话（不管当前是听写/翻译/Ask）
///
/// 三种功能只用「开始热键」区分，结束键统一。
pub const MASTER_FEATURE: &str = "dictation";
