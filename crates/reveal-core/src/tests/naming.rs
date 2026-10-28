use crate::convert::{file_stem, numbered};

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

#[test]
fn joins_all_artists_with_dunhao() {
    assert_eq!(
        file_stem(
            Some("交织together"),
            &s(&["DMYoung", "Hanser", "泠鸢yousa"]),
            "src"
        ),
        "DMYoung、Hanser、泠鸢yousa - 交织together"
    );
}

#[test]
fn missing_title_uses_fallback_and_missing_artist_uses_title_only() {
    assert_eq!(file_stem(None, &s(&["A"]), "原文件"), "A - 原文件");
    assert_eq!(file_stem(Some("歌"), &[], "x"), "歌");
    assert_eq!(file_stem(Some("  "), &[], "x"), "x");
}

#[test]
fn illegal_and_control_chars_replaced_and_edges_trimmed() {
    assert_eq!(
        file_stem(Some("a/b\\c:d*e?f\"g<h>i|j\tk. "), &s(&[" .AC/DC"]), "x"),
        "AC_DC - a_b_c_d_e_f_g_h_i_j_k"
    );
}

#[test]
fn long_chinese_name_truncated_on_char_boundary() {
    let dir = std::path::Path::new("/d");
    let stem = "歌".repeat(100); // 300 字节
    for (n, ext) in [(1, "flac"), (2, "mp3"), (123, "flac")] {
        let p = numbered(dir, &stem, n, ext);
        let name = p.file_name().unwrap().to_str().unwrap();
        assert!(name.len() <= 200, "{}", name.len());
        let suffix = if n == 1 {
            format!(".{ext}")
        } else {
            format!(" ({n}).{ext}")
        };
        let body = name.strip_suffix(&suffix).unwrap();
        assert!(
            !body.is_empty() && body.chars().all(|c| c == '歌'),
            "{name}"
        );
    }
}

#[test]
fn numbered_names() {
    let dir = std::path::Path::new("/d");
    let f = |n| {
        numbered(dir, "A - B", n, "mp3")
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(
        (f(1), f(2), f(3)),
        (
            "A - B.mp3".into(),
            "A - B (2).mp3".into(),
            "A - B (3).mp3".into()
        )
    );
}

use super::Tmp;

#[test]
fn ffmpeg_candidates_must_be_executable() {
    use std::os::unix::fs::PermissionsExt;
    let t = Tmp::new("ffexec");
    let (a, b) = (t.0.join("a"), t.0.join("b"));
    std::fs::write(&a, b"").unwrap();
    std::fs::write(&b, b"").unwrap();
    std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o755)).unwrap();
    let dir = t.0.clone();
    assert_eq!(
        crate::convert::first_executable([t.0.join("missing"), dir, a, b.clone()]),
        Some(b)
    );
}
