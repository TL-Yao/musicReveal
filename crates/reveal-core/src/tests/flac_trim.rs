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
    let ff = super::pipeline::ffmpeg()?;
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

// ---------- 手工构造的 FLAC（8bit 单声道 verbatim 子帧，PCM 字节可任意指定）----------

fn crc8(d: &[u8]) -> u8 {
    let mut c = 0u8;
    for &b in d {
        c ^= b;
        for _ in 0..8 {
            c = if c & 0x80 != 0 { (c << 1) ^ 7 } else { c << 1 };
        }
    }
    c
}

fn crc16(d: &[u8]) -> u16 {
    let mut c = 0u16;
    for &b in d {
        c ^= u16::from(b) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 {
                (c << 1) ^ 0x8005
            } else {
                c << 1
            };
        }
    }
    c
}

/// 帧头：固定块大小流，块大小用 16 位显式字段，44.1kHz，单声道 8bit。
fn frame_head(num: u64, samples: usize) -> Vec<u8> {
    let mut h = vec![0xFF, 0xF8, 0x79, 0x02];
    if num < 0x80 {
        h.push(num as u8);
    } else {
        h.extend([0xC0 | (num >> 6) as u8, 0x80 | (num & 0x3F) as u8]);
    }
    h.extend(((samples - 1) as u16).to_be_bytes());
    let c = crc8(&h);
    h.push(c);
    h
}

fn frame(num: u64, pcm: &[u8]) -> Vec<u8> {
    let mut f = frame_head(num, pcm.len());
    f.push(0x02); // verbatim 子帧
    f.extend(pcm);
    let c = crc16(&f);
    f.extend(c.to_be_bytes());
    f
}

fn flac_file(block: usize, frames_pcm: &[Vec<u8>]) -> Vec<u8> {
    let total: u64 = frames_pcm.iter().map(|p| p.len() as u64).sum();
    let mut v = b"fLaC".to_vec();
    v.extend([0x80, 0, 0, 34]);
    v.extend((block as u16).to_be_bytes());
    v.extend((block as u16).to_be_bytes());
    v.extend([0u8; 6]);
    v.extend(((44100u64 << 44) | (7 << 36) | total).to_be_bytes());
    v.extend([0u8; 16]);
    for (i, p) in frames_pcm.iter().enumerate() {
        v.extend(frame(i as u64, p));
    }
    v
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// 在 `pcm[at..]` 放一个与末帧帧头完全相同的伪帧头，并让 CRC-16 恰在其后某处归零。
fn plant_fake_frame(pcm: &mut [u8], at: usize, head: &[u8], body: usize) {
    let mut fake = head.to_vec();
    fake.push(0x02);
    fake.extend(std::iter::repeat_n(0x11, body));
    let c = crc16(&fake);
    fake.extend(c.to_be_bytes());
    pcm[at..at + fake.len()].copy_from_slice(&fake);
}

fn last_frame_with_fake(rng: &mut Rng, block: usize, frames: usize, last: usize) -> Vec<Vec<u8>> {
    let mut pcm: Vec<Vec<u8>> = (0..frames - 1).map(|_| rng.bytes(block)).collect();
    let mut tail = rng.bytes(last);
    let head = frame_head((frames - 1) as u64, last);
    plant_fake_frame(&mut tail, 10, &head, 20);
    pcm.push(tail);
    pcm
}

/// 构造器自检：参考解码器 libFLAC（`flac -t`）认可手工样本，含伪帧头的也合法。
/// （ffmpeg 的 FLAC 分帧器会被伪帧头骗到，不能当判据。）
#[test]
fn handmade_flac_passes_reference_decoder() {
    let Ok(st) = Command::new("flac").arg("--version").output() else {
        eprintln!("跳过：找不到 flac 命令行");
        return;
    };
    assert!(st.status.success());
    let t = Tmp::new("trim-hand");
    let mut rng = Rng(7);
    let plain = flac_file(256, &[rng.bytes(256), rng.bytes(256), rng.bytes(200)]);
    let fake = flac_file(256, &last_frame_with_fake(&mut rng, 256, 3, 200));
    for (name, data) in [("plain.flac", plain), ("fake.flac", fake)] {
        let p = t.write(name, &data);
        let out = Command::new("flac")
            .args(["-t", "-s"])
            .arg(&p)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn fake_header_inside_last_frame_pcm_is_not_cut() {
    let t = Tmp::new("trim-fake");
    let mut rng = Rng(42);
    let clean = flac_file(256, &last_frame_with_fake(&mut rng, 256, 3, 200));
    let (changed, out) = trimmed(&t, &clean);
    assert!(!changed);
    assert!(out == clean);
    // 带尾巴时仍按真末帧截
    let (changed, out) = trimmed(&t, &[&clean[..], &[0x47; 30]].concat());
    assert!(changed);
    assert!(out == clean);
}

#[test]
fn single_frame_flac_with_tail_is_trimmed() {
    let t = Tmp::new("trim-single");
    let clean = flac_file(300, &[Rng(3).bytes(123)]);
    let (changed, out) = trimmed(&t, &[&clean[..], b"tail-garbage-1234567890"].concat());
    assert!(changed && out == clean);
}

#[test]
fn random_valid_flacs_are_never_cut_and_tails_never_cut_wrong() {
    let t = Tmp::new("trim-prop");
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut cut = 0;
    for case in 0..60 {
        let block = 16 + (rng.next() % 300) as usize;
        let frames = 1 + (rng.next() % 6) as usize;
        let last = 1 + (rng.next() % block as u64) as usize;
        let mut pcm: Vec<Vec<u8>> = (0..frames - 1).map(|_| rng.bytes(block)).collect();
        pcm.push(rng.bytes(last));
        // 一半样本在任意帧的 PCM 里种伪末帧头
        if case % 2 == 0 {
            let k = (rng.next() % frames as u64) as usize;
            let head = frame_head((frames - 1) as u64, last);
            if pcm[k].len() >= head.len() + 24 {
                let at = (rng.next() % (pcm[k].len() - head.len() - 23) as u64) as usize;
                plant_fake_frame(&mut pcm[k], at, &head, 20);
            }
        }
        let clean = flac_file(block, &pcm);
        let (changed, out) = trimmed(&t, &clean);
        assert!(!changed && out == clean, "case {case}: 合法 FLAC 被改动");
        let n = 1 + (rng.next() % 64) as usize;
        let dirty = [&clean[..], &rng.bytes(n)].concat();
        let (_, out) = trimmed(&t, &dirty);
        assert!(out == clean || out == dirty, "case {case}: 截错位置");
        cut += usize::from(out == clean);
    }
    assert!(cut >= 55, "能确认的尾巴大多应被截掉：{cut}/60");
}
