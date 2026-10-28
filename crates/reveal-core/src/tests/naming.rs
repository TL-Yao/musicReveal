use crate::convert::{file_stem, unique_path};

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

#[test]
fn joins_all_artists_with_comma() {
    assert_eq!(
        file_stem(
            Some("交织together"),
            &s(&["DMYoung", "Hanser", "泠鸢yousa"]),
            "src"
        ),
        "DMYoung, Hanser, 泠鸢yousa - 交织together"
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
    let t = Tmp::new("name-long");
    let stem = "歌".repeat(100); // 300 字节
    let (p, n) = unique_path(&t.0, &stem, "flac");
    let name = p.file_name().unwrap().to_str().unwrap();
    assert!(name.len() <= 200, "{}", name.len());
    assert!(name.ends_with(".flac"));
    assert_eq!(
        name.trim_end_matches(".flac")
            .chars()
            .filter(|&c| c != '歌')
            .count(),
        0
    );
    assert_eq!(n, None);
}

#[test]
fn existing_target_gets_numbered() {
    let t = Tmp::new("name-dup");
    let (p, n) = unique_path(&t.0, "A - B", "mp3");
    assert_eq!(
        (p.file_name().unwrap().to_str().unwrap(), n),
        ("A - B.mp3", None)
    );
    std::fs::write(&p, b"").unwrap();
    let (p2, n2) = unique_path(&t.0, "A - B", "mp3");
    assert_eq!(
        (p2.file_name().unwrap().to_str().unwrap(), n2),
        ("A - B (2).mp3", Some(2))
    );
    std::fs::write(&p2, b"").unwrap();
    let (p3, n3) = unique_path(&t.0, "A - B", "mp3");
    assert_eq!(
        (p3.file_name().unwrap().to_str().unwrap(), n3),
        ("A - B (3).mp3", Some(3))
    );
    // 超长名加编号后仍 ≤ 200 字节
    let long = "歌".repeat(100);
    let (q, _) = unique_path(&t.0, &long, "mp3");
    std::fs::write(&q, b"").unwrap();
    let (q2, m) = unique_path(&t.0, &long, "mp3");
    assert_eq!(m, Some(2));
    assert!(q2.file_name().unwrap().len() <= 200);
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
