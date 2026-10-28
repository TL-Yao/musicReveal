//! FLAC 尾部清理：部分源文件（如 QQ mflac）在末帧之后带有若干字节非音频数据，
//! 播放器 / 解码器会因此报错。只在能确认末帧时截断，否则原样保留。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const WINDOW: u64 = 1 << 20;

fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |mut c, &b| {
        c ^= b;
        for _ in 0..8 {
            c = if c & 0x80 != 0 {
                (c << 1) ^ 0x07
            } else {
                c << 1
            };
        }
        c
    })
}

fn crc16_step(mut c: u16, b: u8) -> u16 {
    c ^= u16::from(b) << 8;
    for _ in 0..8 {
        c = if c & 0x8000 != 0 {
            (c << 1) ^ 0x8005
        } else {
            c << 1
        };
    }
    c
}

/// 解析帧头：返回（帧头长度，起始样本号，本帧样本数）；CRC-8 不过或字段非法返回 None。
/// `fixed_block` 为 STREAMINFO 的块大小，用于固定块大小流的帧号换算。
fn frame_header(h: &[u8], fixed_block: u64) -> Option<(usize, u64, u64)> {
    if h.len() < 6 || h[0] != 0xFF || h[1] & 0xFE != 0xF8 {
        return None;
    }
    let (bs_code, sr_code) = (h[2] >> 4, h[2] & 0x0F);
    let (ch, ss) = (h[3] >> 4, (h[3] >> 1) & 7);
    if bs_code == 0 || sr_code == 15 || ch > 10 || ss == 3 || h[3] & 1 != 0 {
        return None;
    }
    // UTF-8 式变长编码的帧号 / 样本号
    let lead = h[4].leading_ones() as usize;
    let n = match lead {
        0 => 1,
        2..=7 => lead,
        _ => return None,
    };
    let mut num = u64::from(h[4] & (0x7F >> lead));
    for i in 5..4 + n {
        let b = *h.get(i)?;
        if b & 0xC0 != 0x80 {
            return None;
        }
        num = (num << 6) | u64::from(b & 0x3F);
    }
    let mut p = 4 + n;
    let mut take = |k: usize| -> Option<u64> {
        let v = h
            .get(p..p + k)?
            .iter()
            .fold(0u64, |a, &b| (a << 8) | u64::from(b));
        p += k;
        Some(v)
    };
    let block = match bs_code {
        1 => 192,
        2..=5 => 576 << (bs_code - 2),
        6 => take(1)? + 1,
        7 => take(2)? + 1,
        _ => 256 << (bs_code - 8),
    };
    match sr_code {
        12 => take(1)?,
        13 | 14 => take(2)?,
        _ => 0,
    };
    if crc8(&h[..p]) != *h.get(p)? {
        return None;
    }
    let start = if h[1] & 1 == 0 {
        num * fixed_block
    } else {
        num
    };
    Some((p + 1, start, block))
}

/// 截掉末帧之后的字节。必须同时满足：帧头 CRC-8 通过、帧的样本范围恰好结束于
/// STREAMINFO 总样本数、帧尾 CRC-16 在唯一一个位置成立。返回是否截断。
pub(crate) fn trim_tail(path: &Path) -> std::io::Result<bool> {
    let mut f = File::options().read(true).write(true).open(path)?;
    let len = f.metadata()?.len();
    let mut head = [0u8; 42];
    if len < 42 || f.read_exact(&mut head).is_err() || &head[..4] != b"fLaC" || head[4] & 0x7F != 0
    {
        return Ok(false);
    }
    let si = &head[8..];
    let block = u64::from(u16::from_be_bytes([si[2], si[3]]));
    let total = (u64::from(si[13] & 0x0F) << 32)
        | u64::from(u32::from_be_bytes(si[14..18].try_into().unwrap()));
    if total == 0 {
        return Ok(false);
    }
    let base = len.saturating_sub(WINDOW).max(42);
    f.seek(SeekFrom::Start(base))?;
    let mut w = Vec::new();
    (&mut f).take(len - base).read_to_end(&mut w)?;

    for i in (0..w.len().saturating_sub(1)).rev() {
        let Some((hlen, start, n)) = frame_header(&w[i..], block) else {
            continue;
        };
        if start + n != total {
            continue;
        }
        // 帧含 CRC-16 在内整体余数为 0 的位置即帧尾候选，要求唯一
        let mut crc = 0u16;
        let mut ends = vec![];
        for (k, &b) in w[i..].iter().enumerate() {
            crc = crc16_step(crc, b);
            if crc == 0 && k > hlen {
                ends.push(k + 1);
            }
        }
        return Ok(match ends[..] {
            [end] if i + end < w.len() => {
                f.set_len(base + (i + end) as u64)?;
                true
            }
            _ => false,
        });
    }
    Ok(false)
}
