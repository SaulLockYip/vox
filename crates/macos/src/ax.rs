//! macOS 辅助功能（AX）读取。
//!
//! 用途：拿到**前台应用焦点控件里选中的文字**，作为 Ask Anything 的上下文。
//!
//! 实现走 ApplicationServices 的 AXUIElement FFI（不引入 Objective-C 绑定，
//! 免去 objc2 版本兼容问题）：
//! 1. `lsappinfo front` → 前台应用的 ASN → `lsappinfo info -only pid` → pid
//! 2. `AXUIElementCreateApplication(pid)` → 读 `AXFocusedUIElement`
//! 3. 在焦点控件上读 `AXSelectedText`
//!
//! 注意：需要「辅助功能」权限；读不到时返回 None，调用方应回退到模拟 ⌘C。

use std::ffi::{c_void, CString};
use std::process::Command;

#[allow(non_camel_case_types)]
type CFTypeRef = *const c_void;
#[allow(non_camel_case_types)]
type CFStringRef = *const c_void;
#[allow(non_camel_case_types)]
type AXUIElementRef = *const c_void;
type AXError = i32;

/// `kCFStringEncodingUTF8`
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        c_str: *const std::os::raw::c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFStringGetLength(s: CFStringRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFStringGetCString(
        s: CFStringRef,
        buffer: *mut u8,
        buffer_size: isize,
        encoding: u32,
    ) -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    /// 当前进程是否已被授予「辅助功能」权限。
    fn AXIsProcessTrusted() -> bool;
}

/// 当前进程是否拿到了「辅助功能」权限。
///
/// 这是 macOS 官方的判定方式：全局热键监听（CGEventTap）和 ⌘V 注入都依赖它。
/// 注意：**ad-hoc 签名**的 app 每次重新签名后 code signature 会变，
/// 系统可能因此把之前的授权判为无效 —— 所以这个值要实时查，不能假设「以前给过」。
pub fn has_accessibility_permission() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// 取前台应用的 pid。
pub fn frontmost_app_pid() -> Option<i32> {
    let asn = Command::new("lsappinfo").arg("front").output().ok()?;
    let asn = String::from_utf8_lossy(&asn.stdout).trim().to_string();
    if asn.is_empty() {
        return None;
    }
    let out = Command::new("lsappinfo")
        .args(["info", "-only", "pid", &asn])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    // 输出形如："pid"=1974
    let pid = text
        .split('=')
        .nth(1)
        .map(|s| s.trim().trim_matches('"'))
        .and_then(|s| s.parse::<i32>().ok())?;
    Some(pid)
}

/// 取前台应用名（展示用）。
pub fn frontmost_app_display_name() -> Option<String> {
    let asn = Command::new("lsappinfo").arg("front").output().ok()?;
    let asn = String::from_utf8_lossy(&asn.stdout).trim().to_string();
    if asn.is_empty() {
        return None;
    }
    let out = Command::new("lsappinfo")
        .args(["info", "-only", "name", &asn])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let name = text
        .split('"')
        .nth(3)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    Some(name)
}

/// 读前台应用焦点控件里**选中的文字**。
///
/// 返回 None 表示：没有辅助功能权限 / 没有焦点控件 / 没有选中任何文本。
pub fn selected_text() -> Option<String> {
    let pid = frontmost_app_pid()?;
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return None;
        }
        let focused = copy_attribute(app, "AXFocusedUIElement")?;
        let selected = copy_attribute(focused, "AXSelectedText")?;
        let text = cfstring_to_string(selected);
        match text {
            Some(t) if !t.is_empty() => Some(t),
            _ => None,
        }
    }
}

/// 读选中的文字，AX 读不到时用 **⌘C 复制**兜底。
///
/// AX 读不到时的兜底：模拟 ⌘C 复制。
/// 兜底会临时改剪贴板，读完立刻还原。
pub fn selected_text_with_fallback() -> Option<String> {
    if let Some(t) = selected_text() {
        if !t.trim().is_empty() {
            return Some(t);
        }
    }

    // 用**原始字节**做前后对比与还原：剪贴板里可能有非 UTF-8 内容，
    // 走 String 往返会把它变成 `�` 乱码，还会把乱码当成「选区」喂给模型。
    let prev = crate::clipboard::read_clipboard_bytes().ok();
    crate::inject::simulate_copy();
    std::thread::sleep(std::time::Duration::from_millis(180));
    let got = crate::clipboard::read_clipboard_bytes().ok();

    // ⌘C 之后剪贴板**没变** → 说明其实没选中任何东西（只是光标），
    // 这时不能把剪贴板里原有的内容当成选区喂给模型。
    let copied = match (&prev, &got) {
        (Some(a), Some(b)) => a != b,
        (None, Some(b)) => !b.is_empty(),
        _ => false,
    };

    // 还原剪贴板（非 UTF-8 的内容会被跳过，写回只会变成乱码）
    if let Some(p) = prev {
        let _ = crate::clipboard::restore_clipboard_safely(&p);
    }

    if !copied {
        return None;
    }
    // 只有真正复制到的内容才当「选区」（解码成文本给人/模型看）
    got.map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .filter(|t| !t.is_empty())
}

/// 光标周围的文本上下文。
///
/// 取光标前后的文字，作为问答的上下文。
#[derive(Debug, Clone, Default)]
pub struct CursorContext {
    /// 光标之前的文字
    pub before: String,
    /// 选中的文字
    pub selected: String,
    /// 光标之后的文字
    pub after: String,
}

/// 读焦点控件里光标前后的文字。
///
/// 不依赖 `AXSelectedTextRange`（不同 app 返回的类型五花八门，硬读会抛 C 异常），
/// 改成：拿整段文本 + 选区文本，再在整段里定位选区。
pub fn cursor_context(max_before: usize, max_after: usize) -> Option<CursorContext> {
    let pid = frontmost_app_pid()?;
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return None;
        }
        let focused = copy_attribute(app, "AXFocusedUIElement")?;
        let full = copy_attribute(focused, "AXValue")
            .and_then(|v| cfstring_to_string(v))
            .unwrap_or_default();
        let selected = copy_attribute(focused, "AXSelectedText")
            .and_then(|v| cfstring_to_string(v))
            .unwrap_or_default();

        if full.trim().is_empty() && selected.trim().is_empty() {
            return None;
        }

        // 有选区 → 在整段里找它的位置；没选区（只有光标）→ 取整段尾部作为「之前」
        let (before, after) = if !selected.is_empty() {
            match full.find(&selected) {
                Some(pos) => {
                    let start = full[..pos].chars().count();
                    let end = start + selected.chars().count();
                    split_context(&full, start, end, max_before, max_after)
                }
                None => (tail(&full, max_before), String::new()),
            }
        } else {
            (tail(&full, max_before), String::new())
        };

        Some(CursorContext {
            before,
            selected,
            after,
        })
    }
}

/// 取整段文本的最后 N 个字符。
fn tail(text: &str, n: usize) -> String {
    text.chars().rev().take(n).collect::<Vec<_>>().into_iter().rev().collect()
}

/// 按字符位置切出前后文。
fn split_context(
    full: &str,
    start: usize,
    end: usize,
    max_before: usize,
    max_after: usize,
) -> (String, String) {
    let chars: Vec<char> = full.chars().collect();
    let start = start.min(chars.len());
    let end = end.min(chars.len());
    let before: String = chars[..start]
        .iter()
        .rev()
        .take(max_before)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let after: String = chars[end..].iter().take(max_after).collect();
    (before, after)
}

/// 读某个 AX 元素的属性值。
unsafe fn copy_attribute(element: AXUIElementRef, attribute: &str) -> Option<CFTypeRef> {
    let attr_name = CString::new(attribute).ok()?;
    let attr = CFStringCreateWithCString(
        std::ptr::null(),
        attr_name.as_ptr(),
        CF_STRING_ENCODING_UTF8,
    );
    if attr.is_null() {
        return None;
    }

    let mut value: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(element, attr, &mut value);
    CFRelease(attr);

    if err != 0 || value.is_null() {
        return None;
    }
    Some(value)
}

/// CFString → Rust String。
unsafe fn cfstring_to_string(s: CFTypeRef) -> Option<String> {    if s.is_null() {
        return None;
    }
    let len = CFStringGetLength(s as CFStringRef);
    let size = CFStringGetMaximumSizeForEncoding(len, CF_STRING_ENCODING_UTF8);
    if size <= 0 {
        return None;
    }
    let mut buf = vec![0u8; size as usize + 1];
    if !CFStringGetCString(
        s as CFStringRef,
        buf.as_mut_ptr(),
        size,
        CF_STRING_ENCODING_UTF8,
    ) {
        return None;
    }
    Some(String::from_utf8_lossy(&buf[..size as usize]).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmost_pid_is_readable() {
        // 前台应用一定存在（至少是 Finder / 通知中心）
        let pid = frontmost_app_pid();
        assert!(pid.is_some(), "应能取到前台应用 pid");
        assert!(pid.unwrap() > 0);
    }

    #[test]
    fn selected_text_does_not_panic() {
        // 没有焦点控件/无选区时返回 None，不应 panic
        let _ = selected_text();
    }

    #[test]
    fn cursor_context_does_not_panic() {
        let _ = cursor_context(100, 100);
    }
}
