//! 个人词典：词条与替换规则完全在本地实现。
//!
//! 词条语义：
//! - `term`：需要正确识别的词（会作为提示注入 prompt）
//! - `replacement`：可选的替换结果（若为空则不做本地替换，只做提示）
//! - `auto`：是否由系统自动捕获

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DictionaryEntry {
    pub id: i64,
    pub term: String,
    #[serde(default)]
    pub replacement: String,
    #[serde(default)]
    pub auto: bool,
    #[serde(default)]
    pub created_at: String,
}

impl DictionaryEntry {
    /// 是否有可执行的本地替换。
    pub fn has_replacement(&self) -> bool {
        let r = self.replacement.trim();
        !r.is_empty() && r != self.term.trim()
    }
}

/// 对文本应用词典替换。
///
/// - 拉丁字母词：大小写不敏感 + 全词匹配（`vox` 不会命中 `voxel`）
/// - CJK 等无空格文本：直接全串替换
pub fn apply_dictionary(text: &str, entries: &[DictionaryEntry]) -> String {
    let mut out = text.to_string();
    for entry in entries {
        if !entry.has_replacement() {
            continue;
        }
        let term = entry.term.trim();
        let replacement = entry.replacement.trim();
        if term.is_empty() {
            continue;
        }
        out = replace_word(&out, term, replacement);
    }
    out
}

/// 全词替换：拉丁词要求前后不是字母数字，中文等直接替换。
fn replace_word(text: &str, term: &str, replacement: &str) -> String {
    if !is_word_like(term) {
        return text.replace(term, replacement);
    }

    let mut out = String::with_capacity(text.len());
    let lower_text = text.to_lowercase();
    let lower_term = term.to_lowercase();
    let mut i = 0usize;

    while i < text.len() {
        if lower_text[i..].starts_with(&lower_term) {
            let end = i + term.len();
            let before_ok = i == 0 || !is_word_char(prev_char(text, i));
            let after_ok = end >= text.len() || !is_word_char(text[end..].chars().next().unwrap());
            if before_ok && after_ok {
                // 保留原始大小写风格：原词首字母大写则替换词也首字母大写
                let repl = if starts_uppercase(&text[i..end]) {
                    capitalize_first(replacement)
                } else {
                    replacement.to_string()
                };
                out.push_str(&repl);
                i = end;
                continue;
            }
        }
        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn prev_char(text: &str, i: usize) -> char {
    text[..i].chars().next_back().unwrap_or(' ')
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 是否是「拉丁词」——只有 ASCII 字母数字/下划线/连字符才算。
///
/// 必须用 ASCII 判定：Unicode 里汉字也满足 `is_alphanumeric()`，
/// 会让中文词条走全词匹配路径而永远匹配不上。
fn is_word_like(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn starts_uppercase(s: &str) -> bool {
    s.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(term: &str, replacement: &str) -> DictionaryEntry {
        DictionaryEntry {
            id: 1,
            term: term.into(),
            replacement: replacement.into(),
            auto: false,
            created_at: String::new(),
        }
    }

    #[test]
    fn replaces_whole_word_only() {
        let e = vec![entry("vox", "Vox")];
        assert_eq!(apply_dictionary("vox is nice", &e), "Vox is nice");
        assert_eq!(apply_dictionary("voxel engine", &e), "voxel engine");
    }

    #[test]
    fn case_insensitive_but_preserves_capitalization() {
        let e = vec![entry("rust", "Rust")];
        assert_eq!(apply_dictionary("I love rust", &e), "I love Rust");
        assert_eq!(apply_dictionary("I love RUST", &e), "I love Rust");
    }

    #[test]
    fn cjk_replaces_as_substring() {
        let e = vec![entry("微信", "WeChat")];
        assert_eq!(apply_dictionary("用微信聊天", &e), "用WeChat聊天");
    }

    #[test]
    fn entry_without_replacement_is_noop() {
        let e = vec![entry("Vox", "")];
        assert_eq!(apply_dictionary("vox", &e), "vox");
    }

    #[test]
    fn same_term_and_replacement_is_noop() {
        let e = vec![entry("Vox", "Vox")];
        assert_eq!(apply_dictionary("vox", &e), "vox");
    }

    #[test]
    fn multiple_entries_apply_in_order() {
        let e = vec![entry("a", "A"), entry("b", "B")];
        assert_eq!(apply_dictionary("a and b", &e), "A and B");
    }
}
