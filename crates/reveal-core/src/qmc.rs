//! QQ 音乐 QMC。v1 静态密钥表、扩展名清单移植自 music-geshizhuanhuan（MIT，
//! Copyright (c) 2026 music-geshizhuanhuan contributors）music_unlock/formats/qmc.py。
//! MIT 许可全文见 crates/reveal-core/THIRD_PARTY_LICENSES.md。

use crate::{Error, SourceKind};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

pub(crate) const V1_EXTS: &[&str] = &[
    "tkm", "bkcmp3", "bkcm4a", "bkcflac", "bkcwav", "bkcape", "bkcogg", "bkcwma", "666c6163",
    "6d7033", "6f6767", "6d3461", "776176",
];
pub(crate) const V2_EXTS: &[&str] = &[
    "mflac", "mflac0", "mgg", "mgg0", "mgg1", "mggl", "mmp4", "qmcflac", "qmcogg", "qmc0", "qmc2",
    "qmc3", "qmc4", "qmc6", "qmc8",
];

#[rustfmt::skip]
const V1_KEY: [u8; 128] = [
    0xC3, 0x4A, 0xD6, 0xCA, 0x90, 0x67, 0xF7, 0x52, 0xD8, 0xA1, 0x66, 0x62, 0x9F, 0x5B, 0x09, 0x00,
    0xC3, 0x5E, 0x95, 0x23, 0x9F, 0x13, 0x11, 0x7E, 0xD8, 0x92, 0x3F, 0xBC, 0x90, 0xBB, 0x74, 0x0E,
    0xC3, 0x47, 0x74, 0x3D, 0x90, 0xAA, 0x3F, 0x51, 0xD8, 0xF4, 0x11, 0x84, 0x9F, 0xDE, 0x95, 0x1D,
    0xC3, 0xC6, 0x09, 0xD5, 0x9F, 0xFA, 0x66, 0xF9, 0xD8, 0xF0, 0xF7, 0xA0, 0x90, 0xA1, 0xD6, 0xF3,
    0xC3, 0xF3, 0xD6, 0xA1, 0x90, 0xA0, 0xF7, 0xF0, 0xD8, 0xF9, 0x66, 0xFA, 0x9F, 0xD5, 0x09, 0xC6,
    0xC3, 0x1D, 0x95, 0xDE, 0x9F, 0x84, 0x11, 0xF4, 0xD8, 0x51, 0x3F, 0xAA, 0x90, 0x3D, 0x74, 0x47,
    0xC3, 0x0E, 0x74, 0xBB, 0x90, 0xBC, 0x3F, 0x92, 0xD8, 0x7E, 0x11, 0x13, 0x9F, 0x23, 0x95, 0x5E,
    0xC3, 0x00, 0x09, 0x5B, 0x9F, 0x62, 0x66, 0xA1, 0xD8, 0x52, 0xF7, 0x67, 0x90, 0xCA, 0xD6, 0x4A,
];

/// 超过 0x7FFF 的偏移按 % 0x7FFF 回绕（0x7FFF 本身不回绕）。
fn wrap(off: u64) -> u64 {
    if off > 0x7FFF {
        off % 0x7FFF
    } else {
        off
    }
}

/// 0x7FFF 周期内的掩码表（v1 静态 / v2 Map），或分段 RC4（主密钥 > 300 字节）。
pub(crate) enum Cipher {
    Table(Vec<u8>),
    Rc4(Rc4),
}

impl Cipher {
    pub(crate) fn static_v1() -> Self {
        Cipher::Table((0..=0x7FFF).map(|o| V1_KEY[o % 128]).collect())
    }

    /// Map 规则参考 qmunlock（MIT，Copyright (c) 2026 QM Unlock contributors）
    /// core/qmc2.rs `map_decrypt`：index = (o² + 71214) % n，按 ((index & 7) + 4) & 7
    /// 双向移位后 OR（不是普通循环移位）。
    pub(crate) fn new(master: &[u8]) -> Self {
        if master.len() > 300 {
            return Cipher::Rc4(Rc4::new(master));
        }
        let n = master.len() as u64;
        Cipher::Table(
            (0..=0x7FFFu64)
                .map(|o| {
                    let idx = ((o * o + 71214) % n) as usize;
                    let v = u32::from(master[idx]);
                    let s = ((idx & 7) + 4) & 7;
                    ((v << s) | (v >> s)) as u8
                })
                .collect(),
        )
    }

    pub(crate) fn xor(&self, buf: &mut [u8], off: u64) {
        match self {
            Cipher::Table(t) => {
                for (i, b) in buf.iter_mut().enumerate() {
                    *b ^= t[wrap(off + i as u64) as usize];
                }
            }
            Cipher::Rc4(r) => r.xor(buf, off),
        }
    }
}

const SEGMENT: u64 = 0x1400;

/// QMC2 分段 RC4。每段都从同一个 KSA 状态重新起步，故整段密钥流只需预生成
/// SEGMENT + n 字节，按段跳过量切片。
pub(crate) struct Rc4 {
    key: Vec<u8>,
    hash: f64,
    stream: Vec<u8>,
}

impl Rc4 {
    fn new(key: &[u8]) -> Self {
        let n = key.len();
        let mut s: Vec<u8> = (0..n).map(|i| i as u8).collect();
        let mut j = 0;
        for i in 0..n {
            j = (j + s[i] as usize + key[i] as usize) % n;
            s.swap(i, j);
        }
        let (mut i, mut j) = (0, 0);
        let stream = (0..SEGMENT as usize + n)
            .map(|_| {
                i = (i + 1) % n;
                j = (j + s[i] as usize) % n;
                s.swap(i, j);
                s[(s[i] as usize + s[j] as usize) % n]
            })
            .collect();
        let mut hash = 1u32;
        for &v in key.iter().filter(|&&v| v != 0) {
            let next = hash.wrapping_mul(u32::from(v));
            if next == 0 || next <= hash {
                break;
            }
            hash = next;
        }
        Rc4 {
            key: key.to_vec(),
            hash: f64::from(hash),
            stream,
        }
    }

    fn seg_key(&self, id: u64) -> usize {
        let n = self.key.len();
        let seed = self.key[(id % n as u64) as usize];
        if seed == 0 {
            return 0;
        }
        (self.hash / ((id + 1) as f64 * f64::from(seed)) * 100.0) as u64 as usize % n
    }

    fn xor(&self, buf: &mut [u8], off: u64) {
        let mut done = 0;
        while done < buf.len() {
            let at = off + done as u64;
            if at < 128 {
                buf[done] ^= self.key[self.seg_key(at)];
                done += 1;
                continue;
            }
            let in_seg = (at % SEGMENT) as usize;
            let take = (SEGMENT as usize - in_seg).min(buf.len() - done);
            let start = self.seg_key(at / SEGMENT) + in_seg;
            for (b, k) in buf[done..done + take].iter_mut().zip(&self.stream[start..]) {
                *b ^= k;
            }
            done += take;
        }
    }
}

// ---------- ekey 派生（Tencent TEA）----------
// 常量与 EncV2 双层结构参考 qmunlock core/qmc2.rs `derive_key`（MIT）及
// music-geshizhuanhuan music_unlock/ciphers.py（MIT）。

const SIMPLE_KEY: [u8; 8] = [0x69, 0x56, 0x46, 0x38, 0x2b, 0x20, 0x15, 0x0b];
const V2_PREFIX: &[u8] = b"QQMusic EncV2,Key:";
const V2_KEY1: &[u8; 16] = b"386ZJY!@#*$%^&)(";
const V2_KEY2: &[u8; 16] = b"**#!(#$%&^a1cZ,T";
const TEA_DELTA: u32 = 0x9E37_79B9;

fn tea_decrypt_block(blk: u64, k: &[u32; 4]) -> u64 {
    let mix = |v: u32, s: u32, a: u32, b: u32| {
        (v << 4).wrapping_add(a) ^ (v >> 5).wrapping_add(b) ^ s.wrapping_add(v)
    };
    let (mut hi, mut lo) = ((blk >> 32) as u32, blk as u32);
    let mut s = TEA_DELTA.wrapping_mul(16);
    for _ in 0..16 {
        lo = lo.wrapping_sub(mix(hi, s, k[2], k[3]));
        hi = hi.wrapping_sub(mix(lo, s, k[0], k[1]));
        s = s.wrapping_sub(TEA_DELTA);
    }
    (u64::from(hi) << 32) | u64::from(lo)
}

/// Tencent TEA tweaked-CBC 解密，剥掉 1+pad+2 字节头并校验 7 字节零尾。
fn tea_cbc_decrypt(data: &[u8], key: &[u8; 16]) -> Option<Vec<u8>> {
    if data.len() < 16 || !data.len().is_multiple_of(8) {
        return None;
    }
    let k: [u32; 4] =
        std::array::from_fn(|i| u32::from_be_bytes(key[i * 4..i * 4 + 4].try_into().unwrap()));
    let (mut prev, mut cur) = (0u64, 0u64);
    let mut plain = Vec::with_capacity(data.len());
    for ch in data.as_chunks::<8>().0 {
        let b = u64::from_be_bytes(*ch);
        let next = tea_decrypt_block(b ^ cur, &k);
        plain.extend((next ^ prev).to_be_bytes());
        prev = b;
        cur = next;
    }
    let start = 3 + (plain[0] & 7) as usize;
    let end = plain.len() - 7;
    if start > end || plain[end..].iter().any(|&b| b != 0) {
        return None;
    }
    Some(plain[start..end].to_vec())
}

fn b64(s: &[u8]) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim_ascii())
        .ok()
}

/// ekey（v1 或 "QQMusic EncV2,Key:" v2）→ QMC2 主密钥。
pub(crate) fn derive_key(ekey: &str) -> Option<Vec<u8>> {
    let mut d = b64(ekey.as_bytes())?;
    if let Some(rest) = d.strip_prefix(V2_PREFIX) {
        let inner = tea_cbc_decrypt(&tea_cbc_decrypt(rest, V2_KEY1)?, V2_KEY2)?;
        let end = inner.iter().position(|&b| b == 0).unwrap_or(inner.len());
        d = b64(&inner[..end])?;
    }
    if d.len() < 8 {
        return None;
    }
    let mut tk = [0u8; 16];
    for i in 0..8 {
        tk[i * 2] = SIMPLE_KEY[i];
        tk[i * 2 + 1] = d[i];
    }
    match tea_cbc_decrypt(&d[8..], &tk) {
        Some(body) => Some([&d[..8], &body[..]].concat()),
        // 在线接口可能直接返回已解码的原始密钥（qmunlock 同样兜底）
        None => Some(d),
    }
}

// ---------- 文件尾 ----------
// QTag / STag / PcV1Legacy 判别参考 music-geshizhuanhuan qmc.py `parse_footer`（MIT）。

const TAIL: u64 = 4096;
const MAX_FOOTER: u64 = 1 << 20;
const MAX_EKEY_LEN: usize = 0x500;

pub(crate) struct Footer {
    /// 文件尾占用字节数，音频长度 = 文件长度 - size。
    pub size: u64,
    /// `QmcV2` 或 `MusicEx`。
    pub kind: SourceKind,
}

enum Parsed {
    Found(Footer),
    Nothing,
    /// 声明的文件尾比已读的尾部长，需要再读这么多字节。
    Need(u64),
}

/// 从文件末尾读出 footer；`Ok(None)` 表示没有可识别的文件尾。
pub(crate) fn read_footer(f: &mut File) -> Result<Option<Footer>, Error> {
    let file_len = f.metadata()?.len();
    let mut want = TAIL;
    loop {
        f.seek(SeekFrom::Start(file_len.saturating_sub(want)))?;
        let mut tail = Vec::new();
        (&mut *f).take(want).read_to_end(&mut tail)?;
        match parse_footer(&tail, file_len)? {
            Parsed::Found(ft) => return Ok(Some(ft)),
            Parsed::Nothing => return Ok(None),
            Parsed::Need(n) if n > want => want = n,
            Parsed::Need(_) => return Err(Error::Corrupt("文件尾长度不一致".into())),
        }
    }
}

/// 校验文件尾声明的总长 `n`：低于格式最小值或超上限是真非法；超出文件是数据还没到。
fn check_len(n: u64, min: u64, tail: &[u8], file_len: u64, what: &str) -> Result<bool, Error> {
    if n < min || n > MAX_FOOTER {
        return Err(Error::Corrupt(format!("{what} 长度非法 {n}")));
    }
    if n > file_len {
        return Err(Error::Incomplete);
    }
    Ok(n <= tail.len() as u64)
}

fn u32_at(b: &[u8], at: usize, be: bool) -> u64 {
    let raw: [u8; 4] = b[at..at + 4].try_into().unwrap();
    u64::from(if be {
        u32::from_be_bytes(raw)
    } else {
        u32::from_le_bytes(raw)
    })
}

/// musicex V1：[0x0C..0x48) song mid、[0x48..0xB0) 媒体文件名（UTF-16LE，NUL 结尾），
/// 末 16 字节 = u32 尾长 + u32 版本 + "musicex\0"。布局参考 qmunlock core/footer.rs（MIT）。
fn musicex(tail: &[u8], file_len: u64) -> Result<Parsed, Error> {
    let Some(t) = tail.len().checked_sub(16).map(|i| &tail[i..]) else {
        return Err(Error::Incomplete);
    };
    let size = u32_at(t, 0, false);
    if !check_len(size, 0xC0, tail, file_len, "musicex")? {
        return Ok(Parsed::Need(size));
    }
    let version = u32_at(t, 4, false);
    if version != 1 {
        return Err(Error::Unsupported(format!("musicex 版本 {version}")));
    }
    let f = &tail[tail.len() - size as usize..];
    let utf16 = |r: std::ops::Range<usize>| {
        let w: Vec<u16> = f[r]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .take_while(|&c| c != 0)
            .collect();
        String::from_utf16(&w).ok().filter(|s| !s.is_empty())
    };
    let (Some(song_mid), Some(media_filename)) = (utf16(0x0C..0x48), utf16(0x48..0xB0)) else {
        return Err(Error::Corrupt("musicex 缺少歌曲 mid 或媒体文件名".into()));
    };
    Ok(Parsed::Found(Footer {
        size,
        kind: SourceKind::MusicEx {
            song_mid,
            media_filename,
        },
    }))
}

fn is_b64(s: &[u8]) -> bool {
    !s.is_empty()
        && s.iter()
            .all(|&c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/' || c == b'=')
}

/// QTag：[ekey,资源 id,2][u32 大端长度]"QTag"。
fn qtag(tail: &[u8], file_len: u64) -> Result<Parsed, Error> {
    let size = u32_at(tail, tail.len() - 8, true) + 8;
    if !check_len(size, "A,1,2".len() as u64 + 8, tail, file_len, "QTag")? {
        return Ok(Parsed::Need(size));
    }
    let csv = &tail[tail.len() - size as usize..tail.len() - 8];
    let parts: Vec<&[u8]> = csv.split(|&b| b == b',').collect();
    if parts.len() != 3
        || parts[2] != b"2"
        || !parts[1].iter().all(u8::is_ascii_digit)
        || !is_b64(parts[0])
    {
        return Err(Error::Corrupt("QTag 内容非法".into()));
    }
    Ok(Parsed::Found(Footer {
        size,
        kind: SourceKind::QmcV2 {
            ekey: String::from_utf8_lossy(parts[0]).into_owned(),
        },
    }))
}

fn parse_footer(tail: &[u8], file_len: u64) -> Result<Parsed, Error> {
    if tail.ends_with(b"musicex\0") {
        return musicex(tail, file_len);
    }
    if tail.len() < 8 {
        return Ok(Parsed::Nothing);
    }
    if tail.ends_with(b"STag") {
        return Err(Error::Unsupported(
            "STag 格式（安卓端，文件内无 ekey）暂不支持".into(),
        ));
    }
    if tail.ends_with(b"QTag") {
        return qtag(tail, file_len);
    }
    // PcV1Legacy：[ekey][u32 小端长度]，无魔数，只能按形状猜；对不上就当没有文件尾。
    let n = u32_at(tail, tail.len() - 4, false) as usize;
    if n == 0 || n > MAX_EKEY_LEN || n + 4 > tail.len() {
        return Ok(Parsed::Nothing);
    }
    let ekey = &tail[tail.len() - 4 - n..tail.len() - 4];
    let ekey = &ekey[..ekey.iter().position(|&b| b == 0).unwrap_or(ekey.len())];
    if !is_b64(ekey) {
        return Ok(Parsed::Nothing);
    }
    Ok(Parsed::Found(Footer {
        size: n as u64 + 4,
        kind: SourceKind::QmcV2 {
            ekey: String::from_utf8_lossy(ekey).into_owned(),
        },
    }))
}
