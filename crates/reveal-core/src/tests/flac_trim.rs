//! FLAC 尾部垃圾修剪：只在能确认末帧时截断。

use super::Tmp;
use crate::flac::trim_tail;
use std::path::Path;
use std::process::Command;

fn gen_flac(ff: &Path, out: &Path) -> Vec<u8> {
    let st = Command::new(ff)
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "sine=f=440:d=1.37",
        ])
        .args(["-ar", "48000", "-sample_fmt", "s32", "-c:a", "flac"])
        .arg(out)
        .status()
        .unwrap();
    assert!(st.success());
    std::fs::read(out).unwrap()
}

fn setup(tag: &str) -> Option<(Tmp, Vec<u8>)> {
    let Some(ff) = crate::find_ffmpeg() else {
        eprintln!("跳过：找不到 ffmpeg");
        return None;
    };
    let t = Tmp::new(tag);
    let clean = gen_flac(&ff, &t.0.join("clean.flac"));
    Some((t, clean))
}

fn trimmed(t: &Tmp, data: &[u8]) -> (bool, Vec<u8>) {
    let p = t.write("x.flac", data);
    let changed = trim_tail(&p).unwrap();
    (changed, std::fs::read(&p).unwrap())
}

#[test]
fn trailing_garbage_is_removed() {
    let Some((t, clean)) = setup("trim-junk") else {
        return;
    };
    let junk: Vec<u8> = (0..30u8)
        .map(|i| i.wrapping_mul(37).wrapping_add(0x23))
        .collect();
    let (changed, out) = trimmed(&t, &[&clean[..], &junk].concat());
    assert!(changed);
    assert!(out == clean);
}

#[test]
fn clean_flac_is_untouched() {
    let Some((t, clean)) = setup("trim-clean") else {
        return;
    };
    let (changed, out) = trimmed(&t, &clean);
    assert!(!changed);
    assert!(out == clean);
}

#[test]
fn garbage_containing_sync_code_is_not_mistaken_for_a_frame() {
    let Some((t, clean)) = setup("trim-sync") else {
        return;
    };
    // 末尾垃圾里放一个像帧头的 0xFFF8 / 0xFFF9 开头片段
    let mut junk = vec![
        0x47, 0x40, 0xFF, 0xF8, 0x69, 0x18, 0x00, 0x00, 0xFF, 0xF9, 0xC9,
    ];
    junk.extend((0..20u8).map(|i| i ^ 0x5A));
    junk.extend([0xFF, 0xF8]);
    let (changed, out) = trimmed(&t, &[&clean[..], &junk].concat());
    assert!(changed);
    assert!(out == clean);
}

#[test]
fn unknown_total_samples_or_non_flac_is_left_alone() {
    let Some((t, clean)) = setup("trim-unknown") else {
        return;
    };
    let mut data = [&clean[..], &[1u8; 30][..]].concat();
    // STREAMINFO 总样本数（字节 8+13 起 36 位）清零 = 未知
    data[8 + 13] &= 0xF0;
    data[8 + 14..8 + 18].fill(0);
    let (changed, out) = trimmed(&t, &data);
    assert!(!changed);
    assert!(out == data);
    let (changed, out) = trimmed(&t, b"not a flac file at all");
    assert!(!changed && out == b"not a flac file at all");
}
