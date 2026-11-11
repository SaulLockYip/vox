//! Vox CLI —— M1 阶段的命令行入口。
//!
//! ```text
//! vox init                     写出默认配置
//! vox doctor                   体检：配置 / 密钥 / 麦克风 / 权限
//! vox asr <file.wav>           只做语音识别
//! vox llm "<文本>"             只做 LLM
//! vox dictation [--no-insert]  录音（回车停止）→ 整理 → 注入
//! vox translate [--target en-US]
//! vox ask
//! vox hotkey                   常驻热键模式
//! vox history [--limit 20]
//! vox dict list|add|rm|import
//! vox secret list|set|migrate
//! ```

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use vox_app::VoxApp;
use vox_core::config::{Config, PromptsConfig};
use vox_core::mode::Mode;
use vox_macos::hotkey::{HotkeyEvent, HotkeyListener, KeyCombo};
use vox_pipeline::template::PromptContext;
use vox_providers::asr::AsrClient;
use vox_providers::llm::{LlmClient, LlmRequest};

const USAGE: &str = r#"Vox —— 无登录的语音输入工具（听写 / 翻译 / Ask Anything）

用法:
  vox init                     写出默认配置
  vox doctor                   体检：配置 / 密钥 / 麦克风 / 权限
  vox asr <file.wav>           只做语音识别
  vox llm "<文本>"             只做 LLM（按听写 prompt 处理）
  vox dictation [--no-insert]  录音（回车停止）→ 整理 → 注入
  vox translate [--target en-US]
  vox ask [--no-insert]
  vox hotkey                   常驻热键模式
  vox audio                    把蓝牙耳机的音质切回 A2DP（录音后卡在单声道时用）
  vox history [--limit 20]
  vox dict list
  vox dict add <词> [替换为]
  vox dict rm <id>
  vox dict import <csv文件>
  vox secret list               列出已存的密钥（只显示条目名）
  vox secret set <stt|llm>      从标准输入读密钥并加密保存
  vox secret migrate            把系统钥匙串里的旧密钥迁到加密文件
  vox prompt show               打印当前的三个 prompt
  vox prompt reset              把三个 prompt 恢复为内置默认值
  vox prompt set <模式> <文件>    从文件导入某个模式的 prompt

配置: ~/Library/Application Support/Vox/settings.toml
密钥: ~/Library/Application Support/Vox/secrets.json（XChaCha20-Poly1305 加密）
"#;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print!("{USAGE}");
        return Ok(());
    }

    let cmd = args[0].clone();
    let rest = args[1..].to_vec();

    match cmd.as_str() {
        "init" => cmd_init(),
        "doctor" => cmd_doctor(),
        "asr" => cmd_asr(&rest).await,
        "selection" => cmd_selection(),
        "llm" => cmd_llm(&rest).await,
        "dictation" | "dictate" => cmd_record(Mode::Dictation, &rest).await,
        "translate" | "translation" => cmd_record(Mode::Translate, &rest).await,
        "ask" | "ask-anything" => cmd_record(Mode::AskAnything, &rest).await,
        "run" => cmd_run(&rest).await,
        "hotkey" => cmd_hotkey().await,
        "audio" | "audio-restore" => cmd_audio_restore(),
        "history" => cmd_history(&rest),
        "dict" => cmd_dict(&rest),
        "secret" | "secrets" => cmd_secret(&rest),
        "prompt" | "prompts" => cmd_prompt(&rest),
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => {
            eprintln!("未知命令: {other}\n");
            print!("{USAGE}");
            Ok(())
        }
    }
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn opt(args: &[String], name: &str) -> Option<String> {
    let prefix = format!("--{name}=");
    for a in args {
        if let Some(v) = a.strip_prefix(&prefix) {
            return Some(v.to_string());
        }
    }
    for (i, a) in args.iter().enumerate() {
        if a == &format!("--{name}") {
            return args.get(i + 1).cloned();
        }
    }
    None
}

fn cmd_init() -> Result<()> {
    let path = Config::path();
    if path.exists() {
        println!("配置已存在: {}", path.display());
        println!("如需重置，请先手动移走该文件。");
        return Ok(());
    }
    Config::default().save()?;
    println!("已写出默认配置: {}", path.display());
    println!();
    println!("接下来：");
    println!("  1) 存 API Key 到钥匙串（推荐）:");
    println!("     security add-generic-password -s vox -a vox.llm -w '<你的 key>'");
    println!("     security add-generic-password -s vox -a vox.stt -w '<你的 key>'");
    println!("  2) 运行 `vox doctor` 自检");
    Ok(())
}

fn cmd_doctor() -> Result<()> {
    let cfg = Config::load()?;
    println!("配置路径: {}", Config::path().display());
    println!(
        "  ASR : {}  (model={}, language={})",
        cfg.stt.url(),
        cfg.stt.model,
        if cfg.stt.language.is_empty() {
            "混合".to_string()
        } else {
            cfg.stt.language.clone()
        }
    );
    println!(
        "  LLM : {}  (model={}, effort={})",
        cfg.llm.url(),
        cfg.llm.model,
        cfg.llm.effort
    );

    match cfg.stt.api_key() {
        Ok(_) => println!("  ✅ ASR 密钥可用"),
        Err(e) => println!("  ❌ ASR 密钥: {e}"),
    }
    match cfg.llm.api_key() {
        Ok(_) => println!("  ✅ LLM 密钥可用"),
        Err(e) => println!("  ❌ LLM 密钥: {e}"),
    }

    match vox_audio::recorder::list_input_devices() {
        Ok(list) if !list.is_empty() => {
            println!("  ✅ 麦克风: {} 个可用", list.len());
            for name in list.iter().take(5) {
                println!("     - {name}");
            }
        }
        Ok(_) => println!("  ❌ 没有找到可用的麦克风"),
        Err(e) => println!("  ❌ 麦克风: {e}"),
    }

    match KeyCombo::parse(&cfg.general.hotkey_dictation) {
        Ok(_) => println!("  ✅ 热键配置有效（首次监听可能需要授权辅助功能）"),
        Err(e) => println!("  ❌ 热键配置无效: {e}"),
    }

    println!();
    println!("数据库: {}", vox_app::db_path().display());
    Ok(())
}

async fn cmd_asr(args: &[String]) -> Result<()> {
    let path = args.first().context("用法: vox asr <file.wav>")?;
    let cfg = Config::load()?;
    let client = AsrClient::new(cfg.stt.clone())?;
    let result = client.transcribe_file(&PathBuf::from(path)).await?;
    println!("转写: {}", result.text);
    if let Some(d) = result.duration {
        println!("时长: {d:.2}s");
    }
    Ok(())
}

/// 打印前台应用焦点控件里的选区（取不到则回退剪贴板）。
fn cmd_selection() -> Result<()> {
    let app = vox_macos::inject::frontmost_app_name().unwrap_or_else(|| "(未知)".to_string());
    println!("前台应用: {app}");
    match vox_macos::selected_text() {
        Some(text) => println!("选区（AX）: {text}"),
        None => {
            println!("AX 选区: 读不到（需要辅助功能权限，或焦点控件没有选中文字）");
            match vox_macos::read_clipboard() {
                Ok(text) if !text.trim().is_empty() => println!("剪贴板回退: {}", text.trim()),
                _ => println!("剪贴板回退: 空"),
            }
        }
    }
    Ok(())
}

async fn cmd_llm(args: &[String]) -> Result<()> {
    // 子命令：reset（恢复默认参数）/ show（看生效参数）
    match args.first().map(|s| s.as_str()) {
        Some("reset") => {
            let mut cfg = Config::load()?;
            cfg.llm = vox_core::config::LlmConfig::default();
            cfg.save()?;
            println!("已把 LLM 参数恢复为默认（听写/翻译=关思考，Ask=开思考低强度）");
            return Ok(());
        }
        Some("show") => {
            let cfg = Config::load()?;
            println!("通用（听写/翻译）:");
            println!("  model    = {}", cfg.llm.model);
            println!("  thinking = {:?}", cfg.llm.thinking);
            println!("  effort   = {}", cfg.llm.effort);
            println!("Ask Anything:");
            println!("  model    = {:?}", cfg.llm.ask.model);
            println!("  thinking = {:?}", cfg.llm.ask.thinking);
            println!("  effort   = {:?}", cfg.llm.ask.effort);
            println!("service_tier = {}", cfg.llm.service_tier);
            return Ok(());
        }
        _ => {}
    }

    let use_ask = flag(args, "--ask");
    let text = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .context("用法: vox llm [--ask] \"<文本>\"")?;
    let cfg = Config::load()?;
    let client = LlmClient::new(cfg.llm.clone())?;
    let ctx = PromptContext {
        transcript: text.clone(),
        ..Default::default()
    };
    // --ask 用 Ask Anything 的 prompt 与参数（开思考、低强度）；否则用润色档（关思考求快）
    let (prompt, overrides) = if use_ask {
        (
            vox_pipeline::render(&cfg.prompts.ask_anything, &ctx),
            cfg.llm.ask.clone(),
        )
    } else {
        (
            vox_pipeline::render(&cfg.prompts.dictation, &ctx),
            vox_core::config::LlmOverrides::default(),
        )
    };
    let t0 = std::time::Instant::now();
    let out = client
        .complete(&LlmRequest::new(prompt).with_overrides(overrides))
        .await?;
    eprintln!("（耗时 {:.2}s）", t0.elapsed().as_secs_f64());
    println!("{out}");
    Ok(())
}

/// 对一个已有的音频文件跑完整条流水线（ASR → LLM → 词典 → 落历史）。
///
/// 用来在没有麦克风的情况下验证整条链路，也方便复现问题：
/// `vox run dictation /tmp/a.wav`、`vox run ask /tmp/a.wav`
async fn cmd_run(args: &[String]) -> Result<()> {
    let mode = Mode::parse(args.first().context("用法: vox run <dictation|translate|ask> <文件.wav>")?)
        .ok_or_else(|| anyhow::anyhow!("未知模式"))?;
    let file = args.get(1).context("用法: vox run <模式> <文件.wav>")?;
    let app = VoxApp::load()?;
    let target = app.config().general.translate_target.clone();
    let ctx = vox_app::build_context(mode, &target);

    let t0 = std::time::Instant::now();
    let outcome = app
        .process_file(mode, std::path::Path::new(file), ctx)
        .await?;
    eprintln!("（耗时 {:.2}s）", t0.elapsed().as_secs_f64());

    println!("--- 原始转写 ---\n{}", outcome.raw_text);
    if outcome.refined {
        println!("--- 最终结果 ---\n{}", outcome.final_text);
    }
    if let Some(action) = &outcome.action {
        println!("--- 外部动作 ---\n{action}");
    }
    Ok(())
}

async fn cmd_record(mode: Mode, args: &[String]) -> Result<()> {
    let app = VoxApp::load()?;
    let no_insert = flag(args, "--no-insert");

    let target = opt(args, "target")
        .unwrap_or_else(|| app.config().general.translate_target.clone());
    let ctx = vox_app::build_context(mode, &target);

    eprint!("录音中… 说完按回车停止。\n");
    let _ = std::io::stdout().flush();
    vox_macos::sound::play_start();

    let (audio_path, duration) = app.record_to_wav(
        Some(app.config().general.input_device.as_str()).filter(|d| !d.trim().is_empty()),
        || {},
        || {
            let mut line = String::new();
            let _ = std::io::stdin().lock().read_line(&mut line);
        },
    )?;
    vox_macos::sound::play_stop();

    eprintln!("录音 {duration:.1}s，正在识别…");
    let outcome = app.process_file(mode, &audio_path, ctx).await?;

    println!("--- 原始转写 ---");
    println!("{}", outcome.raw_text);
    if outcome.refined {
        println!("--- 最终结果 ---");
        println!("{}", outcome.final_text);
    }

    if !no_insert {
        if let Err(e) = app.insert(&outcome.final_text) {
            eprintln!("注入失败: {e}");
        } else {
            eprintln!("已插入到前台应用。");
        }
    }
    Ok(())
}

/// 把蓝牙耳机的链路推回 A2DP（立体声 44.1/48k）。
///
/// 录音时打开麦克风会把蓝牙链路从 A2DP 切到 HFP（单声道 16k，音质差），
/// 这是正常的；但**录完之后有时切不回来** —— 跑这个命令手动推一下。
fn cmd_audio_restore() -> Result<()> {
    match vox_macos::audio::output_is_mono_hfp() {
        Some(false) => {
            println!("当前输出设备已经是立体声（A2DP），音质正常，无需处理。");
        }
        Some(true) => {
            let acted = vox_macos::audio::restore_bluetooth_profile();
            if acted {
                println!("已重新激活默认输入设备，蓝牙耳机应已切回 A2DP 立体声。");
            } else {
                println!("系统拒绝了重新激活（不影响使用）。可试试在「系统设置 › 声音」里切一次输出设备。");
            }
        }
        None => println!("读不到默认输出设备（没有输出设备？）。"),
    }
    Ok(())
}

async fn cmd_hotkey() -> Result<()> {    let cfg = Config::load()?;

    // **全局热键只能有一个监听者**。
    //
    // 曾经有个遗留的 `vox hotkey` 调试进程和 Vox.app 同时跑了两天：
    // 两边都收到热键、都开麦录同一段话、都把结果插进光标
    // → 用户看到同一句话被插了两遍。
    // 抢这把锁，抢不到就直接退出（并说清楚是谁占着）。
    let _hotkey_lock = match vox_core::instance::acquire(vox_core::instance::HOTKEY_LOCK) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("无法监听全局热键：{e}");
            eprintln!("请先退出正在运行的 Vox.app 再试（否则两边会同时录音、同时插入，内容会重复）。");
            std::process::exit(1);
        }
    };

    let bindings = vec![
        (
            "dictation".to_string(),
            KeyCombo::parse(&cfg.general.hotkey_dictation)?,
        ),
        (
            "translate".to_string(),
            KeyCombo::parse(&cfg.general.hotkey_translate)?,
        ),
        ("ask".to_string(), KeyCombo::parse(&cfg.general.hotkey_ask)?),
    ];

    eprintln!("热键监听中（需要辅助功能权限）:");
    eprintln!("  听写 : {}", cfg.general.hotkey_dictation);
    eprintln!("  翻译 : {}", cfg.general.hotkey_translate);
    eprintln!("  Ask  : {}", cfg.general.hotkey_ask);
    eprintln!("按 Ctrl-C 退出。");

    let listener = HotkeyListener::start(bindings)?;
    let (tx, rx) = std::sync::mpsc::channel::<HotkeyEvent>();
    std::thread::spawn(move || {
        while let Some(ev) = listener.next() {
            if tx.send(ev).is_err() {
                break;
            }
        }
    });

    // 语义（`general.hotkey_mode` 决定）：
    // - toggle（默认）：组合键按下 → 开始录音；**再按一次同一个键** → 结束并处理；松开不结束
    // - push_to_talk：按住说话，全部松开 → 结束并处理
    // - 录音中加按另一个组合 → 切换模式，录音继续
    let hotkey_mode = cfg.general.hotkey_mode;
    // (当前模式, 录音器)
    let mut active: Option<(Mode, vox_audio::recorder::Recorder)> = None;

    while let Ok(ev) = rx.recv() {
        match ev {
            HotkeyEvent::ComboDown {
                feature,
                master_pressed,
            } => {
                eprintln!("[事件] ComboDown feature={feature} master_pressed={master_pressed}");
                let m = match feature.as_str() {
                    "dictation" => Mode::Dictation,
                    "translate" => Mode::Translate,
                    "ask" => Mode::AskAnything,
                    _ => continue,
                };

                match active.take() {
                    // 主控键（听写热键）被按下 = **统一的结束键**
                    Some((cur_mode, rec)) if master_pressed => {
                        eprintln!("■ {} 处理中…", cur_mode.label());
                        finish(rec, cur_mode).await;
                    }
                    // 换了个组合 = 切换模式，录音继续
                    Some((_cur_mode, rec)) => {
                        eprintln!("● 切换到 {}", m.label());
                        active = Some((m, rec));
                    }
                    None => match vox_audio::recorder::Recorder::start(
                        Some(cfg.general.input_device.as_str()).filter(|d| !d.trim().is_empty()),
                    ) {
                        Ok(r) => {
                            vox_macos::sound::play_start();
                            active = Some((m, r));
                            match hotkey_mode {
                                vox_core::config::HotkeyMode::Toggle => {
                                    eprintln!("▶ {} 录音中… 再按一次结束", m.label())
                                }
                                vox_core::config::HotkeyMode::PushToTalk => {
                                    eprintln!("▶ {} 录音中… 松开结束", m.label())
                                }
                            }
                        }
                        Err(e) => eprintln!("无法开始录音: {e}"),
                    },
                }
            }
            HotkeyEvent::ComboUp { .. } => {
                // 只有 push_to_talk 才「松开即结束」；toggle 模式下松开不结束
                if hotkey_mode != vox_core::config::HotkeyMode::PushToTalk {
                    continue;
                }
                if let Some((m, rec)) = active.take() {
                    eprintln!("■ {} 处理中…", m.label());
                    finish(rec, m).await;
                }
            }
        }
    }
    Ok(())
}

/// 停止录音并处理：写 WAV → ASR → LLM → 注入。
async fn finish(rec: vox_audio::recorder::Recorder, mode: Mode) {
    vox_macos::sound::play_stop();
    let recording = match rec.stop() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("停止录音失败: {e}");
            return;
        }
    };
    let target = recording.to_target_rate();
    // 麦克风已经释放：顺手把蓝牙耳机的音质推回 A2DP（卡在 HFP 时用）
    vox_macos::audio::restore_bluetooth_profile();
    let path = match vox_app::temp_audio_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("创建临时文件失败: {e}");
            return;
        }
    };
    if let Err(e) = vox_audio::wav::write_wav(&path, &target.samples, target.sample_rate) {
        eprintln!("写 WAV 失败: {e}");
        return;
    }

    let app = match VoxApp::load() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("加载配置失败: {e}");
            return;
        }
    };
    let ctx = PromptContext {
        target_language: Some(app.config().general.translate_target.clone()),
        app_name: app.frontmost_app_name(),
        ..Default::default()
    };
    match app.process_file(mode, &path, ctx).await {
        Ok(o) => {
            println!("{}", o.final_text);
            if let Err(e) = app.insert(&o.final_text) {
                eprintln!("注入失败: {e}");
            }
        }
        Err(e) => eprintln!("处理失败: {e}"),
    }
}

fn cmd_history(args: &[String]) -> Result<()> {
    let limit: usize = opt(args, "limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let store = vox_app::open_store()?;
    let items = store.list_history(limit)?;
    if items.is_empty() {
        println!("（暂无历史）");
        return Ok(());
    }
    for h in items {
        println!("[{}] {}", h.created_at, h.mode);
        println!("  {}", h.refined_text.replace('\n', " "));
    }
    Ok(())
}

fn cmd_dict(args: &[String]) -> Result<()> {
    let store = vox_app::open_store()?;
    match args.first().map(|s| s.as_str()) {
        Some("list") | None => {
            for e in store.list_dictionary()? {
                let repl = if e.replacement.is_empty() {
                    String::new()
                } else {
                    format!(" → {}", e.replacement)
                };
                println!("{:>4}  {}{}", e.id, e.term, repl);
            }
        }
        Some("add") => {
            let term = args.get(1).context("用法: vox dict add <词> [替换为]")?;
            let replacement = args.get(2).cloned().unwrap_or_default();
            store.upsert_dictionary(term, &replacement, false)?;
            println!("已添加: {term}");
        }
        Some("rm") => {
            let id: i64 = args
                .get(1)
                .context("用法: vox dict rm <id>")?
                .parse()
                .context("id 必须是数字")?;
            store.delete_dictionary(id)?;
            println!("已删除 {id}");
        }
        Some("import") => {
            let path = args.get(1).context("用法: vox dict import <csv文件>")?;
            let content = std::fs::read_to_string(path)?;
            let words = store.import_dictionary_csv(&content)?;
            println!("已导入 {} 个词条", words.len());
        }
        Some(other) => bail!("未知 dict 子命令: {other}"),
    }
    Ok(())
}

/// 密钥管理：加密存本地文件，不进系统钥匙串。
fn cmd_secret(args: &[String]) -> Result<()> {
    match args.first().map(|s| s.as_str()) {
        Some("list") | None => {
            println!("密钥文件: {}", vox_core::secrets::secrets_file_path().display());
            let accounts = vox_core::secrets::list()?;
            if accounts.is_empty() {
                println!("（还没有存任何密钥）");
            } else {
                for a in accounts {
                    println!("  • {a}");
                }
            }
        }
        Some("set") => {
            let kind = args.get(1).context("用法: vox secret set <stt|llm>（从标准输入读）")?;
            let account = match kind.as_str() {
                "stt" => "vox.stt",
                "llm" => "vox.llm",
                other => bail!("未知密钥类型: {other}（可选 stt / llm）"),
            };
            // 从 stdin 读，避免密钥进 shell history / ps 输出
            print!("粘贴 {account} 的密钥后回车: ");
            std::io::stdout().flush()?;
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            let key = line.trim();
            if key.is_empty() {
                bail!("没读到内容，已取消");
            }
            vox_core::secrets::store(account, key)?;
            println!("已加密保存到 {}", vox_core::secrets::secrets_file_path().display());
        }
        Some("migrate") => {
            let migrated = vox_core::secrets::migrate_from_keychain(&["vox.stt", "vox.llm"])?;
            if migrated.is_empty() {
                println!("没有需要迁移的密钥");
            } else {
                println!(
                    "已迁移 {} 个密钥到加密文件并删除钥匙串副本: {}",
                    migrated.len(),
                    migrated.join(", ")
                );
            }
        }
        Some(other) => bail!("未知 secret 子命令: {other}"),
    }
    Ok(())
}

/// Prompt 管理：查看 / 恢复默认 / 从文件导入。
fn cmd_prompt(args: &[String]) -> Result<()> {
    let mut cfg = Config::load()?;
    match args.first().map(|s| s.as_str()) {
        Some("show") | None => {
            println!("── 听写 ──\n{}\n", cfg.prompts.dictation);
            println!("── 翻译 ──\n{}\n", cfg.prompts.translate);
            println!("── Ask Anything ──\n{}", cfg.prompts.ask_anything);
        }
        Some("reset") => {
            cfg.prompts = PromptsConfig::default();
            cfg.save()?;
            println!("已把三个 prompt 恢复为内置默认值");
        }
        Some("set") => {
            let mode = args.get(1).context("用法: vox prompt set <dictation|translate|ask> <文件>")?;
            let path = args.get(2).context("用法: vox prompt set <dictation|translate|ask> <文件>")?;
            let content = std::fs::read_to_string(path)?;
            match mode.as_str() {
                "dictation" => cfg.prompts.dictation = content,
                "translate" => cfg.prompts.translate = content,
                "ask" | "ask_anything" => cfg.prompts.ask_anything = content,
                other => bail!("未知模式: {other}（可选 dictation / translate / ask）"),
            }
            cfg.save()?;
            println!("已更新 {mode} 的 prompt（来自 {path}）");
        }
        Some(other) => bail!("未知 prompt 子命令: {other}"),
    }
    Ok(())
}
