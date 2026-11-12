//! Vox 桌面壳（Tauri v2）。
//!
//! 这里只做「把 Rust 能力暴露给前端」：配置读写、密钥管理、历史与词典 CRUD、
//! 麦克风列表、录音起停、菜单栏常驻、全局热键。所有业务逻辑都在 `vox-*` crate 里。

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use vox_app::{RecordingSession, VoxApp};
use vox_core::config::{Config, HotkeyMode};
use vox_core::mode::Mode;
use vox_macos::hotkey::{HotkeyEvent, HotkeyListener, KeyCombo};

pub struct AppState {
    session: Mutex<Option<Arc<RecordingSession>>>,
    /// 当前正在录音的会话。Some 表示正在录。
    recording: Mutex<Option<RecordingState>>,
    /// **会话阶段**（空闲 / 录音中 / 处理中）—— 判断「能不能开新录音」的**唯一真相源**。
    ///
    /// 以前只用 `recording.is_some()` 判断，结果在「结果还在算」的那几秒里按热键
    /// 会凭空开一段新录音：浮条卡在「录音中」再也结束不了，而且多出一段空录音。
    phase: Mutex<SessionPhase>,
    /// 运行时健康状态：麦克风列表、全局热键监听是否正常。
    health: Mutex<Health>,
    /// 悬浮条显示的状态（唯一真相源）。
    float: Mutex<FloatState>,
}

/// 会话阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionPhase {
    /// 空闲：按热键 = 开始一段新录音。
    #[default]
    Idle,
    /// 录音中：主控键 = 结束；其它组合 = 切换模式。
    Recording,
    /// 处理中（ASR/LLM）：**忽略热键**，避免凭空开新录音。
    Processing,
}

/// 正在进行的录音会话。
#[derive(Debug, Clone)]
pub struct RecordingState {
    /// 当前模式（中途加按别的组合会切换）。
    pub mode: Mode,
    /// 当前生效的功能。
    pub feature: String,
    /// **开始**这次录音的功能 —— 再按一次它就结束录音。
    ///
    /// 单独记这个是因为：中途切了模式之后，当前功能可能已经不是开始时的那个了，
    /// 但「按一次开始、再按一次结束」应该认开始时的那个键。
    pub start_feature: String,
    /// 本次录音的唯一 id。悬浮条靠它判断「是不是新的一次录音」来决定要不要重置计时。
    pub session: String,
}

/// 悬浮条要显示的状态。
///
/// 这是**唯一真相源** —— 悬浮条页面每 250ms 轮询 `float_state`，
/// 事件只用来做即时推送。这样即使事件丢了（页面还没 ready），
/// 状态也不会停在旧值（比如计时器一直涨）。
#[derive(Debug, Clone, Default)]
pub struct FloatState {
    /// 正在录音的模式（None = 没在录）。
    pub mode: Option<String>,
    /// 本次录音的唯一 id（用来判断是不是新的一次，计时器要不要归零）。
    pub session: Option<String>,
    /// idle / recording / processing / done
    pub phase: FloatPhase,
    /// 完成后的结果文本。
    pub text: String,
    /// 结果是否已插入到焦点应用。
    pub inserted: bool,
    /// 模型要求的外部动作（打开网址/应用）。
    pub action: String,
    /// 模型决定的投递方式：reply / insert / replace（面板上显示用）。
    pub delivery: String,
    /// 用户问的是什么（原始转写）—— 回答面板上显示，方便对照。
    pub question: String,
    /// 覆盖默认提示语（例如「已松开 · 再按一次 RightCtrl 结束」）。
    pub hint: String,
}

/// 悬浮条的状态机。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FloatPhase {
    /// 空闲
    #[default]
    Idle,
    /// 录音中
    Recording,
    /// 处理中（ASR/LLM）
    Processing,
    /// 没能插入到光标处 → 提示「已复制到剪贴板」，1~2 秒后自动消失
    Copied,
    /// Ask Anything 的回答面板（显示答案 + 复制 + 可选的「打开」动作）
    Reply,
    /// 出错了（麦克风不可用等）—— 显示原因，不让浮条凭空消失
    Error,
}

impl FloatPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            FloatPhase::Idle => "idle",
            FloatPhase::Recording => "recording",
            FloatPhase::Processing => "processing",
            FloatPhase::Copied => "copied",
            FloatPhase::Reply => "reply",
            FloatPhase::Error => "error",
        }
    }
}

/// 运行时健康状态。
#[derive(Debug, Clone, Default)]
pub struct Health {
    /// 可用的输入设备名（空 = 没有麦克风）。
    pub mics: Vec<String>,
    /// 热键监听失败的原因（None = 正常）。
    pub hotkey_error: Option<String>,
    /// **全局热键功能上是否可用** —— 以「event tap 建成没有」为准。
    ///
    /// 这才是用户真正关心的：热键能不能响应、⌘V 能不能注入。
    /// `AXIsProcessTrusted()` 只是一次系统判定，可能与实际不符
    /// （比如 app 从终端启动时判定为否，但 tap 照样能收到事件），
    /// 所以**不用它来驱动提示**，只作参考信息展示。
    pub hotkey_ok: bool,
    /// macOS 的 `AXIsProcessTrusted()` 判定（仅展示用）。
    pub ax_trusted: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            session: Mutex::new(None),
            recording: Mutex::new(None),
            phase: Mutex::new(SessionPhase::Idle),
            health: Mutex::new(Health {
                mics: vec![],
                hotkey_error: None,
                hotkey_ok: false,
                ax_trusted: false,
            }),
            float: Mutex::new(FloatState::default()),
        }
    }
}

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// 进程启动以来的秒数（给日志打时间戳，排查事件顺序用）。
fn ts() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

// ---------------- 配置 ----------------

#[tauri::command]
fn get_config() -> Result<Value, String> {
    let cfg = Config::load().map_err(err)?;
    Ok(cfg.redacted())
}

#[tauri::command]
fn save_config(value: Value) -> Result<Value, String> {
    let cfg: Config = serde_json::from_value(value).map_err(err)?;
    cfg.save().map_err(err)?;
    Ok(cfg.redacted())
}

#[tauri::command]
fn config_path() -> String {
    Config::path().to_string_lossy().to_string()
}

// ---------------- 密钥 ----------------

#[tauri::command]
fn key_status() -> Result<Value, String> {
    let cfg = Config::load().map_err(err)?;
    Ok(json!({
        "stt": cfg.stt.api_key().is_ok(),
        "llm": cfg.llm.api_key().is_ok(),
        "stt_ref": cfg.stt.api_key_ref,
        "llm_ref": cfg.llm.api_key_ref,
    }))
}

/// 密钥文件位置与条目（设置界面展示用）。
#[tauri::command]
fn secrets_info() -> Result<Value, String> {
    let accounts = vox_core::secrets::list().map_err(err)?;
    Ok(json!({
        "path": vox_core::secrets::secrets_file_path().to_string_lossy(),
        "accounts": accounts,
    }))
}

/// 把密钥写进系统钥匙串。`kind` 为 "stt" 或 "llm"。
#[tauri::command]
fn set_key(kind: String, key: String) -> Result<(), String> {
    let account = match kind.as_str() {
        "stt" => "vox.stt",
        "llm" => "vox.llm",
        other => return Err(format!("未知的密钥类型: {other}")),
    };
    vox_core::secrets::store(account, key.trim()).map_err(err)
}

// ---------------- 历史 ----------------

#[tauri::command]
fn list_history(limit: Option<usize>) -> Result<Value, String> {
    let store = vox_app::open_store().map_err(err)?;
    let items = store.list_history(limit.unwrap_or(100)).map_err(err)?;
    Ok(serde_json::to_value(items).map_err(err)?)
}

#[tauri::command]
fn delete_history(id: String) -> Result<(), String> {
    let store = vox_app::open_store().map_err(err)?;
    store.delete_history(&id).map_err(err)
}

#[tauri::command]
fn clear_history() -> Result<usize, String> {
    let store = vox_app::open_store().map_err(err)?;
    let items = store.list_history(10_000).map_err(err)?;
    let mut n = 0usize;
    for it in items {
        store.delete_history(&it.id).map_err(err)?;
        n += 1;
    }
    Ok(n)
}

// ---------------- 词典 ----------------

#[tauri::command]
fn dict_list() -> Result<Value, String> {
    let store = vox_app::open_store().map_err(err)?;
    let items = store.list_dictionary().map_err(err)?;
    Ok(serde_json::to_value(items).map_err(err)?)
}

#[tauri::command]
fn dict_upsert(term: String, replacement: String) -> Result<Value, String> {
    let store = vox_app::open_store().map_err(err)?;
    let e = store
        .upsert_dictionary(term.trim(), replacement.trim(), false)
        .map_err(err)?;
    Ok(serde_json::to_value(e).map_err(err)?)
}

#[tauri::command]
fn dict_delete(id: i64) -> Result<(), String> {
    let store = vox_app::open_store().map_err(err)?;
    store.delete_dictionary(id).map_err(err)
}

/// 从单列 CSV 文本导入词条（单列、无表头）。
#[tauri::command]
fn dict_import(content: String) -> Result<Value, String> {
    let store = vox_app::open_store().map_err(err)?;
    let words = store.import_dictionary_csv(&content).map_err(err)?;
    Ok(json!({ "count": words.len() }))
}

// ---------------- 设备 / 应用 ----------------

#[tauri::command]
fn list_mics() -> Result<Value, String> {
    let list = vox_app::list_input_devices().map_err(err)?;
    Ok(serde_json::to_value(list).map_err(err)?)
}

/// 打开系统设置的对应隐私面板，让用户一键授权。
///
/// `kind`：`accessibility`（辅助功能）/ `microphone`（麦克风）。
#[tauri::command]
fn open_privacy_settings(kind: String) -> Result<(), String> {
    use std::process::Command;

    let anchor = match kind.as_str() {
        "microphone" => "Privacy_Microphone",
        "siri" => "Privacy_Siri",
        _ => "Privacy_Accessibility",
    };
    let url = format!(
        "x-apple.systempreferences:com.apple.Preferences.PrivacySecurity.extension?{anchor}"
    );
    Command::new("open")
        .arg(&url)
        .spawn()
        .map_err(|e| format!("打开系统设置失败: {e}"))?;
    Ok(())
}

/// 把文本复制到剪贴板（浮条上的「复制」按钮用）。
#[tauri::command]
fn copy_to_clipboard(text: String) -> Result<(), String> {
    vox_macos::write_clipboard(&text).map_err(err)
}

/// 执行「外部动作」：打开网址或本机应用。
///
/// 模型的「外部动作」：打开某个网址或应用。
/// 用 `Command::new("open")` 直接传参（不经 shell），所以没有注入风险；
/// 另外对应用名做了字符白名单校验。
#[tauri::command]
fn open_action(target: String) -> Result<(), String> {
    do_open_action(&target)
}

/// 「打开」的真正实现（命令和处理流水线共用）。
fn do_open_action(target: &str) -> Result<(), String> {
    use std::process::Command;

    let t = target.trim();
    if t.is_empty() {
        return Err("动作目标为空".into());
    }
    if t.starts_with("https://") || t.starts_with("http://") {
        Command::new("open")
            .arg(t)
            .spawn()
            .map_err(|e| format!("打开失败: {e}"))?;
        return Ok(());
    }
    // 应用名：只允许安全的字符（避免把奇怪的东西当参数传给 open）
    if !t.chars().all(|c| c.is_alphanumeric() || " .-_".contains(c)) {
        return Err("不支持的动作目标".into());
    }
    Command::new("open")
        .arg("-a")
        .arg(t)
        .spawn()
        .map_err(|e| format!("打开应用失败: {e}"))?;
    Ok(())
}

/// 把文本插入到当前焦点应用的光标处（浮窗回答面板上的「插入」按钮用）。
#[tauri::command]
fn insert_text(text: String) -> Result<(), String> {
    let cfg = Config::load().map_err(err)?;
    let restore = !cfg.general.keep_result_in_clipboard;
    vox_macos::insert_text(&text, cfg.general.insert_method, restore).map_err(err)
}

#[tauri::command]
fn frontmost_app() -> Option<String> {
    vox_macos::inject::frontmost_app_name()
}

#[tauri::command]
fn selected_text() -> Option<String> {
    vox_macos::selected_text()
}

// ---------------- 开机自启 ----------------

#[tauri::command]
fn set_launch_at_login(enabled: bool) -> Result<(), String> {
    vox_macos::set_launch_at_login(enabled).map_err(err)
}

#[tauri::command]
fn launch_at_login_status() -> bool {
    vox_macos::is_launch_at_login()
}

// ---------------- 悬浮条 ----------------

/// 当前悬浮条状态（悬浮条页面靠它对齐，唯一真相源）。
#[tauri::command]
fn float_state(app: AppHandle) -> Value {
    app.try_state::<AppState>()
        .and_then(|s| s.float.lock().ok().map(|f| float_json(&f)))
        .unwrap_or_else(|| float_json(&FloatState::default()))
}

/// 把 FloatState 序列化成给前端看的 JSON。
fn float_json(f: &FloatState) -> Value {
    json!({
        "mode": f.mode,
        "session": f.session,
        "phase": f.phase.as_str(),
        "text": f.text,
        "inserted": f.inserted,
        "action": f.action,
        "delivery": f.delivery,
        "question": f.question,
        "hint": f.hint,
    })
}

/// 把当前悬浮条状态推给悬浮条（事件只做即时推送，状态以轮询为准）。
fn emit_float_state(app: &AppHandle) {
    let payload = app
        .try_state::<AppState>()
        .and_then(|s| s.float.lock().ok().map(|f| float_json(&f)));
    if let Some(p) = payload {
        if let Some(w) = app.get_webview_window("float") {
            let _ = w.emit("vox://float-mode", p);
        }
    }
}

/// 开不了录时，让浮条**显示原因**（而不是凭空消失）。
fn show_float_error(app: &AppHandle, msg: &str) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut g) = state.float.lock() {
            g.phase = FloatPhase::Error;
            g.text = msg.to_string();
        }
    }
    emit_float_state(app);
    layout_float(app, false);
    if let Some(w) = app.get_webview_window("float") {
        let _ = w.show();
        let _ = w.set_focus();
    }
    // 错误提示不能永远挂在屏幕上：后端兜底 6 秒后收起
    schedule_float_autohide(app, 6, FloatPhase::Error);
}

/// 让浮条在 `secs` 秒后自动收起。
///
/// **后端兜底**：前端也有定时器，但前端页面一旦没加载/被重载，定时器就没了，
/// 浮条会永远停在那里 —— 这正是「结束了但悬浮窗不结束」那个 bug 的来源。
///
/// `expect` 是预期被收起的阶段：期间状态变了（比如用户又开了一段录音）就不动它。
fn schedule_float_autohide(app: &AppHandle, secs: u64, expect: FloatPhase) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(secs));
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        {
            let Ok(mut g) = state.float.lock() else {
                return;
            };
            if g.phase != expect {
                return; // 状态已经变了（用户在做别的事），不管
            }
            g.phase = FloatPhase::Idle;
        }
        emit_float_state(&app);
        if let Some(w) = app.get_webview_window("float") {
            let _ = w.hide();
        }
    });
}

/// 收起浮条（回到空闲态）。
fn hide_float_now(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut g) = state.float.lock() {
            g.phase = FloatPhase::Idle;
        }
    }
    emit_float_state(app);
    if let Some(w) = app.get_webview_window("float") {
        let _ = w.hide();
    }
}

/// 当前会话阶段。
fn session_phase(app: &AppHandle) -> SessionPhase {
    app.try_state::<AppState>()
        .and_then(|s| s.phase.lock().ok().map(|p| *p))
        .unwrap_or(SessionPhase::Idle)
}

/// 设置会话阶段。
fn set_session_phase(app: &AppHandle, p: SessionPhase) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut g) = state.phase.lock() {
            *g = p;
        }
    }
}

/// 调整悬浮窗的尺寸与位置。
///
/// - 录音/处理中：一条**紧凑的横条**（贴着主屏底部居中，不挡视线）
/// - 回答面板：一块**大一点的卡片**，高度由内容决定（前端量好再调 [`set_float_height`]）
fn layout_float(app: &AppHandle, tall: bool) {
    let (width, height) = if tall {
        (620.0, 440.0)
    } else {
        (460.0, 92.0)
    };
    layout_float_size(app, width, height);
}

/// 按给定的宽高调整悬浮窗，并摆到主屏底部居中。
fn layout_float_size(app: &AppHandle, width: f64, height: f64) {
    use tauri::{LogicalPosition, LogicalSize};

    let Some(w) = app.get_webview_window("float") else {
        return;
    };
    let _ = w.set_size(LogicalSize::new(width, height));

    // 摆到主屏底部居中（跟随主屏，不是外接屏）
    if let Ok(Some(monitor)) = w.current_monitor() {
        let scale = monitor.scale_factor();
        let screen_w = monitor.size().width as f64 / scale;
        let screen_h = monitor.size().height as f64 / scale;
        let x = ((screen_w - width) / 2.0).max(0.0);
        let y = (screen_h - height - 48.0).max(0.0);
        let _ = w.set_position(LogicalPosition::new(x, y));
    }
}

/// 让回答面板**按内容高度**收缩（前端渲染完量好 scrollHeight 再调）。
///
/// 短回答不该撑出一个巨大的空盒子。
#[tauri::command]
fn set_float_height(app: AppHandle, height: f64) -> Result<(), String> {
    let h = height.clamp(150.0, 460.0);
    layout_float_size(&app, 620.0, h);
    Ok(())
}

#[tauri::command]
fn show_float(app: AppHandle, mode: String) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("float") {
        let _ = w.show();
        let _ = w.emit("vox://float-mode", mode);
    }
    Ok(())
}

#[tauri::command]
fn hide_float(app: AppHandle) -> Result<(), String> {
    // 状态也要一起归位，否则前端轮询到的还是旧状态
    hide_float_now(&app);
    Ok(())
}

// ---------------- 录音 ----------------

/// 刷新健康状态（麦克风、系统对辅助功能的判定），并同步托盘提示。
fn refresh_mics(app: &AppHandle) {
    let mics = vox_app::list_input_devices().unwrap_or_default();
    let ax_trusted = vox_macos::inject::has_accessibility_permission();
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut h) = state.health.lock() {
            h.mics = mics.clone();
            h.ax_trusted = ax_trusted;
        }
    }
    // 打一行启动诊断，排查「热键没反应」时一眼能看出是不是权限掉了
    eprintln!(
        "Vox 启动：系统判定辅助功能={} 麦克风={}",
        if ax_trusted { "已授予" } else { "未授予" },
        if mics.is_empty() {
            "无".to_string()
        } else {
            mics.join("、")
        }
    );
    update_tray_tooltip(app);
}

/// 按当前健康状态更新托盘提示（有问题时直接写进 tooltip，不用点开窗口）。
fn update_tray_tooltip(app: &AppHandle) {
    let (mics_ok, hotkey_err, hotkey_ok) = app
        .try_state::<AppState>()
        .and_then(|s| {
            s.health.lock().ok().map(|h| {
                (!h.mics.is_empty(), h.hotkey_error.clone(), h.hotkey_ok)
            })
        })
        .unwrap_or((true, None, false));

    let tip = match (&hotkey_err, hotkey_ok, mics_ok) {
        (Some(e), _, _) => format!("Vox · ⚠️ 热键不可用：{e}"),
        (None, false, _) => "Vox · ⚠️ 全局热键不可用（缺辅助功能权限？）".to_string(),
        (None, true, false) => "Vox · ⚠️ 未检测到麦克风".to_string(),
        (None, true, true) => "Vox".to_string(),
    };
    if let Some(tray) = app.tray_by_id("vox-tray") {
        let _ = tray.set_tooltip(Some(tip));
    }
}

/// 应用整体健康状态（供设置界面显示，别让用户「一切正常」地面对一堆故障）。
///
/// **每次调用都重新查**设备与系统判定，不用启动时的缓存 ——
/// 否则「重新检测」按钮读的还是同一份陈旧数据，按多少次都没用。
#[tauri::command]
fn app_status(app: AppHandle) -> Value {
    let cfg = Config::load().ok();

    // 实时查设备与系统判定
    let mics = vox_app::list_input_devices().unwrap_or_default();
    let ax_trusted = vox_macos::inject::has_accessibility_permission();

    // 热键能不能用，看 event tap 建成没有（功能事实），而不是 AX 判定
    let (hotkey_error, hotkey_ok) = app
        .try_state::<AppState>()
        .and_then(|s| {
            s.health
                .lock()
                .ok()
                .map(|h| (h.hotkey_error.clone(), h.hotkey_ok))
        })
        .unwrap_or((None, false));

    // 顺手把缓存更新掉
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut h) = state.health.lock() {
            h.mics = mics.clone();
            h.ax_trusted = ax_trusted;
        }
    }

    let mut problems: Vec<String> = Vec::new();
    if !hotkey_ok {
        problems.push(
            "全局热键不可用（需要辅助功能权限：请到 系统设置 › 隐私与安全性 › 辅助功能 勾选 Vox）"
                .into(),
        );
    }
    if mics.is_empty() {
        problems.push("没有检测到麦克风".into());
    }
    if let Some(e) = &hotkey_error {
        problems.push(format!("全局热键不可用：{e}"));
    }
    if let Some(cfg) = &cfg {
        if cfg.stt.api_key().is_err() {
            problems.push("ASR 密钥未配置".into());
        }
        if cfg.llm.api_key().is_err() {
            problems.push("LLM 密钥未配置".into());
        }
    }

    json!({
        "mics": mics,
        "hotkey_error": hotkey_error,
        "hotkey_ok": hotkey_ok,
        "accessibility_ok": ax_trusted,
        "stt_ok": cfg.as_ref().map(|c| c.stt.api_key().is_ok()).unwrap_or(false),
        "llm_ok": cfg.as_ref().map(|c| c.llm.api_key().is_ok()).unwrap_or(false),
        "ready": problems.is_empty(),
        "problems": problems,
    })
}

/// 开始录音（后台线程采集 + 异步处理）。完成时 emit `vox://result`。
fn spawn_recording(app: AppHandle, mode: Mode, feature: String) -> Result<(), String> {
    let cfg = Config::load().map_err(err)?;

    // 用设置里选定的输入设备（空 = 系统默认）
    let device = cfg.general.input_device.trim().to_string();
    let device = if device.is_empty() {
        None
    } else {
        Some(device.as_str())
    };

    // 开录前先确认真的有麦克风 —— 没有就别闪浮条，直接把原因报出去
    let mics = vox_app::list_input_devices().map_err(err)?;
    if mics.is_empty() {
        return Err("未检测到麦克风，无法录音（请连接输入设备后重试）".into());
    }
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut h) = state.health.lock() {
            h.mics = mics;
        }
    }

    // 「语音输入时静音」：录之前把系统默认输出设备静音，录完还原
    let mute_guard = if cfg.general.mute_audio_while_dictating {
        vox_macos::audio::MuteGuard::mute().ok()
    } else {
        None
    };

    let session = Arc::new(RecordingSession::start(device).map_err(err)?);
    // **不要留下孤儿录音器**：如果上一个会话还开着麦克风，先让它停掉。
    // 否则会同时有两个采集线程开麦（内容重复 + 麦克风一直被占着，
    // 蓝牙耳机卡在 HFP，音质恢复不了）。
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(guard) = state.session.lock() {
            if let Some(old) = guard.as_ref() {
                old.request_stop();
            }
        }
    }
    // 开始录音的提示音（「交互声音」设置）
    if cfg.general.interaction_sounds {
        vox_macos::sound::play_start();
    }

    let session_id = uuid::Uuid::new_v4().to_string();
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut guard) = state.session.lock() {
            *guard = Some(Arc::clone(&session));
        }
        if let Ok(mut guard) = state.recording.lock() {
            *guard = Some(RecordingState {
                mode,
                feature: feature.clone(),
                start_feature: feature,
                session: session_id.clone(),
            });
        }
        if let Ok(mut guard) = state.float.lock() {
            *guard = FloatState {
                mode: Some(mode.as_str().to_string()),
                session: Some(session_id.clone()),
                phase: FloatPhase::Recording,
                text: String::new(),
                inserted: false,
                action: String::new(),
                delivery: String::new(),
                question: String::new(),
                hint: String::new(),
            };
        }
        if let Ok(mut guard) = state.phase.lock() {
            *guard = SessionPhase::Recording;
        }
    }

    // 显示悬浮条（录音状态 + 计时）
    layout_float(&app, false);
    if let Some(w) = app.get_webview_window("float") {
        let _ = w.show();
    }
    emit_float_state(&app);

    let handle = app.clone();
    std::thread::spawn(move || {
        let result = session.wait();
        // 录音结束 → 立刻还原被静音的输出设备（不等 ASR/LLM）
        drop(mute_guard);
        // 麦克风已经释放了：把蓝牙耳机的链路推回 A2DP（立体声 44.1/48k）。
        // 否则有时会卡在 HFP（单声道 16k），表现是「都录完了，音质还是差的」。
        vox_macos::audio::restore_bluetooth_profile();
        eprintln!("[{:7.3}] 录音结束，开始处理", ts());
        // 结束录音的提示音（「交互声音」设置）
        if Config::load().map(|c| c.general.interaction_sounds).unwrap_or(true) {
            vox_macos::sound::play_stop();
        }
        // 浮条切到「处理中」
        if let Some(state) = handle.try_state::<AppState>() {
            if let Ok(mut g) = state.float.lock() {
                if g.phase == FloatPhase::Recording {
                    g.phase = FloatPhase::Processing;
                }
            }
            if let Ok(mut g) = state.phase.lock() {
                *g = SessionPhase::Processing;
            }
        }
        emit_float_state(&handle);
        if let Some(w) = handle.get_webview_window("float") {
            let _ = w.show();
        }
        // 模式可能在录音过程中被热键切换过，这里取**最终**模式
        let final_mode = handle
            .try_state::<AppState>()
            .and_then(|s| s.recording.lock().ok().and_then(|g| g.as_ref().map(|r| r.mode)))
            .unwrap_or(mode);
        if let Some(state) = handle.try_state::<AppState>() {
            if let Ok(mut guard) = state.session.lock() {
                *guard = None;
            }
            if let Ok(mut guard) = state.recording.lock() {
                *guard = None;
            }
        }
        match result {
            Ok((path, duration)) => {
                match tauri::async_runtime::block_on(process(&handle, final_mode, path, duration)) {
                    Ok(v) => {
                        // 按**投递方式**分叉（投递方式由模型自己决定）：
                        // - reply  → 显示回答面板
                        // - insert/replace → 写进焦点应用；成功 → 浮条立刻消失
                        // - 写不进去 → 短窗提示「已自动复制到剪贴板」
                        let text = v
                            .get("final_text")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let inserted = v
                            .get("inserted")
                            .and_then(|x| x.as_bool())
                            .unwrap_or(false);
                        let action = v
                            .get("action")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let delivery = v
                            .get("delivery")
                            .and_then(|x| x.as_str())
                            .unwrap_or("reply")
                            .to_string();
                        let question = v
                            .get("raw_text")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let wants_panel = delivery == "reply";
                        // LLM 失败 → **明确告诉用户**，别让他以为「我问什么它就答什么」
                        let llm_error = v
                            .get("llm_error")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();

                        let phase = if !llm_error.is_empty() {
                            FloatPhase::Error
                        } else if wants_panel && !text.trim().is_empty() {
                            FloatPhase::Reply
                        } else if text.trim().is_empty() || inserted {
                            FloatPhase::Idle
                        } else {
                            FloatPhase::Copied
                        };

                        if let Some(state) = handle.try_state::<AppState>() {
                            if let Ok(mut g) = state.float.lock() {
                                g.phase = phase;
                                // 失败时把原因显示出来（结果仍已插入，别让用户白说一遍）
                                g.text = if llm_error.is_empty() {
                                    text
                                } else {
                                    format!("处理失败：{llm_error}（已插入原始转写）")
                                };
                                g.inserted = inserted;
                                g.action = action.clone();
                                g.delivery = delivery;
                                g.question = question;
                            }
                        }
                        emit_float_state(&handle);
                        match phase {
                            FloatPhase::Idle => {
                                layout_float(&handle, false);
                                if let Some(w) = handle.get_webview_window("float") {
                                    let _ = w.hide();
                                }
                            }
                            FloatPhase::Reply => {
                                layout_float(&handle, true);
                                if let Some(w) = handle.get_webview_window("float") {
                                    let _ = w.show();
                                }
                                // 回答面板最多停留 90 秒，之后自动收起
                                // （不然它会一直挂在屏幕上，用户以为「浮窗不会结束」）
                                schedule_float_autohide(&handle, 90, FloatPhase::Reply);
                            }
                            _ => {
                                layout_float(&handle, false);
                                if let Some(w) = handle.get_webview_window("float") {
                                    let _ = w.show();
                                }
                                schedule_float_autohide(&handle, 2, FloatPhase::Copied);
                            }
                        }
                        // 模型要求打开某个网站/应用 → **直接打开**
                        // （用户明确要求「帮我打开 B 站」时）
                        if !action.trim().is_empty() {
                            if let Err(e) = do_open_action(&action) {
                                eprintln!("打开外部动作失败：{e}");
                            }
                        }
                        let _ = handle.emit("vox://result", v);
                    }
                    Err(e) => {
                        let msg = e.clone();
                        if let Some(state) = handle.try_state::<AppState>() {
                            if let Ok(mut g) = state.float.lock() {
                                g.phase = FloatPhase::Error;
                                g.text = msg.clone();
                            }
                        }
                        emit_float_state(&handle);
                        if let Some(w) = handle.get_webview_window("float") {
                            let _ = w.show();
                        }
                        schedule_float_autohide(&handle, 6, FloatPhase::Error);
                        let _ = handle.emit("vox://error", e);
                    }
                }
            }
            Err(e) => {
                let msg = err(e);
                if let Some(state) = handle.try_state::<AppState>() {
                    if let Ok(mut g) = state.float.lock() {
                        g.phase = FloatPhase::Error;
                        g.text = msg.clone();
                    }
                }
                emit_float_state(&handle);
                if let Some(w) = handle.get_webview_window("float") {
                    let _ = w.show();
                }
                schedule_float_autohide(&handle, 6, FloatPhase::Error);
                let _ = handle.emit("vox://error", msg);
            }
        }
        // 收尾：会话回到空闲（放在最后，保证上面的浮条状态不会被覆盖）
        set_session_phase(&handle, SessionPhase::Idle);
    });

    Ok(())
}

/// 停止录音。
fn stop_recording_internal(app: &AppHandle) -> Result<(), String> {
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(guard) = state.session.lock() {
            if let Some(s) = guard.as_ref() {
                s.request_stop();
            }
        }
    }
    Ok(())
}

/// 开始录音（前端按钮）。
#[tauri::command]
fn start_recording(app: AppHandle, mode: String) -> Result<(), String> {
    let m = Mode::parse(&mode).ok_or_else(|| format!("未知模式: {mode}"))?;
    spawn_recording(app, m, feature_of(m).to_string())
}

/// 停止录音（前端按钮）。
#[tauri::command]
fn stop_recording(app: AppHandle) -> Result<(), String> {
    stop_recording_internal(&app)
}

/// 处理一次录音：ASR → LLM → 词典 → 落历史。
async fn process(
    app: &AppHandle,
    mode: Mode,
    path: std::path::PathBuf,
    duration: f64,
) -> Result<Value, String> {
    let vox = VoxApp::load().map_err(err)?;
    let cfg = vox.config().clone();
    let ctx = vox_app::build_context(mode, &cfg.general.translate_target);

    // 落历史（带时长）。
    //
    // **只写一条**：`VoxApp::process_file_with_duration` 已经写过了，
    // 这里再写一次的话历史列表里每条都会重复一遍。
    let outcome = vox
        .process_file_with_duration(mode, &path, ctx, Some(duration))
        .await
        .map_err(err)?;

    // 投递策略（三态）：
    // - **听写 / 翻译** → 恒定插入到光标处。成功 → 浮条立刻收起；
    //   失败 → 短窗提示「已自动复制到剪贴板」。
    // - **Ask Anything** → **由模型自己决定**（`[[deliver: reply|insert|replace]]`）：
    //   reply = 显示在浮窗面板里（默认）；insert/replace = 写进焦点应用。
    let delivery = outcome.delivery;
    let llm_error = outcome.llm_error.clone();
    let empty = outcome.final_text.trim().is_empty();
    let (inserted, insert_error) = if empty || !delivery.writes_into_app() {
        (false, None)
    } else {
        match vox.insert(&outcome.final_text) {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e.to_string())),
        }
    };

    let _ = app;

    Ok(json!({
        "mode": mode.as_str(),
        "raw_text": outcome.raw_text,
        "final_text": outcome.final_text,
        "duration": duration,
        "inserted": inserted,
        "insert_error": insert_error,
        "action": outcome.action,
        "delivery": delivery.as_str(),
        // LLM 失败时不为空 —— 上层必须把它显示出来，
        // 否则用户只看到「我问什么它就答什么」，不知道请求其实失败了
        "llm_error": llm_error,
        "status": if outcome.llm_error.is_some() {
            "failed"
        } else {
            "completed"
        },
    }))
}

// ---------------- 全局热键 ----------------

/// 功能名（热键绑定用的短名），与模式一一对应。
fn feature_of(mode: Mode) -> &'static str {
    match mode {
        Mode::Dictation => "dictation",
        Mode::Translate => "translate",
        Mode::AskAnything => "ask",
    }
}

/// 启动全局热键监听线程。
///
/// 语义（集合精确匹配）：
/// - 组合键按下 → 开始录音
/// - 按住不放再加按另一个组合 → **切换模式**，录音继续
/// - 全部松开 → 结束录音并处理
fn spawn_hotkeys(app: AppHandle) -> Result<(), String> {
    let cfg = Config::load().map_err(err)?;
    let hotkey_mode = cfg.general.hotkey_mode;
    let bindings = vec![
        (
            "dictation".to_string(),
            KeyCombo::parse(&cfg.general.hotkey_dictation).map_err(err)?,
        ),
        (
            "translate".to_string(),
            KeyCombo::parse(&cfg.general.hotkey_translate).map_err(err)?,
        ),
        (
            "ask".to_string(),
            KeyCombo::parse(&cfg.general.hotkey_ask).map_err(err)?,
        ),
    ];

    std::thread::spawn(move || {
        let listener = match HotkeyListener::start(bindings) {
            Ok(l) => {
                eprintln!("全局热键监听已启动");
                // event tap 建成 = 全局热键功能上可用（比 AX 判定更准）
                if let Some(state) = app.try_state::<AppState>() {
                    if let Ok(mut h) = state.health.lock() {
                        h.hotkey_ok = true;
                        h.hotkey_error = None;
                    }
                }
                update_tray_tooltip(&app);
                l
            }
            Err(e) => {
                eprintln!("热键监听失败：{e}");
                // 记进健康状态 + 托盘提示 + 事件，别让用户「按了没反应」还不知道原因
                if let Some(state) = app.try_state::<AppState>() {
                    if let Ok(mut h) = state.health.lock() {
                        h.hotkey_ok = false;
                        h.hotkey_error = Some(e.to_string());
                    }
                }
                update_tray_tooltip(&app);
                let _ = app.emit("vox://error", format!("热键监听失败：{e}"));
                return;
            }
        };
        while let Some(ev) = listener.next() {
            match ev {
                HotkeyEvent::ComboDown {
                    feature,
                    master_pressed,
                } => {
                    eprintln!(
                        "[{:7.3}] 热键按下：{feature}（主控键按下={master_pressed}）",
                        ts()
                    );
                    let Some(mode) = Mode::parse(&feature) else {
                        continue;
                    };
                    let current = app
                        .try_state::<AppState>()
                        .and_then(|s| s.recording.lock().ok().and_then(|g| g.clone()));
                    let phase = session_phase(&app);

                    match phase {
                        // **处理中**：结果还没算出来，按热键**不要**开新录音。
                        // 否则会凭空多出一段空录音，而且浮条会卡在「录音中」再也结束不了
                        // —— 这正是「结束了但悬浮窗不结束」那个 bug 的来源。
                        SessionPhase::Processing => {
                            eprintln!("[{:7.3}] 处理中，忽略热键 {feature}", ts());
                        }
                        // 录音中
                        SessionPhase::Recording => match current {
                            // 主控键（听写热键）被按下 = **统一的结束键**：
                            // 不管当前是听写/翻译/Ask，按它就结束本次会话。
                            // （三种功能只用「开始热键」区分，结束键统一）
                            Some(cur) if master_pressed => {
                                let _ = stop_recording_internal(&app);
                                let _ = app.emit(
                                    "vox://hotkey",
                                    json!({ "feature": feature, "action": "stop", "mode": cur.mode.as_str() }),
                                );
                            }
                            // 按了另一个功能的键 → 切换模式，录音继续
                            Some(mut cur) => {
                                cur.mode = mode;
                                cur.feature = feature.clone();
                                if let Some(state) = app.try_state::<AppState>() {
                                    if let Ok(mut guard) = state.recording.lock() {
                                        *guard = Some(cur);
                                    }
                                    if let Ok(mut g) = state.float.lock() {
                                        g.mode = Some(mode.as_str().to_string());
                                        g.hint = String::new();
                                    }
                                }
                                if let Some(w) = app.get_webview_window("float") {
                                    let _ = w.show();
                                }
                                emit_float_state(&app);
                                let _ = app.emit(
                                    "vox://hotkey",
                                    json!({ "feature": feature, "action": "switch", "mode": mode.as_str() }),
                                );
                            }
                            // 状态不一致（录音刚结束）→ 忽略，别开新录音
                            None => {
                                eprintln!("[{:7.3}] 录音状态不一致，忽略热键 {feature}", ts());
                            }
                        },
                        // 空闲 → 开始一段新录音
                        SessionPhase::Idle => match spawn_recording(app.clone(), mode, feature.clone()) {
                            Ok(()) => {
                                let _ = app.emit(
                                    "vox://hotkey",
                                    json!({ "feature": feature, "action": "start", "mode": mode.as_str() }),
                                );
                            }
                            Err(e) => {
                                // 开不了录（比如没麦克风）→ **浮条要显示原因**，
                                // 不然用户只看到「按了没反应」，浮条还凭空消失。
                                show_float_error(&app, &e);
                                let _ = app.emit("vox://error", e);
                            }
                        },
                    }
                }
                // 全部松开。push_to_talk 模式下松开即结束；toggle 模式下松开不结束。
                HotkeyEvent::ComboUp { feature } => {
                    eprintln!("[{:7.3}] 热键松开：{feature}", ts());
                    if hotkey_mode != HotkeyMode::PushToTalk {
                        // toggle 模式：松开不结束录音，但**明确告诉用户怎么结束**，
                        // 否则浮窗会一直停在「录音中」，用户以为它坏了。
                        if session_phase(&app) == SessionPhase::Recording {
                            let master = Config::load()
                                .map(|c| c.general.hotkey_dictation)
                                .unwrap_or_else(|_| "RightCtrl".into());
                            if let Some(state) = app.try_state::<AppState>() {
                                if let Ok(mut g) = state.float.lock() {
                                    if g.phase == FloatPhase::Recording {
                                        g.hint =
                                            format!("已松开 · 再按一次 {master} 结束（或点这里）");
                                    }
                                }
                            }
                            emit_float_state(&app);
                        }
                        continue;
                    }
                    if session_phase(&app) != SessionPhase::Recording {
                        continue;
                    }
                    let current = app
                        .try_state::<AppState>()
                        .and_then(|s| s.recording.lock().ok().and_then(|g| g.clone()));
                    if let Some(cur) = current {
                        let _ = stop_recording_internal(&app);
                        let _ = app.emit(
                            "vox://hotkey",
                            json!({ "feature": feature, "action": "stop", "mode": cur.mode.as_str() }),
                        );
                    }
                }
            }
        }
    });
    Ok(())
}

/// 重试建立全局热键监听（用户刚授权完权限时用）。
///
/// 只有在**上次没建成**的情况下才重试 —— 否则会出现两个监听器、事件翻倍。
#[tauri::command]
fn retry_hotkey(app: AppHandle) -> Result<(), String> {
    let already_ok = app
        .try_state::<AppState>()
        .and_then(|s| s.health.lock().ok().map(|h| h.hotkey_ok))
        .unwrap_or(false);
    if already_ok {
        return Ok(());
    }
    spawn_hotkeys(app.clone()).map_err(err)?;
    // 监听是在后台线程里建的，稍等一下让状态落定
    std::thread::sleep(std::time::Duration::from_millis(300));
    Ok(())
}

// ---------------- 单实例 ----------------

// ---------------- 入口 ----------------

/// 初始化日志：**stderr + `<app_dir>/vox.log`**。
///
/// 之前桌面端**完全没有初始化 tracing**，于是所有 `tracing::warn!/error!`
/// 都丢进了黑洞 —— 模型请求失败也看不见，用户只看到「我问什么它就答什么」。
/// 写进文件后，用户可以直接把日志发出来看。
fn init_logging() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

        let _ = std::fs::create_dir_all(vox_core::config::Config::app_dir());
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(vox_core::config::Config::app_dir().join("vox.log"))
            .ok();

        let stderr_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);
        let registry = tracing_subscriber::registry().with(filter).with(stderr_layer);
        if let Some(f) = log_file {
            let _ = registry
                .with(tracing_subscriber::fmt::layer().with_writer(f))
                .try_init();
        } else {
            let _ = registry.try_init();
        }

        // 环境信息要**在 subscriber 装好之后**打，否则这条日志会被丢掉
        tracing::info!(
            "启动环境: LANG={:?} LC_ALL={:?} LC_CTYPE={:?} __CF_USER_TEXT_ENCODING={:?}",
            std::env::var("LANG").ok(),
            std::env::var("LC_ALL").ok(),
            std::env::var("LC_CTYPE").ok(),
            std::env::var("__CF_USER_TEXT_ENCODING").ok(),
        );
    });
}

pub fn run() {
    init_logging();

    // 单实例：已经有 Vox 在跑 → 把那个叫到前台，自己退出。
    // （不这么做的话两个实例会同时开麦、同时插入，内容会重复。）
    let _instance_lock = match vox_core::instance::acquire(vox_core::instance::INSTANCE_LOCK) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("检测到已有 Vox 实例在运行，本次启动退出（{e}）");
            let _ = std::process::Command::new("open")
                .arg("-a")
                .arg("Vox")
                .spawn();
            std::process::exit(0);
        }
    };

    // 全局热键只能有一个监听者（CLI 的 `vox hotkey` 也抢这把锁）。
    let _hotkey_lock = match vox_core::instance::acquire(vox_core::instance::HOTKEY_LOCK) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("全局热键已被其它进程占用，本次启动退出（{e}）");
            std::process::exit(0);
        }
    };

    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            config_path,
            key_status,
            secrets_info,
            app_status,
            set_key,
            list_history,
            delete_history,
            clear_history,
            dict_list,
            dict_upsert,
            dict_delete,
            dict_import,
            list_mics,
            copy_to_clipboard,
            open_action,
            insert_text,
            set_float_height,
            open_privacy_settings,
            retry_hotkey,
            frontmost_app,
            selected_text,
            set_launch_at_login,
            launch_at_login_status,
            float_state,
            show_float,
            hide_float,
            start_recording,
            stop_recording,
        ])
        .setup(|app| {
            // 老版本把密钥存在系统钥匙串里；这里一次性迁到本地加密文件，
            // 并删除钥匙串副本（用户要求密钥不留在系统存储里）。
            match vox_core::secrets::migrate_from_keychain(&["vox.stt", "vox.llm"]) {
                Ok(list) if !list.is_empty() => {
                    eprintln!(
                        "已把 {} 个密钥从系统钥匙串迁移到本地加密文件：{}",
                        list.len(),
                        list.join(", ")
                    );
                }
                Ok(_) => {}
                Err(e) => eprintln!("密钥迁移失败（不影响使用）：{e}"),
            }

            // 菜单栏托盘：常驻，点图标唤出设置
            let show = MenuItem::with_id(app, "show", "打开 Vox 设置", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "退出 Vox", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &sep, &quit])?;

            let mut tray = TrayIconBuilder::with_id("vox-tray")
                .menu(&menu)
                .tooltip("Vox")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        if let Some(w) = tray.app_handle().get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;

            // 关闭主窗口 = 隐藏到菜单栏，不退出
            if let Some(w) = app.get_webview_window("main") {
                let w2 = w.clone();
                w.on_window_event(move |e| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                        api.prevent_close();
                        let _ = w2.hide();
                    }
                });
            }

            // 悬浮条默认隐藏
            if let Some(w) = app.get_webview_window("float") {
                let _ = w.hide();
            }

            // 全局热键
            if let Err(e) = spawn_hotkeys(app.handle().clone()) {
                eprintln!("热键监听启动失败：{e}");
            }

            // 启动时先摸一遍麦克风 + 同步托盘提示
            refresh_mics(app.handle());

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("启动 Vox 失败");
}

/// 供其它模块复用的窗口辅助函数。
pub fn focus_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// 悬浮条窗口类型别名。
pub type FloatWindow = WebviewWindow;
