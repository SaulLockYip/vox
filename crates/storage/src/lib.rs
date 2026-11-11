//! 本地存储：转写历史 + 个人词典（无登录、无云同步）。
//!
//! 数据库文件：`~/Library/Application Support/Vox/vox.db`

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use vox_core::error::{Error, Result};
use vox_pipeline::dictionary::DictionaryEntry;

/// 一条历史记录。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HistoryEntry {
    pub id: String,
    pub mode: String,
    pub status: String,
    pub raw_text: String,
    pub refined_text: String,
    pub duration: Option<f64>,
    pub created_at: String,
    pub updated_at: String,
    pub app_name: Option<String>,
    pub audio_path: Option<String>,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// 打开（并在需要时创建）数据库。
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn =
            Connection::open(path).map_err(|e| Error::Other(format!("打开数据库失败: {e}")))?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    /// 在内存数据库上打开（测试用）。
    pub fn open_in_memory() -> Result<Store> {
        let conn =
            Connection::open_in_memory().map_err(|e| Error::Other(format!("打开内存数据库失败: {e}")))?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        self.conn
            .execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS history (
                    id           TEXT PRIMARY KEY,
                    mode         TEXT NOT NULL DEFAULT 'dictation',
                    status       TEXT NOT NULL DEFAULT 'completed',
                    raw_text     TEXT NOT NULL DEFAULT '',
                    refined_text TEXT NOT NULL DEFAULT '',
                    duration     REAL,
                    created_at   TEXT NOT NULL,
                    updated_at   TEXT NOT NULL,
                    app_name     TEXT,
                    audio_path   TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_history_created_at ON history(created_at DESC);

                CREATE TABLE IF NOT EXISTS dictionary (
                    id          INTEGER PRIMARY KEY AUTOINCREMENT,
                    term        TEXT NOT NULL UNIQUE,
                    replacement TEXT NOT NULL DEFAULT '',
                    auto        INTEGER NOT NULL DEFAULT 0,
                    created_at  TEXT NOT NULL
                );
                "#,
            )
            .map_err(|e| Error::Other(format!("初始化数据库失败: {e}")))?;
        Ok(())
    }

    // ---------- 历史 ----------

    pub fn insert_history(&self, entry: &HistoryEntry) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO history
                 (id, mode, status, raw_text, refined_text, duration, created_at, updated_at, app_name, audio_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    entry.id,
                    entry.mode,
                    entry.status,
                    entry.raw_text,
                    entry.refined_text,
                    entry.duration,
                    entry.created_at,
                    entry.updated_at,
                    entry.app_name,
                    entry.audio_path,
                ],
            )
            .map_err(|e| Error::Other(format!("写入历史失败: {e}")))?;
        Ok(())
    }

    pub fn list_history(&self, limit: usize) -> Result<Vec<HistoryEntry>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, mode, status, raw_text, refined_text, duration, created_at, updated_at, app_name, audio_path
                 FROM history ORDER BY created_at DESC LIMIT ?1",
            )
            .map_err(|e| Error::Other(format!("查询历史失败: {e}")))?;
        let rows = stmt
            .query_map([limit as i64], map_history)
            .map_err(|e| Error::Other(format!("读取历史失败: {e}")))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| Error::Other(format!("读取历史失败: {e}")))?);
        }
        Ok(out)
    }

    pub fn delete_history(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM history WHERE id = ?1", params![id])
            .map_err(|e| Error::Other(format!("删除历史失败: {e}")))?;
        Ok(())
    }

    /// 清理超过保留期的记录。`retention_days` 为负表示永久保留。
    pub fn purge_history(&self, retention_days: i64) -> Result<usize> {
        if retention_days < 0 {
            return Ok(0);
        }
        let cutoff = now_iso_minus_days(retention_days);
        let n = self
            .conn
            .execute("DELETE FROM history WHERE created_at < ?1", params![cutoff])
            .map_err(|e| Error::Other(format!("清理历史失败: {e}")))?;
        Ok(n)
    }

    // ---------- 词典 ----------

    pub fn list_dictionary(&self) -> Result<Vec<DictionaryEntry>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, term, replacement, auto, created_at FROM dictionary ORDER BY term")
            .map_err(|e| Error::Other(format!("查询词典失败: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(DictionaryEntry {
                    id: row.get(0)?,
                    term: row.get(1)?,
                    replacement: row.get(2)?,
                    auto: row.get::<_, i64>(3)? != 0,
                    created_at: row.get(4)?,
                })
            })
            .map_err(|e| Error::Other(format!("读取词典失败: {e}")))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| Error::Other(format!("读取词典失败: {e}")))?);
        }
        Ok(out)
    }

    /// 添加或更新词条（按 term 唯一）。
    pub fn upsert_dictionary(&self, term: &str, replacement: &str, auto: bool) -> Result<DictionaryEntry> {
        let now = now_iso();
        self.conn
            .execute(
                "INSERT INTO dictionary (term, replacement, auto, created_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(term) DO UPDATE SET replacement = excluded.replacement, auto = excluded.auto",
                params![term, replacement, auto as i64, now],
            )
            .map_err(|e| Error::Other(format!("写入词典失败: {e}")))?;

        let entry = self
            .conn
            .query_row(
                "SELECT id, term, replacement, auto, created_at FROM dictionary WHERE term = ?1",
                params![term],
                |row| {
                    Ok(DictionaryEntry {
                        id: row.get(0)?,
                        term: row.get(1)?,
                        replacement: row.get(2)?,
                        auto: row.get::<_, i64>(3)? != 0,
                        created_at: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(|e| Error::Other(format!("读取词典失败: {e}")))?;
        entry.ok_or_else(|| Error::Other("词条写入后读不到".into()))
    }

    pub fn delete_dictionary(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM dictionary WHERE id = ?1", params![id])
            .map_err(|e| Error::Other(format!("删除词条失败: {e}")))?;
        Ok(())
    }

    /// 从单列 CSV 文本导入词条（单列、无表头）。
    pub fn import_dictionary_csv(&self, content: &str) -> Result<Vec<String>> {
        let mut words: Vec<String> = Vec::new();
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.contains(',') || trimmed.contains('\t') {
                return Err(Error::Other(format!(
                    "第 {} 行不是单列格式（CSV 只允许一列词条）",
                    idx + 1
                )));
            }
            if !words.iter().any(|w| w == trimmed) {
                words.push(trimmed.to_string());
            }
        }
        for w in &words {
            self.upsert_dictionary(w, "", false)?;
        }
        Ok(words)
    }
}

fn map_history(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
    Ok(HistoryEntry {
        id: row.get(0)?,
        mode: row.get(1)?,
        status: row.get(2)?,
        raw_text: row.get(3)?,
        refined_text: row.get(4)?,
        duration: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        app_name: row.get(8)?,
        audio_path: row.get(9)?,
    })
}

/// 当前时间的 ISO-8601（秒精度，UTC）。
pub fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    iso_from_unix(secs)
}

/// N 天前的 ISO-8601（秒精度，UTC）。
pub fn now_iso_minus_days(days: i64) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    iso_from_unix(secs - days * 86_400)
}

/// Unix 秒 → `YYYY-MM-DDTHH:MM:SSZ`（Howard Hinnant 的 civil_from_days 算法）。
fn iso_from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> HistoryEntry {
        HistoryEntry {
            id: id.into(),
            mode: "dictation".into(),
            status: "completed".into(),
            raw_text: "原始".into(),
            refined_text: "整理后".into(),
            duration: Some(1.5),
            created_at: now_iso(),
            updated_at: now_iso(),
            app_name: Some("TextEdit".into()),
            audio_path: None,
        }
    }

    #[test]
    fn history_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        s.insert_history(&entry("a")).unwrap();
        let list = s.list_history(10).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].refined_text, "整理后");
        assert_eq!(list[0].app_name.as_deref(), Some("TextEdit"));
    }

    #[test]
    fn history_delete() {
        let s = Store::open_in_memory().unwrap();
        s.insert_history(&entry("a")).unwrap();
        s.delete_history("a").unwrap();
        assert!(s.list_history(10).unwrap().is_empty());
    }

    #[test]
    fn purge_respects_retention() {
        let s = Store::open_in_memory().unwrap();
        let mut old = entry("old");
        old.created_at = "2000-01-01T00:00:00Z".into();
        s.insert_history(&old).unwrap();
        s.insert_history(&entry("new")).unwrap();

        assert_eq!(s.purge_history(-1).unwrap(), 0);
        assert_eq!(s.list_history(10).unwrap().len(), 2);

        assert_eq!(s.purge_history(30).unwrap(), 1);
        let left = s.list_history(10).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, "new");
    }

    #[test]
    fn dictionary_upsert_by_term() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_dictionary("Vox", "Vox", false).unwrap();
        s.upsert_dictionary("Vox", "Vox Pro", false).unwrap();
        let list = s.list_dictionary().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].replacement, "Vox Pro");
    }

    #[test]
    fn dictionary_delete() {
        let s = Store::open_in_memory().unwrap();
        let e = s.upsert_dictionary("a", "", false).unwrap();
        s.delete_dictionary(e.id).unwrap();
        assert!(s.list_dictionary().unwrap().is_empty());
    }

    #[test]
    fn csv_import_requires_single_column() {
        let s = Store::open_in_memory().unwrap();
        let words = s.import_dictionary_csv("alpha\nbeta\nalpha\n").unwrap();
        assert_eq!(words, vec!["alpha", "beta"]);
        assert_eq!(s.list_dictionary().unwrap().len(), 2);

        assert!(s.import_dictionary_csv("a,b\n").is_err());
    }

    #[test]
    fn iso_formatting() {
        assert_eq!(iso_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_from_unix(1_700_000_000), "2023-11-14T22:13:20Z");
    }
}
