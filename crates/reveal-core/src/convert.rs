//! 一首歌的完整转换：解密 → 可选转码 → 标签 → 命名落盘。

use std::path::{Path, PathBuf};

const MAX_NAME: usize = 200;

fn clean(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '.')
}

/// `<歌手> - <歌名>`，非法字符换成 `_`，未截断。
pub(crate) fn file_stem(title: Option<&str>, artists: &[String], fallback: &str) -> String {
    let title = title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(fallback);
    let artists: Vec<&str> = artists
        .iter()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty())
        .collect();
    let raw = if artists.is_empty() {
        title.to_string()
    } else {
        format!("{} - {title}", artists.join(", "))
    };
    let s: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || r#"/\:*?"<>|"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    clean(&s).to_string()
}

fn truncate(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    clean(&s[..end])
}

/// 目录里第一个不存在的 `stem.ext` / `stem (n).ext`，整个文件名 ≤ 200 字节。
pub(crate) fn unique_path(dir: &Path, stem: &str, ext: &str) -> (PathBuf, Option<u32>) {
    let mut n = 1u32;
    loop {
        let suffix = if n == 1 {
            format!(".{ext}")
        } else {
            format!(" ({n}).{ext}")
        };
        let p = dir.join(format!(
            "{}{suffix}",
            truncate(stem, MAX_NAME - suffix.len())
        ));
        if !p.exists() {
            return (p, (n > 1).then_some(n));
        }
        n += 1;
    }
}
