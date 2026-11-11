//! 本地工具执行器（给 Ask Anything 用）。
//!
//! 问答能真的去查实时数据（比如「明天北京天气」），靠的是在本地执行工具
//! 执行能力。Vox 走的是自己的 MiniMax 接口，没有服务端工具，所以这里在**本地**
//! 补上：把工具定义交给模型，模型要调就调，我们执行完把输出喂回去，模型再给最终答案。
//!
//! ## 权限档位（`general.agent_tools`）
//!
//! | 值 | 行为 |
//! |---|---|
//! | `off` | 完全不给工具，模型只能凭已有知识回答 |
//! | `safe`（默认） | 只允许**只读**命令白名单（curl / date / open / grep …） |
//! | `all` | 允许任意命令（等于把 shell 交给模型，请自行评估风险） |
//!
//! `safe` 白名单里**故意不含** `python3`/`sh`/`perl`/`ruby` 之类 —— 它们等于任意代码执行，
//! 给了就等于 `all`。模型想格式化 JSON 可以自己解析，或用 `jq`。
//!
//! 无论哪档，都有**超时**（默认 15 秒）和**输出上限**（默认 16KB），避免一条命令
//! 把界面卡死或刷屏。

use std::process::{Command, Stdio};
use std::time::Duration;

use vox_providers::llm::ToolDef;

/// 工具名。
pub const TOOL_RUN_COMMAND: &str = "run_command";

/// `safe` 档允许的命令白名单（只读 / 无副作用）。
pub const SAFE_COMMANDS: &[&str] = &[
    // 网络与信息
    "curl",
    "ping",
    "date",
    "cal",
    "uname",
    "sw_vers",
    "hostname",
    "whoami",
    "id",
    "pwd",
    "which",
    "scutil",
    "networksetup",
    "system_profiler",
    "sysctl",
    "ifconfig",
    // 文件与文本（只读）
    "ls",
    "cat",
    "head",
    "tail",
    "wc",
    "grep",
    "sed",
    "awk",
    "sort",
    "uniq",
    "jq",
    "echo",
    "stat",
    "file",
    "find",
    // 打开网址 / 应用
    "open",
];

/// 工具权限档。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPerm {
    /// 不给工具
    Off,
    /// 只允许只读白名单
    Safe,
    /// 允许任意命令
    All,
}

impl ToolPerm {
    pub fn parse(s: &str) -> ToolPerm {
        match s.trim().to_ascii_lowercase().as_str() {
            "all" | "全部" | "任意" => ToolPerm::All,
            "off" | "none" | "关闭" => ToolPerm::Off,
            _ => ToolPerm::Safe,
        }
    }
}

/// 单次工具调用的输出上限。
const MAX_OUTPUT: usize = 16 * 1024;
/// 单次工具调用的超时。
const TIMEOUT: Duration = Duration::from_secs(15);

/// 生成给模型看的工具定义（`perm = Off` 时返回空）。
pub fn tool_defs(perm: ToolPerm) -> Vec<ToolDef> {
    if perm == ToolPerm::Off {
        return Vec::new();
    }
    let allowed = if perm == ToolPerm::All {
        "任意命令".to_string()
    } else {
        SAFE_COMMANDS.join(", ")
    };
    vec![ToolDef::new(
        TOOL_RUN_COMMAND,
        format!(
            "在 macOS 上执行一条 shell 命令并返回它的输出（stdout+stderr）。\
             用来查实时信息，例如用 curl 请求天气/新闻/股价等接口，再用 grep/jq 提取需要的字段。\
             可用命令：{allowed}。\
             命令要尽量短；输出太长时只取需要的部分。"
        ),
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "要执行的 shell 命令",
                }
            },
            "required": ["command"]
        }),
    )]
}

/// 执行模型给的命令，返回输出文本（出错时把错误信息也作为输出返回给模型）。
pub fn execute(command: &str, perm: ToolPerm) -> String {
    let cmd = command.trim();
    if cmd.is_empty() {
        return "错误：命令为空".into();
    }
    if perm == ToolPerm::Off {
        return "错误：未启用命令执行能力".into();
    }
    if perm == ToolPerm::Safe {
        if let Some(bad) = first_denied_program(cmd) {
            return format!(
                "错误：命令 `{bad}` 不在允许列表里。可用命令：{}",
                SAFE_COMMANDS.join(", ")
            );
        }
        if let Some(bad) = denied_argument(cmd) {
            return format!("错误：不允许使用 `{bad}`（只允许只读操作）");
        }
    }

    match run(cmd) {
        Ok(out) => {
            if out.trim().is_empty() {
                "（命令执行成功，但没有输出）".to_string()
            } else {
                out
            }
        }
        Err(e) => format!("错误：命令执行失败：{e}"),
    }
}

/// 真正跑命令（带超时与输出上限）。
fn run(cmd: &str) -> std::io::Result<String> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // 超时后杀掉
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        match child.try_wait()? {
            Some(_) => break,
            None => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(format!("（命令超过 {} 秒，已终止）", TIMEOUT.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    let out = child.wait_with_output()?;
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.stderr.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        if !err.trim().is_empty() {
            text.push_str("\n[stderr] ");
            text.push_str(err.trim());
        }
    }
    if text.len() > MAX_OUTPUT {
        text.truncate(MAX_OUTPUT);
        text.push_str("\n…（输出过长，已截断）");
    }
    Ok(text)
}

/// 找出命令里**第一个**不在白名单里的程序名。
///
/// 按 `|`、`;`、`&&`、`||`、换行切成若干段，每段取第一个词来判断。
fn first_denied_program(cmd: &str) -> Option<String> {
    for seg in split_segments(cmd) {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        let mut words = seg.split_whitespace();
        let Some(first) = words.next() else { continue };
        // `VAR=value cmd` 这种前缀赋值形式
        let prog = if first.contains('=') {
            words.next().unwrap_or("")
        } else {
            first
        };
        if prog.is_empty() {
            continue;
        }
        // 取路径的最后一段（/bin/curl → curl）
        let prog = prog.rsplit('/').next().unwrap_or(prog);
        if !SAFE_COMMANDS.contains(&prog) {
            return Some(prog.to_string());
        }
    }
    None
}

/// 检查有没有「有副作用」的参数（写文件、改数据）。
fn denied_argument(cmd: &str) -> Option<&'static str> {
    let lower = cmd.to_ascii_lowercase();
    for bad in [
        " -d ", "--data", " -t ", "--upload-file", " -o ", ">", ">>", " -x post", " -x put",
        " -x delete", "rm ", "mv ", "cp ", "chmod", "chown", "sudo",
    ] {
        if lower.contains(bad) {
            return Some(bad.trim());
        }
    }
    None
}

/// 把命令按 shell 分隔符切成段。
fn split_segments(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = cmd.chars().peekable();
    let mut in_quote: Option<char> = None;

    while let Some(c) = chars.next() {
        match in_quote {
            Some(q) => {
                cur.push(c);
                if c == q {
                    in_quote = None;
                }
            }
            None => match c {
                '\'' | '"' => {
                    in_quote = Some(c);
                    cur.push(c);
                }
                '|' | ';' | '\n' => {
                    out.push(std::mem::take(&mut cur));
                    // 跳过 && / || 的第二个符号
                    if c == '|' && chars.peek() == Some(&'|') {
                        chars.next();
                    }
                }
                '&' => {
                    out.push(std::mem::take(&mut cur));
                    if chars.peek() == Some(&'&') {
                        chars.next();
                    }
                }
                _ => cur.push(c),
            },
        }
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_perm_allows_whitelisted() {
        assert!(first_denied_program("curl -s https://wttr.in/Beijing").is_none());
        assert!(first_denied_program("date +%s").is_none());
        assert!(first_denied_program("curl -s 'https://x/y' | jq .a").is_none());
    }

    #[test]
    fn safe_perm_blocks_dangerous() {
        assert_eq!(
            first_denied_program("python3 -c 'import os'").as_deref(),
            Some("python3")
        );
        assert_eq!(first_denied_program("rm -rf /").as_deref(), Some("rm"));
        assert_eq!(
            first_denied_program("/bin/bash -c ls").as_deref(),
            Some("bash")
        );
    }

    #[test]
    fn blocks_write_arguments() {
        assert!(denied_argument("curl -d @x https://a").is_some());
        assert!(denied_argument("curl https://a > out.txt").is_some());
        assert!(denied_argument("curl -s https://a").is_none());
    }

    #[test]
    fn off_perm_has_no_tools() {
        assert!(tool_defs(ToolPerm::Off).is_empty());
        assert!(!tool_defs(ToolPerm::Safe).is_empty());
    }

    #[test]
    fn executes_and_captures_output() {
        let out = execute("echo hello-vox", ToolPerm::Safe);
        assert!(out.contains("hello-vox"), "got {out}");
    }

    #[test]
    fn off_perm_refuses_to_run() {
        let out = execute("echo hi", ToolPerm::Off);
        assert!(out.contains("错误"), "got {out}");
    }
}
