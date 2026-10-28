//! 只经由 `sniff` / `decrypt` 测行为；合成样本在此生成，不入库二进制。
//! 样本构造法参考 music-geshizhuanhuan（MIT）tests/builders.py。

use super::*;
use aes::cipher::{BlockEncrypt, KeyInit};
use std::path::PathBuf;

// ---------- 通用工具 ----------

struct Tmp(PathBuf);
impl Tmp {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "reveal-core-{}-{}-{tag}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Tmp(dir)
    }
    fn write(&self, name: &str, data: &[u8]) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, data).unwrap();
        p
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 伪随机填充（xorshift），前面接容器魔数。
fn audio(magic: &[u8], len: usize) -> Vec<u8> {
    let mut v = magic.to_vec();
    let mut x: u32 = 0x1234_5678;
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        v.push(x as u8);
    }
    v
}

fn mp3(len: usize) -> Vec<u8> {
    audio(b"ID3\x03\x00\x00\x00\x00\x00\x00", len)
}

fn run(path: &Path, ekey: Option<&str>) -> Result<(SourceKind, Decrypted, Vec<u8>), Error> {
    let kind = sniff(path)?;
    let mut out = Vec::new();
    let d = decrypt(path, &kind, ekey, &mut out)?;
    Ok((kind, d, out))
}

// ---------- NCM ----------

const NCM_CORE_KEY: &[u8; 16] = b"hzHRAmso5kInbaxW";
const NCM_META_KEY: &[u8; 16] = b"#14ljk_!\\]&0U<'(";

fn aes_ecb_pkcs7(key: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let c = aes::Aes128::new(key.into());
    let pad = 16 - data.len() % 16;
    let mut v = data.to_vec();
    v.extend(std::iter::repeat_n(pad as u8, pad));
    for b in v.chunks_mut(16) {
        c.encrypt_block(b.into());
    }
    v
}

fn b64(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn build_ncm(plain: &[u8], rc4_key: &[u8], meta_json: &str, cover: Option<&[u8]>) -> Vec<u8> {
    let key_blob: Vec<u8> = aes_ecb_pkcs7(NCM_CORE_KEY, &[b"neteasecloudmusic", rc4_key].concat())
        .into_iter()
        .map(|b| b ^ 0x64)
        .collect();
    let meta_enc = aes_ecb_pkcs7(NCM_META_KEY, format!("music:{meta_json}").as_bytes());
    let meta_blob: Vec<u8> = format!("163 key(Don't modify):{}", b64(&meta_enc))
        .into_bytes()
        .into_iter()
        .map(|b| b ^ 0x63)
        .collect();
    // RC4-KSA + box 派生，音频字节 i 用 box[(i+1) % 256]
    let mut s: Vec<u8> = (0..=255).collect();
    let mut j = 0u8;
    for i in 0..256 {
        j = j
            .wrapping_add(s[i])
            .wrapping_add(rc4_key[i % rc4_key.len()]);
        s.swap(i, j as usize);
    }
    let bx: Vec<u8> = (0..256)
        .map(|i| {
            let a = s[i] as usize;
            s[(a + s[(i + a) & 0xff] as usize) & 0xff]
        })
        .collect();
    let mut out = b"CTENFDAM\0\0".to_vec();
    out.extend((key_blob.len() as u32).to_le_bytes());
    out.extend(&key_blob);
    out.extend((meta_blob.len() as u32).to_le_bytes());
    out.extend(&meta_blob);
    out.extend([0u8; 5]);
    let cover = cover.unwrap_or(&[]);
    out.extend((cover.len() as u32).to_le_bytes());
    out.extend((cover.len() as u32).to_le_bytes());
    out.extend(cover);
    out.extend(
        plain
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ bx[(i + 1) & 0xff]),
    );
    out
}

const META_MULTI: &str = r#"{"musicId":"1352449924","musicName":"雪乡游梦记（伴奏）","artist":[["著小生zoki","12558238"],["徵羽摩柯",0],["乐正龙牙",0],["Hanser","1049179"]],"album":"测试专辑","bitrate":320000,"duration":216032,"format":"mp3"}"#;

#[test]
fn ncm_decrypts_audio_and_metadata() {
    let t = Tmp::new("ncm");
    let plain = mp3(300_000);
    let cover = b"\x89PNG\r\n\x1a\nfake-cover";
    let p = t.write(
        "a.ncm",
        &build_ncm(&plain, b"0123456789abcdef", META_MULTI, Some(cover)),
    );
    let (kind, d, out) = run(&p, None).unwrap();
    assert_eq!(kind, SourceKind::Ncm);
    assert_eq!(out, plain);
    assert_eq!(d.container, Container::Mp3);
    let m = d.meta;
    assert_eq!(m.platform, Platform::Netease);
    assert_eq!(m.title.as_deref(), Some("雪乡游梦记（伴奏）"));
    assert_eq!(m.artists, ["著小生zoki", "徵羽摩柯", "乐正龙牙", "Hanser"]);
    assert_eq!(m.album.as_deref(), Some("测试专辑"));
    assert_eq!(m.song_id, Some(SongId::Netease(1352449924)));
    assert_eq!(m.cover.as_deref(), Some(&cover[..]));
}

#[test]
fn ncm_numeric_music_id_and_no_cover() {
    let t = Tmp::new("ncm2");
    let plain = audio(b"fLaC", 5000);
    let meta = r#"{"musicId":42,"musicName":"x","artist":[["a",1]],"format":"flac"}"#;
    let p = t.write("b.ncm", &build_ncm(&plain, b"k3y", meta, None));
    let (_, d, out) = run(&p, None).unwrap();
    assert_eq!(out, plain);
    assert_eq!(d.container, Container::Flac);
    assert_eq!(d.meta.song_id, Some(SongId::Netease(42)));
    assert_eq!(d.meta.cover, None);
}

#[test]
fn ncm_truncated_header_is_incomplete() {
    let t = Tmp::new("ncm3");
    let full = build_ncm(&mp3(1000), b"0123456789abcdef", META_MULTI, None);
    let p = t.write("c.ncm", &full[..60]);
    assert!(matches!(sniff(&p), Err(Error::Incomplete)));
}
