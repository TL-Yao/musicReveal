//! 文件夹扫描：监听找转换候选（跳过输出目录），资料库找可播放文件；都跳过隐藏项，深度与条目数有上限。

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

/// 监听扫描。`Err` 是给用户看的「不可访问」原因。输出目录本身或其内的文件夹扫出来是空的。
pub fn scan(root: &Path, skip: &Path) -> Result<Vec<(PathBuf, Stat)>, String> {
    let mut out = vec![];
    if root.starts_with(skip) {
        return Ok(out);
    }
    walk(root, 0, MAX_DEPTH, Some(skip), &candidate, &mut 0, &mut out)?;
    Ok(out)
}

/// 拖入的文件夹：只看一层，规则同监听扫描；超过条目上限时只取已看到的部分。
pub fn shallow(dir: &Path, skip: &Path) -> Vec<(PathBuf, Stat)> {
    let mut out = vec![];
    if !dir.starts_with(skip) {
        let _ = walk(dir, 0, 0, Some(skip), &candidate, &mut 0, &mut out);
    }
    out
}

/// 资料库扫描：`root` 下 `keep` 认可的文件，隐藏项、深度与条目上限同监听扫描；
/// 读不了的为空，超过上限时只取已看到的部分。
pub fn files(root: &Path, keep: &dyn Fn(&Path) -> bool) -> Vec<(PathBuf, Stat)> {
    let mut out = vec![];
    let _ = walk(root, 0, MAX_DEPTH, None, keep, &mut 0, &mut out);
    out
}

fn walk(
    dir: &Path,
    depth: u32,
    max_depth: u32,
    skip: Option<&Path>,
    keep: &dyn Fn(&Path) -> bool,
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
            if depth < max_depth && !skip.is_some_and(|s| p.starts_with(s)) {
                walk(&p, depth + 1, max_depth, skip, keep, n, out)?;
            }
        } else if ft.is_file() && keep(&p) {
            if let Some(s) = stat(&p) {
                out.push((p, s));
            }
        }
    }
    Ok(())
}
