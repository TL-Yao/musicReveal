//! 网易云 NCM。结构与密钥参考 music-geshizhuanhuan（MIT，Copyright (c) 2026
//! music-geshizhuanhuan contributors）music_unlock/formats/ncm.py 与 ciphers.py。
//! MIT 许可全文见 crates/reveal-core/THIRD_PARTY_LICENSES.md。

use crate::{eof, pump, Decrypted, EmbeddedMeta, Error, Platform, SongId};
use aes::cipher::{BlockDecrypt, KeyInit};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

pub(crate) const MAGIC: &[u8] = b"CTENFDAM";
const CORE_KEY: &[u8; 16] = b"hzHRAmso5kInbaxW";
const META_KEY: &[u8; 16] = b"#14ljk_!\\]&0U<'(";
const META_PREFIX: &[u8] = b"163 key(Don't modify):";

pub(crate) struct Header {
    rc4_key: Vec<u8>,
    meta: Option<serde_json::Value>,
    cover: Option<Vec<u8>>,
}

fn u32le(f: &mut File) -> Result<u32, Error> {
    let mut b = [0u8; 4];
    f.read_exact(&mut b).map_err(eof)?;
    Ok(u32::from_le_bytes(b))
}

/// 长度字段不可信：先按区块上限拒绝（防止按伪造长度分配），再看数据是否已到。
fn block(f: &mut File, len: u32, max: u32, file_len: u64, what: &str) -> Result<Vec<u8>, Error> {
    if len > max {
        return Err(Error::Corrupt(format!("NCM {what}长度非法 {len}")));
    }
    if f.stream_position()? + len as u64 > file_len {
        return Err(Error::Incomplete);
    }
    let mut v = vec![0u8; len as usize];
    f.read_exact(&mut v).map_err(eof)?;
    Ok(v)
}

fn aes_ecb_unpad(key: &[u8; 16], mut data: Vec<u8>) -> Option<Vec<u8>> {
    if data.is_empty() || !data.len().is_multiple_of(16) {
        return None;
    }
    let c = aes::Aes128::new(key.into());
    for b in data.chunks_mut(16) {
        c.decrypt_block(b.into());
    }
    let pad = *data.last()? as usize;
    if pad == 0 || pad > 16 || data[data.len() - pad..].iter().any(|&b| b as usize != pad) {
        return None;
    }
    data.truncate(data.len() - pad);
    Some(data)
}

fn parse_meta(blob: &[u8]) -> Option<serde_json::Value> {
    use base64::Engine;
    let x: Vec<u8> = blob.iter().map(|b| b ^ 0x63).collect();
    let enc = base64::engine::general_purpose::STANDARD
        .decode(x.strip_prefix(META_PREFIX)?)
        .ok()?;
    let plain = aes_ecb_unpad(META_KEY, enc)?;
    // "music:{...}"（电台节目为 "dj:{...}"）
    let colon = plain.iter().position(|&b| b == b':')?;
    serde_json::from_slice(&plain[colon + 1..]).ok()
}

/// 读完头部后文件指针停在音频流起点。
pub(crate) fn read_header(f: &mut File) -> Result<Header, Error> {
    let file_len = f.metadata()?.len();
    f.seek(SeekFrom::Start(10))?; // magic(8) + 2 字节间隙
    let n = u32le(f)?;
    let key_blob: Vec<u8> = block(f, n, 4 << 10, file_len, "密钥区")?
        .iter()
        .map(|b| b ^ 0x64)
        .collect();
    let key = aes_ecb_unpad(CORE_KEY, key_blob)
        .filter(|k| k.len() > 17)
        .ok_or_else(|| Error::Corrupt("NCM 密钥区解密失败".into()))?;
    let n = u32le(f)?;
    let meta = parse_meta(&block(f, n, 1 << 20, file_len, "元数据")?);
    f.seek(SeekFrom::Current(5))?; // CRC(4) + 1 字节间隙
    let space = u32le(f)?;
    let size = u32le(f)?;
    if size > space {
        return Err(Error::Corrupt("NCM 封面长度非法".into()));
    }
    let cover = block(f, size, 32 << 20, file_len, "封面")?;
    f.seek(SeekFrom::Current((space - size) as i64))?;
    if f.stream_position()? >= file_len {
        return Err(Error::Incomplete);
    }
    Ok(Header {
        rc4_key: key[17..].to_vec(), // 去掉 "neteasecloudmusic"
        meta,
        cover: (!cover.is_empty()).then_some(cover),
    })
}

fn key_box(key: &[u8]) -> [u8; 256] {
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    std::array::from_fn(|i| {
        let a = s[i] as usize;
        s[(a + s[(i + a) & 0xff] as usize) & 0xff]
    })
}

fn meta_of(v: Option<&serde_json::Value>, cover: Option<Vec<u8>>) -> EmbeddedMeta {
    let s = |k: &str| v.and_then(|v| v[k].as_str()).map(str::to_owned);
    let artists = v
        .and_then(|v| v["artist"].as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x[0].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let id = v.and_then(|v| {
        let id = &v["musicId"];
        id.as_u64().or_else(|| id.as_str()?.parse().ok())
    });
    EmbeddedMeta {
        title: s("musicName"),
        artists,
        album: s("album"),
        cover,
        song_id: id.map(SongId::Netease),
        platform: Platform::Netease,
    }
}

pub(crate) fn decrypt(mut f: File, out: &mut dyn Write) -> Result<Decrypted, Error> {
    let h = read_header(&mut f)?;
    let bx = key_box(&h.rc4_key);
    let container = pump(
        f,
        out,
        |buf, off| {
            for (i, b) in buf.iter_mut().enumerate() {
                *b ^= bx[((off as usize) + i + 1) & 0xff];
            }
        },
        || Err(Error::Corrupt("NCM 解密后不是可识别的音频".into())),
    )?;
    Ok(Decrypted {
        container,
        meta: meta_of(h.meta.as_ref(), h.cover),
    })
}
