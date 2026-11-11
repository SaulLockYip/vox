//! 开机自启（macOS LaunchAgent）。
//!
//! 用 `~/Library/LaunchAgents/com.vox.desktop.plist` 实现：
//! - 开启：写 plist（`RunAtLoad` + `ProgramArguments` 指向当前可执行文件），
//!   再 `launchctl bootstrap gui/$UID` 让它**立即**生效（不用等下次登录）
//! - 关闭：`launchctl bootout` + 删除 plist

use std::path::PathBuf;
use std::process::Command;

use vox_core::error::{Error, Result};

const LABEL: &str = "com.vox.desktop";

fn plist_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| Error::Other("找不到 HOME".into()))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

/// 当前是否已配置开机自启。
pub fn is_launch_at_login() -> bool {
    plist_path().map(|p| p.exists()).unwrap_or(false)
}

/// 开启 / 关闭开机自启。
pub fn set_launch_at_login(enabled: bool) -> Result<()> {
    let path = plist_path()?;

    if enabled {
        let exe = std::env::current_exe()
            .map_err(|e| Error::Other(format!("取不到当前可执行文件路径: {e}")))?;

        let dir = path
            .parent()
            .ok_or_else(|| Error::Other("LaunchAgents 路径异常".into()))?;
        std::fs::create_dir_all(dir)?;

        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <false/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
            exe.to_string_lossy()
        );
        std::fs::write(&path, plist)?;

        // 立即生效（失败不致命：下次登录仍会自动加载）
        if let Some(uid) = uid() {
            let _ = Command::new("launchctl")
                .args(["bootstrap", &format!("gui/{uid}"), &path.to_string_lossy()])
                .output();
        }
    } else {
        if let Some(uid) = uid() {
            let _ = Command::new("launchctl")
                .args(["bootout", &format!("gui/{uid}/{LABEL}")])
                .output();
        }
        let _ = std::fs::remove_file(&path);
    }

    Ok(())
}

fn uid() -> Option<u32> {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u32>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_path_is_under_launch_agents() {
        let p = plist_path().unwrap();
        assert!(p.to_string_lossy().contains("LaunchAgents"));
        assert!(p.to_string_lossy().ends_with("com.vox.desktop.plist"));
    }

    #[test]
    fn status_query_does_not_panic() {
        let _ = is_launch_at_login();
    }
}
