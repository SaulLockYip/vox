//! 门面 crate：把配置、录音、ASR、LLM、词典、注入、存储串起来。
//!
//! CLI 与 Tauri 都只依赖这一层。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use vox_audio::recorder::Recorder;
use vox_core::config::{Config, InsertMethod};
use vox_core::error::{Error, Result};
use vox_core::mode::Mode;
use vox_macos::inject;
use vox_pipeline::dictionary::DictionaryEntry;
use vox_pipeline::template::PromptContext;
use vox_pipeline::voice::{PipelineOutcome, VoicePipeline};
use vox_storage::{HistoryEntry, Store};

/// 应用主体。
pub struct VoxApp {
    cfg: Config,
    store: Store,
}

impl VoxApp {
    /// 从默认位置加载配置与数据库。
    pub fn load() -> Result<VoxApp> {
        let cfg = Config::load()?;
        let store = Store::open(&db_path())?;
        Ok(VoxApp { cfg, store })
    }

    /// 用给定配置与数据库构建（测试用）。
    pub fn with_parts(cfg: Config, store: Store) -> VoxApp {
        VoxApp { cfg, store }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn config_mut(&mut self) -> &mut Config {
        &mut self.cfg
    }

    pub fn set_config(&mut self, cfg: Config) {
        self.cfg = cfg;
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// 字典词条。
    pub fn dictionary(&self) -> Result<Vec<DictionaryEntry>> {
        self.store.list_dictionary()
    }

    /// 处理一个已有音频文件（不录音）。
    pub async fn process_file(
        &self,
        mode: Mode,
        audio_path: &Path,
        ctx: PromptContext,
    ) -> Result<PipelineOutcome> {
        self.process_file_with_duration(mode, audio_path, ctx, None).await
    }

    /// 同 [`VoxApp::process_file`]，但**带录音时长**写进历史。
    ///
    /// 桌面端知道时长，就用这个 —— 否则同一次会话会被写**两条**历史
    /// （这里一条、桌面端又一条），历史列表里每条都重复。
    pub async fn process_file_with_duration(
        &self,
        mode: Mode,
        audio_path: &Path,
        ctx: PromptContext,
        duration: Option<f64>,
    ) -> Result<PipelineOutcome> {
        let pipeline = VoicePipeline::new(self.cfg.clone())?;
        let dict = self.dictionary()?;
        let outcome = pipeline.process(mode, audio_path, ctx, &dict).await?;
        self.record_history(mode, &outcome, audio_path, duration)?;
        Ok(outcome)
    }

    /// 录一段音并落盘成 WAV。停止时机由调用方控制（CLI 用 Enter，GUI 用再次按键）。
    pub fn record_to_wav(
        &self,
        device_id: Option<&str>,
        on_start: impl FnOnce(),
        wait_stop: impl FnOnce(),
    ) -> Result<(PathBuf, f64)> {
        let recorder = Recorder::start(device_id)?;
        on_start();
        wait_stop();

        let recording = recorder.stop()?;
        let target = recording.to_target_rate();
        let duration = target.duration_secs();

        let path = temp_audio_path()?;
        vox_audio::wav::write_wav(&path, &target.samples, target.sample_rate)?;
        Ok((path, duration))
    }

    /// 把最终文本注入到焦点应用。
    pub fn insert(&self, text: &str) -> Result<()> {
        let method = self.cfg.general.insert_method;
        // keep_result_in_clipboard = true → 结果留在剪贴板上（不还原原内容）
        let restore = !self.cfg.general.keep_result_in_clipboard;
        inject::insert_text(text, method, restore)
    }

    /// 当前前台应用名。
    pub fn frontmost_app_name(&self) -> Option<String> {
        inject::frontmost_app_name()
    }

    fn record_history(
        &self,
        mode: Mode,
        outcome: &PipelineOutcome,
        audio_path: &Path,
        duration: Option<f64>,
    ) -> Result<()> {
        let now = vox_storage::now_iso();
        let entry = HistoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            mode: mode.as_str().to_string(),
            // LLM 失败时如实记录，别让历史里显示成「成功」
            status: if outcome.llm_error.is_some() {
                "failed".into()
            } else {
                "completed".into()
            },
            raw_text: outcome.raw_text.clone(),
            refined_text: outcome.final_text.clone(),
            duration,
            created_at: now.clone(),
            updated_at: now,
            app_name: self.frontmost_app_name(),
            audio_path: Some(audio_path.to_string_lossy().to_string()),
        };
        self.store.insert_history(&entry)
    }
}

/// 数据库路径。
pub fn db_path() -> PathBuf {
    Config::app_dir().join("vox.db")
}

/// 临时音频目录。
pub fn tmp_dir() -> PathBuf {
    Config::app_dir().join("tmp")
}

/// 新的临时音频文件路径。
pub fn temp_audio_path() -> Result<PathBuf> {
    let dir = tmp_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{}.wav", uuid::Uuid::new_v4())))
}

/// 打开数据库（供 CLI 直接访问）。
pub fn open_store() -> Result<Store> {
    Store::open(&db_path())
}

/// 列出可用的麦克风名称。
pub fn list_input_devices() -> Result<Vec<String>> {
    vox_audio::recorder::list_input_devices()
}

/// 构造一次处理所需的上下文。
///
/// - 翻译模式：带上目标语言
/// - Ask Anything：优先取**前台应用焦点控件的选区**（AX），读不到再回退剪贴板
/// - 两者都带上前台应用名（供 prompt 里的 `{{app_name}}`）
pub fn build_context(mode: Mode, target_language: &str) -> vox_pipeline::template::PromptContext {
    let mut ctx = vox_pipeline::template::PromptContext::default();

    if mode == Mode::Translate {
        ctx.target_language = Some(target_language.to_string());
    }

    if mode == Mode::AskAnything {
        // ① 选区：AX 读；读不到就用 ⌘C 模拟复制兜底
        if let Some(text) = vox_macos::ax::selected_text_with_fallback() {
            ctx.selection = Some(text);
        }
        // ② 光标前后的内容（作为问答的上下文）
        if let Some(c) = vox_macos::ax::cursor_context(800, 800) {
            if !c.before.trim().is_empty() {
                ctx.context_before = Some(c.before);
            }
            if !c.after.trim().is_empty() {
                ctx.context_after = Some(c.after);
            }
            if ctx.selection.is_none() && !c.selected.trim().is_empty() {
                ctx.selection = Some(c.selected);
            }
        }
        // ③ 再回退：剪贴板
        if ctx.selection.is_none() {
            if let Ok(text) = vox_macos::read_clipboard() {
                if !text.trim().is_empty() {
                    ctx.selection = Some(text.trim().to_string());
                }
            }
        }
    }

    ctx.app_name = vox_macos::inject::frontmost_app_name();
    ctx
}

/// 便捷别名。
pub type AppResult<T> = std::result::Result<T, Error>;

/// 默认注入方式。
pub const DEFAULT_INSERT_METHOD: InsertMethod = InsertMethod::Clipboard;

/// 可停止的录音会话。
///
/// `Recorder` 里的 cpal `Stream` 在部分平台不是 `Send`，所以采集必须留在
/// 启动它的那个线程里：这里在后台线程内完成「开麦 → 等 stop 标志 → 停麦 → 写 WAV」。
pub struct RecordingSession {
    stop: Arc<AtomicBool>,
    join: Mutex<Option<JoinHandle<Result<(PathBuf, f64)>>>>,
}

impl RecordingSession {
    /// 开始录音（后台线程采集）。`device_name` 为 None 时用系统默认设备。
    pub fn start(device_name: Option<&str>) -> Result<RecordingSession> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_c = Arc::clone(&stop);
        let device = device_name.map(|s| s.to_string());

        let join = std::thread::spawn(move || -> Result<(PathBuf, f64)> {
            let recorder = Recorder::start(device.as_deref())?;
            while !stop_c.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(40));
            }
            let recording = recorder.stop()?;
            let target = recording.to_target_rate();
            let path = temp_audio_path()?;
            vox_audio::wav::write_wav(&path, &target.samples, target.sample_rate)?;
            Ok((path, target.duration_secs()))
        });

        Ok(RecordingSession {
            stop,
            join: Mutex::new(Some(join)),
        })
    }

    /// 请求停止录音（不阻塞）。
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// 等待录音结束并返回 (wav 路径, 时长秒)。
    ///
    /// **不会**自己请求停止——要结束录音请先调用 [`RecordingSession::request_stop`]。
    /// 这里只负责 join 采集线程（它会一直录到 stop 标志被置位）。
    pub fn wait(&self) -> Result<(PathBuf, f64)> {
        let handle = self
            .join
            .lock()
            .map_err(|_| Error::Audio("录音会话锁异常".into()))?
            .take();
        match handle {
            Some(h) => h
                .join()
                .map_err(|_| Error::Audio("录音线程异常退出".into()))?,
            None => Err(Error::Audio("录音会话已被消费".into())),
        }
    }
}

impl Drop for RecordingSession {
    /// 会话被丢弃时**必须**让采集线程停下来。
    ///
    /// 否则会留下一个「孤儿录音器」：线程还活着、麦克风还开着，
    /// 而 `JoinHandle` 已经被丢掉（线程被 detach）——
    /// 蓝牙耳机会一直卡在 **HFP**（单声道 16k，音质差），
    /// 因为麦克风一直没被释放。
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}
