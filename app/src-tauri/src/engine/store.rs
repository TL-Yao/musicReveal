//! 持久化：settings.json 与 history.json。原子写；损坏时备份为 .bak 后用默认值。

use super::model::Fmt;
use super::scan::Stat;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub onboarded: bool,
    pub folders: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub format: Fmt,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            onboarded: false,
            folders: vec![],
            output_dir: home().join("Music/musicReveal"),
            format: Fmt::Original,
        }
    }
}

pub fn home() -> PathBuf {
    #[cfg(debug_assertions)]
    if let Some(h) = std::env::var_os("MUSICREVEAL_HOME") {
        return h.into();
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    pub path: PathBuf,
    #[serde(flatten)]
    pub stat: Stat,
}

/// 「要转换已有的歌吗」还没回答：提问时的那批文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pending {
    pub folder: PathBuf,
    pub files: Vec<PathBuf>,
}

/// 处理记录。同一源路径 + 格式只留最新一条。
#[derive(Debug, Default)]
pub struct History {
    done: HashMap<(PathBuf, Fmt), (Stat, PathBuf, Option<String>)>,
    /// 源文件内容哈希 + 格式 → 输出。
    by_hash: HashMap<(String, Fmt), PathBuf>,
    ignored: HashMap<PathBuf, Stat>,
    /// 转换失败（损坏、不支持等）时的样子；没变就不再自动入队。
    failed: HashMap<PathBuf, Stat>,
    pub pending: Vec<Pending>,
}

fn non_empty(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.len() > 0)
}

impl History {
    /// 转过（任意格式）或被忽略。
    pub fn known(&self, path: &Path, stat: Stat) -> bool {
        let done = |f| {
            self.done
                .get(&(path.to_path_buf(), f))
                .is_some_and(|d| d.0 == stat)
        };
        done(Fmt::Original) || done(Fmt::Mp3) || self.ignored.get(path) == Some(&stat)
    }

    /// 同条件转过且输出仍在、非空。
    pub fn reusable(&self, k: &Key, f: Fmt) -> Option<PathBuf> {
        self.done
            .get(&(k.path.clone(), f))
            .filter(|d| d.0 == k.stat)
            .map(|d| d.1.clone())
            .filter(|o| non_empty(o))
    }

    /// 同内容同格式转过且输出仍在、非空。
    pub fn same_content(&self, hash: &str, f: Fmt) -> Option<PathBuf> {
        self.by_hash
            .get(&(hash.to_string(), f))
            .filter(|o| non_empty(o))
            .cloned()
    }

    pub fn record(&mut self, k: Key, f: Fmt, output: PathBuf, hash: Option<String>) {
        self.failed.remove(&k.path);
        if let Some(h) = &hash {
            self.by_hash.insert((h.clone(), f), output.clone());
        }
        self.done.insert((k.path, f), (k.stat, output, hash));
    }

    pub fn fail(&mut self, k: Key) {
        self.failed.insert(k.path, k.stat);
    }

    pub fn failed(&self, path: &Path, stat: Stat) -> bool {
        self.failed.get(path) == Some(&stat)
    }

    pub fn ignore(&mut self, keys: Vec<Key>) {
        self.ignored
            .extend(keys.into_iter().map(|k| (k.path, k.stat)));
    }
}

/// history.json 的磁盘形状。
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryFile {
    done: Vec<DoneRec>,
    ignored: Vec<Key>,
    failed: Vec<Key>,
    pending: Vec<Pending>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DoneRec {
    #[serde(flatten)]
    key: Key,
    format: Fmt,
    output: PathBuf,
    /// 源文件 SHA-256；旧记录没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hash: Option<String>,
}

impl From<HistoryFile> for History {
    fn from(f: HistoryFile) -> Self {
        let mut h = History {
            pending: f.pending,
            ..Default::default()
        };
        for d in f.done {
            h.record(d.key, d.format, d.output, d.hash);
        }
        h.ignore(f.ignored);
        for k in f.failed {
            h.fail(k);
        }
        h
    }
}

impl From<&History> for HistoryFile {
    fn from(h: &History) -> Self {
        HistoryFile {
            done: h
                .done
                .iter()
                .map(|((path, format), (stat, output, hash))| DoneRec {
                    key: Key {
                        path: path.clone(),
                        stat: *stat,
                    },
                    format: *format,
                    output: output.clone(),
                    hash: hash.clone(),
                })
                .collect(),
            ignored: h
                .ignored
                .iter()
                .map(|(path, stat)| Key {
                    path: path.clone(),
                    stat: *stat,
                })
                .collect(),
            failed: h
                .failed
                .iter()
                .map(|(path, stat)| Key {
                    path: path.clone(),
                    stat: *stat,
                })
                .collect(),
            pending: h.pending.clone(),
        }
    }
}

pub fn load<T: DeserializeOwned + Default>(p: &Path) -> T {
    let Ok(text) = std::fs::read(p) else {
        return T::default();
    };
    serde_json::from_slice(&text).unwrap_or_else(|_| {
        let _ = std::fs::rename(p, p.with_extension("json.bak"));
        T::default()
    })
}

/// 写到唯一的临时文件再 rename。调用方负责串行化。
pub fn save<T: Serialize>(p: &Path, v: &T) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let tmp = p.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let r = (|| {
        std::fs::create_dir_all(p.parent().unwrap_or(Path::new(".")))?;
        std::fs::write(&tmp, serde_json::to_vec(v)?)?;
        std::fs::rename(&tmp, p)
    })();
    if let Err(e) = r {
        let _ = std::fs::remove_file(&tmp);
        eprintln!("保存 {} 失败：{e}", p.display());
    }
}
