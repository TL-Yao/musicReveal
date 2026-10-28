//! 文件夹扫描：只找加密格式候选文件，跳过隐藏项与输出目录，深度与条目数有上限。

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// 根目录之下最多再进 3 层子目录。
const MAX_DEPTH: u32 = 3;
const MAX_ENTRIES: u32 = 5000;

/// 大小 + mtime（纳秒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Stat {
    pub size: u64,
    pub mtime: u64,
}

pub fn stat(p: &Path) -> Option<Stat> {
    let m = std::fs::metadata(p).ok().filter(|m| m.is_file())?;
    let mtime = m
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos() as u64;
    Some(Stat {
        size: m.len(),
        mtime,
    })
}

fn hidden(p: &Path) -> bool {
    p.file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
}

/// 可作为转换源的文件：非隐藏、扩展名是加密格式。
pub fn candidate(p: &Path) -> bool {
    !hidden(p) && reveal_core::is_candidate(p)
}

/// 监听扫描。`Err` 是给用户看的「不可访问」原因。
pub fn scan(root: &Path, skip: &Path) -> Result<Vec<(PathBuf, Stat)>, String> {
    let mut out = vec![];
    let mut n = 0;
    walk(root, 0, skip, &mut n, &mut out)?;
    Ok(out)
}

fn walk(
    dir: &Path,
    depth: u32,
    skip: &Path,
    n: &mut u32,
    out: &mut Vec<(PathBuf, Stat)>,
) -> Result<(), String> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) if depth > 0 => return Ok(()),
        Err(e) => {
            return Err(match e.kind() {
                std::io::ErrorKind::NotFound => "文件夹不存在".into(),
                std::io::ErrorKind::PermissionDenied => "没有访问权限".into(),
                _ => format!("无法读取：{e}"),
            })
        }
    };
    for e in rd.flatten() {
        *n += 1;
        if *n > MAX_ENTRIES {
            return Err(format!("文件太多（超过 {MAX_ENTRIES} 个）"));
        }
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if hidden(&p) {
            continue;
        }
        if ft.is_dir() {
            if depth < MAX_DEPTH && !p.starts_with(skip) {
                walk(&p, depth + 1, skip, n, out)?;
            }
        } else if ft.is_file() && candidate(&p) {
            if let Some(s) = stat(&p) {
                out.push((p, s));
            }
        }
    }
    Ok(())
}

/// 拖入的文件夹：只看一层。
pub fn shallow(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return vec![];
    };
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && candidate(p))
        .collect()
}
