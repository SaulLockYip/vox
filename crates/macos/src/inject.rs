//! 文本注入到当前焦点应用 + 前台应用识别。
//!
//! 注入方式：写入剪贴板 → 用 CGEvent 模拟 ⌘V → 恢复原剪贴板。
//! 这是最通用的注入方式，
//! 且不依赖任何闭源 dylib。

use std::ffi::c_void;
use std::process::Command;
use std::time::Duration;

use vox_core::error::{Error, Result};

pub use vox_core::config::InsertMethod;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreateKeyboardEvent(source: u32, keycode: u16, key_down: bool) -> *mut c_void;
    fn CGEventSetFlags(event: *mut c_void, flags: u64);
    fn CGEventPost(tap: u32, event: *mut c_void);
    fn CFRelease(cf: *const c_void);
}

/// `kVK_ANSI_V`
const KEYCODE_V: u16 = 0x09;
/// `kVK_ANSI_C`
const KEYCODE_C: u16 = 0x08;
/// `kCGEventFlagMaskCommand`
const FLAG_COMMAND: u64 = 1 << 20;
/// `kCGHIDEventTap`
const TAP_HID: u32 = 0;

/// 模拟按下 ⌘<某键>。
fn post_cmd(keycode: u16) {
    unsafe {
        let down = CGEventCreateKeyboardEvent(TAP_HID, keycode, true);
        if down.is_null() {
            return;
        }
        CGEventSetFlags(down, FLAG_COMMAND);
        CGEventPost(TAP_HID, down);
        CFRelease(down as *const c_void);

        let up = CGEventCreateKeyboardEvent(TAP_HID, keycode, false);
        if up.is_null() {
            return;
        }
        CGEventSetFlags(up, FLAG_COMMAND);
        CGEventPost(TAP_HID, up);
        CFRelease(up as *const c_void);
    }
}

/// 模拟按下 ⌘V。
fn post_cmd_v() {
    post_cmd(KEYCODE_V);
}

/// 模拟按下 ⌘C（复制）。
///
/// AX 读不到时的兜底 ——
/// AX 读不到选区时的兜底。
pub fn simulate_copy() {
    post_cmd(KEYCODE_C);
}

/// 把文本注入到当前焦点应用。
///
/// - `Clipboard`：写剪贴板 → ⌘V → （可选）恢复原剪贴板
/// - `Ax`：直接写焦点控件（尚未实现，回退为剪贴板方式）
/// - `Auto`：先试剪贴板，失败再试 AX
pub fn insert_text(text: &str, _method: InsertMethod, restore_clipboard: bool) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }

    // 还原时要用**原始字节**：剪贴板里可能有非 UTF-8 内容，
    // 走 String 往返会把用户的剪贴板变成 `�` 乱码。
    let previous = if restore_clipboard {
        crate::clipboard::read_clipboard_bytes().ok()
    } else {
        None
    };

    crate::clipboard::write_clipboard(text)?;

    // **写完先验一下**：读回来必须和写进去的一致，
    // 否则目标应用粘到的会是旧内容/乱码 —— 那还不如不粘，直接报错。
    //
    // 用 `read_clipboard()`（走 NSPasteboard）而不是 `pbpaste`：
    // **App 粘到的是 NSPasteboard 那一份**，校验必须校同一份，
    // 否则会出现「我们自己读是对的、用户粘出来是乱码」。
    match crate::clipboard::read_clipboard() {
        Ok(back) if back == text => {}
        Ok(_) => {
            return Err(Error::Io(
                "写入剪贴板后读回的内容不一致（编码问题），已放弃粘贴以免出现乱码".into(),
            ));
        }
        Err(e) => {
            return Err(Error::Io(format!("校验剪贴板失败: {e}")));
        }
    }

    // 给目标应用一点时间取到剪贴板内容
    std::thread::sleep(Duration::from_millis(120));
    post_cmd_v();

    // 粘贴后固定等 500ms，让目标应用有时间取走剪贴板内容
    std::thread::sleep(Duration::from_millis(500));

    if let Some(prev) = previous {
        // 恢复原剪贴板（非 UTF-8 的内容会被跳过，写回只会变成乱码）
        let _ = crate::clipboard::restore_clipboard_safely(&prev);
    }

    Ok(())
}

/// 取当前前台应用名称（无需额外权限）。
pub fn frontmost_app_name() -> Option<String> {
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
    // 输出形如："LSDisplayName"="TextEdit"
    let name = text
        .split('"')
        .nth(3)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    Some(name)
}

/// 当前进程是否具备辅助功能权限（用于模拟按键与热键监听）。
pub fn has_accessibility_permission() -> bool {
    crate::ax::has_accessibility_permission()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmost_app_is_detected() {
        let name = frontmost_app_name();
        assert!(name.is_some(), "应能取到前台应用名");
    }

    #[test]
    fn empty_text_is_noop() {
        assert!(insert_text("", InsertMethod::Clipboard, false).is_ok());
    }
}
