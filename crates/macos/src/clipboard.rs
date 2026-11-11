//! 系统剪贴板读写。
//!
//! 直接调用 macOS 自带的 `pbcopy` / `pbpaste`，避免引入 Objective-C 绑定，
//! 也避免对剪贴板里的非文本类型造成副作用（我们只关心文本）。

use std::io::Write;
use std::process::{Command, Stdio};

use vox_core::error::{Error, Result};

/// 读取剪贴板中的纯文本。
pub fn read_clipboard() -> Result<String> {
    let out = Command::new("pbpaste")
        .output()
        .map_err(|e| Error::Io(format!("调用 pbpaste 失败: {e}")))?;
    if !out.status.success() {
        return Err(Error::Io(format!(
            "pbpaste 退出码 {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// 写入纯文本到剪贴板。
pub fn write_clipboard(text: &str) -> Result<()> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Io(format!("启动 pbcopy 失败: {e}")))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| Error::Io("pbcopy 没有 stdin".into()))?;
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| Error::Io(format!("写入 pbcopy 失败: {e}")))?;
    }
    let status = child
        .wait()
        .map_err(|e| Error::Io(format!("等待 pbcopy 结束失败: {e}")))?;
    if !status.success() {
        return Err(Error::Io(format!("pbcopy 退出码 {:?}", status.code())));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_clipboard_does_not_panic() {
        // 读写剪贴板会影响用户剪贴板，这里只验证读接口不 panic
        let _ = read_clipboard();
    }
}
