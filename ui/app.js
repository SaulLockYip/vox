// Vox 设置界面前端逻辑
// 所有能力都通过 Tauri command 走 Rust 层（vox-* crate），前端不直接发网络请求。

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

let cfg = null; // 当前配置（结构同 settings.toml）

// ---------- 工具 ----------

const $ = (id) => document.getElementById(id);

/** 应用外观主题：dark / light / system（跟随系统实时切换）。 */
function applyTheme(theme) {
  const root = document.documentElement;
  const dark = theme === "dark";
  const light = theme === "light";
  const sys = !dark && !light;
  const mq = window.matchMedia("(prefers-color-scheme: light)");
  const pick = () =>
    root.dataset.theme = sys ? (mq.matches ? "light" : "dark") : theme;
  pick();
  if (sys && mq.addEventListener) {
    mq.addEventListener("change", pick);
  }
}

function toast(msg, isError = false) {
  const el = $("toast");
  el.textContent = msg;
  el.className = "toast" + (isError ? " error" : "");
  clearTimeout(toast._t);
  toast._t = setTimeout(() => {
    el.textContent = "";
  }, 2600);
}

function setStatus(text, busy = false, isError = false) {
  $("status-text").textContent = text;
  $("status-dot").className =
    "status-dot" + (busy ? " busy" : "") + (isError ? " error" : "");
}

// ---------- 健康状态（麦克风 / 密钥 / 热键） ----------

let health = null;

// ---------- 权限 / 设备引导 ----------

/** 缺权限或缺麦克风时，在顶部弹横幅引导用户去系统设置。 */
function updatePermissionBanner() {
  const h = health;
  const banner = $("perm-banner");
  if (!h || !banner) return;

  let title = "";
  let desc = "";
  let kind = "accessibility";
  // 判据是**功能事实**：全局热键到底能不能用（event tap 建成没有），
  // 不是系统的 AX 判定 —— 后者可能与实际不符，误报会让用户白折腾。
  if (!h.hotkey_ok) {
    title = "全局热键不可用";
    desc =
      "Vox 需要「辅助功能」权限才能监听全局热键、并在录音结束后把文字插进光标处。" +
      "点「打开系统设置」→ 隐私与安全性 → 辅助功能 → 勾选 Vox，然后点「重新检测」。";
    kind = "accessibility";
  } else if (!(h.mics || []).length) {
    title = "未检测到麦克风";
    desc =
      "请确认耳机/麦克风已连接；也可以点「重新检测」重新扫描输入设备，" +
      "或在下面的「输入设备」里换一个。";
    kind = "microphone";
  }

  if (title) {
    $("perm-title").textContent = title;
    $("perm-desc").textContent = desc;
    banner.dataset.kind = kind;
    banner.classList.remove("hidden");
  } else {
    banner.classList.add("hidden");
  }
}

/** 扫描输入设备，填进下拉框。 */
async function refreshMicList() {
  const sel = $("input_device");
  if (!sel) return;
  let list = [];
  try {
    list = (await invoke("list_mics")) || [];
  } catch (e) {
    $("mic-state").textContent = "❌ 枚举输入设备失败";
    return;
  }
  const cur = sel.value || (cfg && cfg.general && cfg.general.input_device) || "";
  sel.innerHTML = "";
  const opt0 = document.createElement("option");
  opt0.value = "";
  opt0.textContent = list.length ? "系统默认输入设备" : "（未检测到输入设备）";
  sel.appendChild(opt0);
  for (const name of list) {
    const o = document.createElement("option");
    o.value = name;
    o.textContent = name;
    sel.appendChild(o);
  }
  sel.value = list.includes(cur) ? cur : "";
  $("mic-state").textContent = list.length
    ? "✅ " + list.length + " 个输入设备"
    : "❌ 没有可用输入设备";
}

async function refreshHealth() {
  try {
    health = await invoke("app_status");
  } catch (e) {
    return;
  }
  const h = health;
  const mics = h.mics || [];
  const lines = [
    (mics.length ? "✅" : "❌") +
      " 麦克风" +
      (mics.length ? "（" + mics.join("、") + "）" : "（未检测到，无法录音）"),
    // **功能事实**：event tap 建成没有 = 热键与 ⌘V 注入能不能用
    (h.hotkey_ok ? "✅" : "❌") +
      " 全局热键与 ⌘V 注入" +
      (h.hotkey_ok ? "" : "（需要辅助功能权限：系统设置 › 隐私与安全性 › 辅助功能 勾选 Vox）"),
    // 系统判定仅供参考，可能与实际不符（例如从终端启动时）
    (h.accessibility_ok ? "✅" : "⚠️") +
      " 系统辅助功能判定" +
      (h.accessibility_ok ? "" : "（系统说未授权，但只要上一行是 ✅ 就不影响使用）"),
    (h.stt_ok ? "✅" : "❌") + " ASR 密钥",
    (h.llm_ok ? "✅" : "❌") + " LLM 密钥",
    (h.hotkey_error ? "❌" : "✅") +
      " 全局热键" +
      (h.hotkey_error ? "（" + h.hotkey_error + "）" : ""),
  ];
  const el = $("health-list");
  if (el) {
    el.innerHTML = "";
    for (const line of lines) {
      const d = document.createElement("div");
      d.textContent = line;
      el.appendChild(d);
    }
  }
  // 有问题就别显示「就绪」——否则用户以为一切正常
  updatePermissionBanner();
  if (!h.ready) {
    setStatus("⚠️ " + (h.problems[0] || "有问题"), false, true);
  } else if (!recording) {
    setStatus("就绪");
  }
}

// ---------- 页签 ----------

for (const btn of document.querySelectorAll(".tab")) {
  btn.addEventListener("click", () => {
    document.querySelectorAll(".tab").forEach((b) => b.classList.remove("active"));
    btn.classList.add("active");
    const name = btn.dataset.tab;
    for (const page of document.querySelectorAll(".page")) {
      page.classList.add("hidden");
    }
    $("page-" + name).classList.remove("hidden");
    if (name === "dict") refreshDict();
    if (name === "history") refreshHistory();
  });
}

// ---------- 配置 ----------

const TEXT_FIELDS = [
  ["stt", "base_url"], ["stt", "path"], ["stt", "model"], ["stt", "language"],
  ["llm", "base_url"], ["llm", "path"], ["llm", "model"],
  ["general", "hotkey_dictation"], ["general", "hotkey_translate"], ["general", "hotkey_ask"],
  ["general", "preferred_language"], ["general", "translate_target"],
];
const NUM_FIELDS = [["llm", "max_tokens"], ["general", "history_retention_days"]];
const BOOL_FIELDS = [
  ["general", "auto_punctuation"], ["general", "smart_formatting"],
  ["general", "use_dictionary"], ["general", "keep_result_in_clipboard"],
  ["general", "interaction_sounds"],
  ["general", "mute_audio_while_dictating"],
  ["general", "launch_at_login"],
];
const PROMPT_FIELDS = [
  ["dictation", "prompt_dictation"],
  ["translate", "prompt_translate"],
  ["ask_anything", "prompt_ask_anything"],
];

function fillForm(c) {
  for (const [sec, key] of TEXT_FIELDS) {
    const el = $(key);
    if (el) el.value = c[sec]?.[key] ?? "";
  }
  for (const [sec, key] of NUM_FIELDS) {
    const el = $(key);
    if (el) el.value = c[sec]?.[key] ?? 0;
  }
  for (const [sec, key] of BOOL_FIELDS) {
    const el = $(key);
    if (el) el.checked = !!c[sec]?.[key];
  }
  $("insert_method").value = c.general?.insert_method ?? "clipboard";
  $("hotkey_mode").value = c.general?.hotkey_mode ?? "toggle";
  // Ask Anything 专用覆盖
  $("ask_model").value = c.llm?.ask?.model ?? "";
  $("ask_thinking").value = c.llm?.ask?.thinking ?? "adaptive";
  $("ask_effort").value = c.llm?.ask?.effort ?? "low";
  $("llm_service_tier").value = c.llm?.service_tier ?? "priority";
  $("agent_tools").value = c.general?.agent_tools ?? "safe";
  $("appearance").value = c.general?.appearance ?? "dark";
  applyTheme(c.general?.appearance ?? "dark");
  // 输入设备（下拉框内容由 refreshMicList 填，这里只记住当前选择）
  const dev = c.general?.input_device ?? "";
  if (dev) $("input_device").value = dev;
  $("llm_effort").value = c.llm?.effort ?? "max";
  $("llm_thinking").value = c.llm?.thinking ?? "adaptive";
  for (const [key, id] of PROMPT_FIELDS) {
    const el = $(id);
    if (el) el.value = c.prompts?.[key] ?? "";
  }
}

function collectForm() {
  const c = JSON.parse(JSON.stringify(cfg));
  for (const [sec, key] of TEXT_FIELDS) {
    const el = $(key);
    if (el) c[sec][key] = el.value.trim();
  }
  for (const [sec, key] of NUM_FIELDS) {
    const el = $(key);
    if (el) c[sec][key] = Number(el.value);
  }
  for (const [sec, key] of BOOL_FIELDS) {
    const el = $(key);
    if (el) c[sec][key] = el.checked;
  }
  c.general.insert_method = $("insert_method").value;
  c.general.hotkey_mode = $("hotkey_mode").value;
  // Ask Anything 专用覆盖
  c.llm.ask = c.llm.ask || {};
  c.llm.ask.model = $("ask_model").value.trim() || null;
  c.llm.ask.thinking = $("ask_thinking").value;
  c.llm.ask.effort = $("ask_effort").value.trim() || null;
  c.llm.service_tier = $("llm_service_tier").value;
  c.general.agent_tools = $("agent_tools").value;
  c.general.input_device = $("input_device").value;
  c.general.appearance = $("appearance").value;
  c.llm.effort = $("llm_effort").value;
  c.llm.thinking = $("llm_thinking").value;
  for (const [key, id] of PROMPT_FIELDS) {
    const el = $(id);
    if (el) c.prompts[key] = el.value;
  }
  // 明文密钥不落盘
  delete c.stt.api_key;
  delete c.llm.api_key;
  return c;
}

async function loadConfig() {
  cfg = await invoke("get_config");
  fillForm(cfg);
  $("config-path").textContent = "配置文件：" + (await invoke("config_path"));
  await refreshKeyState();
  // 开机自启的真实状态来自 LaunchAgent，不只是配置里的布尔值
  $("launch_at_login").checked = await invoke("launch_at_login_status");
}

$("launch_at_login").addEventListener("change", async (ev) => {
  const on = ev.target.checked;
  try {
    await invoke("set_launch_at_login", { enabled: on });
    if (cfg) cfg.general.launch_at_login = on;
    toast(on ? "已开启开机自启" : "已关闭开机自启");
  } catch (e) {
    toast("设置失败：" + e, true);
    ev.target.checked = !on;
  }
});

async function refreshKeyState() {
  const st = await invoke("key_status");
  $("stt_key_state").textContent = st.stt ? "✅ 已配置" : "未配置";
  $("llm_key_state").textContent = st.llm ? "✅ 已配置" : "未配置";
  // 密钥文件位置（加密存储，不进系统钥匙串）
  try {
    const info = await invoke("secrets_info");
    const n = (info.accounts || []).length;
    $("secrets-path").textContent =
      "密钥以 XChaCha20-Poly1305 加密存于：" + info.path + "（已存 " + n + " 项，权限 0600）";
  } catch (e) {
    /* 忽略：只是路径提示 */
  }
}

$("save_all").addEventListener("click", async () => {
  try {
    cfg = await invoke("save_config", { value: collectForm() });
    fillForm(cfg);
    toast("已保存");
  } catch (e) {
    toast("保存失败：" + e, true);
  }
});

// ---------- 密钥 ----------

async function saveKey(kind, inputId) {
  const key = $(inputId).value.trim();
  if (!key) {
    toast("请先填入 Key", true);
    return;
  }
  try {
    await invoke("set_key", { kind, key });
    $(inputId).value = "";
    await refreshKeyState();
    await refreshHealth();
    toast(kind.toUpperCase() + " Key 已加密保存到本地文件");
  } catch (e) {
    toast("保存失败：" + e, true);
  }
}
$("save_stt_key").addEventListener("click", () => saveKey("stt", "stt_key"));
$("save_llm_key").addEventListener("click", () => saveKey("llm", "llm_key"));

// ---------- 词典 ----------

async function refreshDict() {
  const rows = await invoke("dict_list");
  const tbody = $("dict_rows");
  tbody.innerHTML = "";
  for (const r of rows) {
    const tr = document.createElement("tr");
    tr.innerHTML = "<td></td><td></td><td><button class='link danger'>删除</button></td>";
    tr.children[0].textContent = r.term;
    tr.children[1].textContent = r.replacement || "（仅提示）";
    tr.children[2].querySelector("button").addEventListener("click", async () => {
      await invoke("dict_delete", { id: r.id });
      refreshDict();
    });
    tbody.appendChild(tr);
  }
}

$("dict_add").addEventListener("click", async () => {
  const term = $("dict_term").value.trim();
  if (!term) {
    toast("请填词条", true);
    return;
  }
  await invoke("dict_upsert", { term, replacement: $("dict_replacement").value.trim() });
  $("dict_term").value = "";
  $("dict_replacement").value = "";
  refreshDict();
  toast("已添加");
});

$("dict_import").addEventListener("click", async () => {
  const content = $("dict_csv").value;
  if (!content.trim()) {
    toast("请填 CSV 文本", true);
    return;
  }
  try {
    const r = await invoke("dict_import", { content });
    $("dict_csv").value = "";
    refreshDict();
    toast("已导入 " + r.count + " 个词条");
  } catch (e) {
    toast("导入失败：" + e, true);
  }
});

// ---------- 历史 ----------

async function refreshHistory() {
  const rows = await invoke("list_history", { limit: 200 });
  const tbody = $("history_rows");
  tbody.innerHTML = "";
  for (const r of rows) {
    const tr = document.createElement("tr");
    tr.innerHTML = "<td></td><td></td><td></td><td><button class='link danger'>删除</button></td>";
    tr.children[0].textContent = (r.created_at || "").replace("T", " ").replace("Z", "");
    tr.children[1].textContent = r.mode;
    tr.children[2].textContent = r.refined_text || r.raw_text || "";
    tr.children[3].querySelector("button").addEventListener("click", async () => {
      await invoke("delete_history", { id: r.id });
      refreshHistory();
    });
    tbody.appendChild(tr);
  }
}

$("history_refresh").addEventListener("click", refreshHistory);
$("history_clear").addEventListener("click", async () => {
  const n = await invoke("clear_history");
  refreshHistory();
  toast("已清空 " + n + " 条");
});

// ---------- 录音 ----------

let recording = false;

for (const btn of document.querySelectorAll(".rec-btn")) {
  const mode = btn.dataset.mode;

  const begin = async (ev) => {
    if (ev && ev.preventDefault) ev.preventDefault();
    if (recording) return;
    recording = true;
    btn.classList.add("recording");
    setStatus("录音中 · " + mode, true);
    $("rec-output").textContent = "…";
    try {
      await invoke("start_recording", { mode });
    } catch (e) {
      recording = false;
      btn.classList.remove("recording");
      setStatus("⚠️ " + e, false, true);
      toast("无法开始录音：" + e, true);
    }
  };
  const end = async (ev) => {
    if (ev && ev.preventDefault) ev.preventDefault();
    if (!recording) return;
    recording = false;
    btn.classList.remove("recording");
    setStatus("处理中…", true);
    try {
      await invoke("stop_recording");
    } catch (e) {
      toast("停止失败：" + e, true);
    }
  };

  btn.addEventListener("mousedown", begin);
  btn.addEventListener("mouseup", end);
  btn.addEventListener("mouseleave", (ev) => {
    if (recording) end(ev);
  });
  btn.addEventListener("touchstart", begin, { passive: false });
  btn.addEventListener("touchend", end, { passive: false });
}

listen("vox://hotkey", (e) => {
  const d = e.payload || {};
  const mode = d.feature === "ask" ? "ask_anything" : d.feature;
  const btn = document.querySelector('.rec-btn[data-mode="' + mode + '"]');
  if (d.action === "start") {
    btn?.classList.add("recording");
    setStatus("录音中 · " + (d.mode || mode), true);
  } else if (d.action === "switch") {
    // 按住不放又加按了别的组合 → 切模式，录音继续
    for (const b of document.querySelectorAll(".rec-btn")) b.classList.remove("recording");
    btn?.classList.add("recording");
    setStatus("录音中 · " + (d.mode || mode), true);
  } else {
    btn?.classList.remove("recording");
    setStatus("处理中…", true);
  }
});

listen("vox://result", (e) => {
  setStatus("就绪");
  for (const b of document.querySelectorAll(".rec-btn")) b.classList.remove("recording");
  const d = e.payload || {};
  $("rec-label").textContent = "结果 · " + d.mode + " · " + (d.duration || 0).toFixed(1) + "s";
  $("rec-output").textContent = d.final_text || d.raw_text || "（空）";
  if (d.inserted) {
    toast("完成 · 已插入到光标处（结果也在剪贴板上）");
  } else if (d.insert_error) {
    setStatus("⚠️ 插入失败：" + d.insert_error, false, true);
    toast("处理完成，但插入失败：" + d.insert_error, true);
  } else {
    toast("完成");
  }
});

// 清除结果面板
$("clear_result").addEventListener("click", () => {
  $("rec-label").textContent = "结果";
  $("rec-output").textContent = "（还没有结果）";
});

listen("vox://error", (e) => {
  for (const b of document.querySelectorAll(".rec-btn")) b.classList.remove("recording");
  setStatus("⚠️ " + (e.payload || "出错了"), false, true);
  toast("失败：" + e.payload, true);
  refreshHealth();
});

// ---------- 启动 ----------

loadConfig().catch((e) => {
  setStatus("⚠️ 加载配置失败", false, true);
  toast("加载配置失败：" + e, true);
});
refreshHealth();
refreshMicList();

// 权限 / 设备引导
$("perm-open").addEventListener("click", async () => {
  const kind = $("perm-banner").dataset.kind || "accessibility";
  try {
    await invoke("open_privacy_settings", { kind });
  } catch (e) {
    toast("打开系统设置失败：" + e, true);
  }
});
$("perm-recheck").addEventListener("click", async () => {
  // 刚授权完权限 → 试着把热键监听重新建起来（不用重启 app）
  try {
    await invoke("retry_hotkey");
  } catch (e) {
    /* 忽略：下面会显示状态 */
  }
  await refreshHealth();
  await refreshMicList();
  toast("已重新检测");
});
$("mic_refresh").addEventListener("click", async () => {
  await refreshMicList();
  await refreshHealth();
  toast("已重新检测输入设备");
});
$("appearance").addEventListener("change", () => {
  applyTheme($("appearance").value);
});
$("input_device").addEventListener("change", () => {
  // 选了设备就立刻保存，免得忘了点「保存全部设置」
  (async () => {
    try {
      cfg = await invoke("save_config", { value: collectForm() });
      toast("已选择输入设备：" + ($("input_device").value || "系统默认"));
    } catch (e) {
      toast("保存失败：" + e, true);
    }
  })();
});
