//! 持久化：settings.json 与 history.json。原子写；损坏时备份为 .bak 后用默认值。

use super::model::Fmt;
use super::scan::Stat;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Done {
    #[serde(flatten)]
    pub key: Key,
    pub format: Fmt,
    pub output: PathBuf,
}

// ponytail: 线性查找，记录上万条后改 HashMap
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    pub done: Vec<Done>,
    pub ignored: Vec<Key>,
}

impl History {
    /// 转过（任意格式）或被忽略。
    pub fn known(&self, k: &Key) -> bool {
        self.done.iter().any(|d| d.key == *k) || self.ignored.contains(k)
    }
    /// 同条件转过且输出仍在、非空。
    pub fn reusable(&self, k: &Key, f: Fmt) -> Option<PathBuf> {
        self.done
            .iter()
            .rev()
            .find(|d| d.key == *k && d.format == f)
            .map(|d| d.output.clone())
            .filter(|o| std::fs::metadata(o).is_ok_and(|m| m.len() > 0))
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

pub fn save<T: Serialize>(p: &Path, v: &T) {
    let tmp = p.with_extension("json.tmp");
    let r = (|| {
        std::fs::create_dir_all(p.parent().unwrap_or(Path::new(".")))?;
        std::fs::write(&tmp, serde_json::to_vec_pretty(v)?)?;
        std::fs::rename(&tmp, p)
    })();
    if let Err(e) = r {
        eprintln!("保存 {} 失败：{e}", p.display());
    }
}
