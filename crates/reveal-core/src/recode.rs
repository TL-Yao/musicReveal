//! 输出文件夹里已有的歌在 FLAC / WAV 与 ALAC 之间无损互转：隐藏副本上转好、逐项比对一致才替换。

use crate::convert::{
    lyrics_key, lyrics_of, place, read_audio, tags_of, CancelToken, ConvertError, Parts,
};
use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, FileType, TaggedFile, TaggedFileExt};
use lofty::mp4::{AtomData, AtomIdent, Mp4Codec, Mp4File};
use lofty::picture::PictureType;
use lofty::prelude::ItemKey;
use lofty::tag::{ItemValue, Tag, TagExt, TagType};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

/// 改写用户已有音频文件（补嵌歌词、无损互转）时持有：同一进程里不会两边同时改同一首。
// ponytail: 全局一把锁，改写互相排队；要并行再按路径分锁
static REWRITE: Mutex<()> = Mutex::new(());

pub(crate) fn rewrite_lock() -> MutexGuard<'static, ()> {
    REWRITE.lock().unwrap_or_else(|e| e.into_inner())
}

/// 互转的目标格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lossless {
    /// FLAC / WAV → ALAC（.m4a）。
    Alac,
    /// ALAC（.m4a）→ FLAC。
    Flac,
}

fn mp4_codec(p: &Path) -> Option<Mp4Codec> {
    let mut f = std::fs::File::open(p).ok()?;
    let opts = ParseOptions::new().read_tags(false).read_cover_art(false);
    Mp4File::read_from(&mut f, opts).ok()?.properties().codec()
}

/// MP4 容器里是 ALAC 编码。
pub fn is_alac(p: &Path) -> bool {
    mp4_codec(p) == Some(Mp4Codec::ALAC)
}

pub(crate) fn is_aac(p: &Path) -> bool {
    mp4_codec(p) == Some(Mp4Codec::AAC)
}

/// `src`（读自 `path`）能不能无损转成 `to`，不能时给出原因。只收 16 / 24 bit 整数、单声道或立体声
/// （更多声道 ALAC 往返保不住声道数与布局），元数据结构须在白名单里（见 `structure`）。
fn accepts(src: &TaggedFile, path: &Path, to: Lossless) -> Result<(), String> {
    format_ok(src, path, to)?;
    if !matches!(src.properties().channels(), Some(1 | 2)) {
        return Err("只转单声道与立体声，未转换".into());
    }
    structure(path, src.file_type()).map_err(|w| format!("有无法原样转换的元数据（{w}），未转换"))
}

fn format_ok(src: &TaggedFile, path: &Path, to: Lossless) -> Result<(), String> {
    match to {
        Lossless::Alac => {
            if !matches!(src.file_type(), FileType::Flac | FileType::Wav) {
                return Err("不是 FLAC 或 WAV，未转换".into());
            }
            let p = src.properties();
            let bits = p.bit_depth();
            if !matches!(bits, Some(16 | 24)) {
                let b = bits.map_or("未知".into(), |b| b.to_string());
                return Err(format!("位深 {b} bit 无法无损转成 ALAC，未转换"));
            }
            Ok(())
        }
        Lossless::Flac if src.file_type() == FileType::Mp4 && is_alac(path) => Ok(()),
        Lossless::Flac => Err("不是 ALAC，未转换".into()),
    }
}

/// `recode_lossless(path, to)` 会去转它（普通文件、不是只读、格式与位深合适）；
/// 不会去转的不算进待转数。
pub fn recodable(path: &Path, to: Lossless) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && !m.permissions().readonly())
        && read_audio(path).is_ok_and(|f| accepts(&f, path, to).is_ok())
}

/// 两种格式间都能原样表达、并经 ffmpeg 独立核对过的标签（Vorbis 名）：本应用自己写出的就是这些。
/// 其余任何键（自定义键、TXXX、MP4 自由格式原子）一律不转。
const KNOWN: &[&str] = &[
    "TITLE",
    "ARTIST",
    "ALBUM",
    "ALBUMARTIST",
    "DATE",
    "GENRE",
    "COMPOSER",
    "COMMENT",
    "DESCRIPTION",
    "TRACKNUMBER",
    "TRACKTOTAL",
    "DISCNUMBER",
    "DISCTOTAL",
    "LYRICS",
    "ENCODER",
];

/// 源文件的元数据结构是否在白名单里，不在时说出是哪一处。只读文件头部与元数据，不碰音频数据。
///
/// - FLAC：以 `fLaC` 开头（前面不带 ID3），只有 STREAMINFO、PADDING、SEEKTABLE、VORBIS_COMMENT
///   （至多一个）、PICTURE（至多一张，正面封面、无描述）块；文件尾没有 ID3v1 / APE 标签。
/// - WAV：RIFF/WAVE，只有 `fmt `、`data`、`JUNK`/`junk`/`PAD `/`FLLR` 填充、至多一个 `LIST`（须是
///   INFO，子项逐个枚举、恰好铺满 LIST、同一项至多一次）、至多一个 `id3 `/`ID3 ` 块。ID3 须是
///   v2.3 / v2.4、标签头与各帧都没有标志位，帧与 padding 恰好铺满声明的大小、padding 与标签之后
///   只有 0；原始帧逐个枚举，同一帧 ID 至多一次；只有对应 `KNOWN` 的标准文本帧与至多
///   一张正面封面、无描述的 APIC（TXXX、COMM、USLT 等一律不收）。
/// - MP4（ALAC）：顶层只有 ftyp、moov、mdat、free、skip、wide；moov 下只有 mvhd、iods、一条 trak、
///   至多一个 udta（trak 下只有 tkhd、edts、mdia）；udta 下只有一个 meta，其 hdlr 是 mdir、下面只有
///   hdlr、恰好一个 ilst 与填充；盒子长度恰好嵌套；ilst 里每个原子只有一个值、data 类型是该键的规范
///   类型（文本 UTF-8，trkn / disk 二进制，gnre 整数，covr JPEG / PNG），covr 至多一张，没有自由格式
///   （`----`）原子。
/// - 所有格式的标签项：键在 `KNOWN` 里且就是规范名（别名不收）；同一项（大小写、别名、「描述 / 注释」
///   都算同一项）只出现一次；值非空、没有 NUL（多值）；曲序、碟号及其总数是 1–65535 的规范十进制数，
///   有总数就得有序号。
fn structure(path: &Path, ty: FileType) -> Result<(), String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let items = match ty {
        FileType::Flac => flac_items(&mut f),
        FileType::Wav => wav_items(&mut f),
        FileType::Mp4 => mp4_items(&mut f),
        _ => Err("格式".into()),
    }?;
    check_items(&items)
}

type Items = Vec<(String, String)>;

fn rd(f: &mut std::fs::File, n: usize) -> Result<Vec<u8>, String> {
    let mut b = vec![0; n];
    f.read_exact(&mut b)
        .map_err(|_| "文件结构损坏".to_string())?;
    Ok(b)
}

fn skip(f: &mut std::fs::File, n: u64) -> Result<(), String> {
    f.seek(SeekFrom::Current(n as i64))
        .map(drop)
        .map_err(|e| e.to_string())
}

fn u32be(b: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize)
}

fn u32le(b: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize)
}

fn fourcc(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

fn flac_items(f: &mut std::fs::File) -> Result<Items, String> {
    if rd(f, 4)? != b"fLaC" {
        return Err("FLAC 前的附加数据".into());
    }
    let (mut items, mut vcs, mut pics) = (vec![], 0, 0);
    loop {
        let h = rd(f, 4)?;
        let len = u32::from_be_bytes([0, h[1], h[2], h[3]]) as usize;
        match h[0] & 0x7f {
            0 | 1 | 3 => skip(f, len as u64)?,
            4 => {
                vcs += 1;
                items.extend(parse_vc(&rd(f, len)?).ok_or("Vorbis 注释损坏")?);
            }
            6 => {
                pics += 1;
                let b = rd(f, len)?;
                let desc = u32be(&b, 4).and_then(|m| u32be(&b, 8 + m));
                if u32be(&b, 0) != Some(3) || desc != Some(0) {
                    return Err("图片不是无描述的正面封面".into());
                }
            }
            2 => return Err("APPLICATION 块".into()),
            5 => return Err("CUESHEET 块".into()),
            t => return Err(format!("{t} 号元数据块")),
        }
        if h[0] & 0x80 != 0 {
            break;
        }
    }
    if vcs > 1 {
        return Err("多个 Vorbis 注释块".into());
    }
    if pics > 1 {
        return Err("多张图片".into());
    }
    let n = f.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    if n >= 160 {
        f.seek(SeekFrom::End(-160)).map_err(|e| e.to_string())?;
        let t = rd(f, 160)?;
        if &t[32..35] == b"TAG" || &t[128..136] == b"APETAGEX" {
            return Err("文件尾的 ID3v1 / APE 标签".into());
        }
    }
    Ok(items)
}

fn parse_vc(b: &[u8]) -> Option<Items> {
    let mut at = 4 + u32le(b, 0)?;
    let n = u32le(b, at)?;
    at += 4;
    let mut out = vec![];
    for _ in 0..n {
        let len = u32le(b, at)?;
        let c = std::str::from_utf8(b.get(at + 4..at + 4 + len)?).ok()?;
        at += 4 + len;
        let (k, v) = c.split_once('=')?;
        out.push((k.to_owned(), v.to_owned()));
    }
    Some(out)
}

fn wav_items(f: &mut std::fs::File) -> Result<Items, String> {
    let h = rd(f, 12)?;
    if &h[..4] != b"RIFF" || &h[8..] != b"WAVE" {
        return Err("不是 RIFF/WAVE".into());
    }
    let (mut lists, mut id3s) = (0, 0);
    loop {
        let mut h = [0u8; 8];
        match f.read(&mut h[..1]) {
            Ok(0) => break,
            Ok(_) => f.read_exact(&mut h[1..]).map_err(|e| e.to_string())?,
            Err(e) => return Err(e.to_string()),
        }
        let len = u32le(&h, 4).unwrap_or(0) as u64;
        match &h[..4] {
            b"fmt " | b"data" | b"JUNK" | b"junk" | b"PAD " | b"FLLR" => skip(f, len)?,
            b"id3 " | b"ID3 " => {
                id3s += 1;
                id3_frame_ids(&rd(f, len as usize)?)?;
            }
            b"LIST" => {
                lists += 1;
                let b = rd(f, len as usize)?;
                if b.get(..4) != Some(b"INFO") {
                    return Err("非 INFO 的 LIST 块".into());
                }
                // 子项须恰好铺满 LIST：奇数长度后的补齐字节须是 0，只有最后一项可省
                let mut ids = HashSet::new();
                let mut i = 4;
                while i < b.len() {
                    let n = u32le(&b, i + 4).ok_or("INFO 结构损坏")?;
                    let id = fourcc(&b[i..i + 4]);
                    i += 8 + n;
                    if i > b.len() || (n % 2 == 1 && i < b.len() && b[i] != 0) {
                        return Err(format!("INFO {id} 项的长度越界"));
                    }
                    i += (n % 2).min(b.len() - i);
                    if !ids.insert(id.clone()) {
                        return Err(format!("INFO {id} 项出现多次"));
                    }
                }
            }
            id => return Err(format!("{} 数据块", fourcc(id))),
        }
        skip(f, len % 2)?;
    }
    if lists > 1 || id3s > 1 {
        return Err("重复的标签块".into());
    }
    f.rewind().map_err(|e| e.to_string())?;
    let wav = lofty::iff::wav::WavFile::read_from(f, ParseOptions::new())
        .map_err(|_| "标签损坏".to_string())?;
    let mut items = vec![];
    for (k, v) in wav.riff_info().into_iter().flatten() {
        let name = ItemKey::from_key(TagType::RiffInfo, k)
            .and_then(|i| i.map_key(TagType::VorbisComments))
            .ok_or_else(|| format!("INFO {k} 项"))?;
        items.push((name.to_owned(), v.clone()));
    }
    let mut pics = 0;
    for fr in wav.id3v2().into_iter().flatten() {
        use lofty::id3::v2::Frame;
        let id = fr.id_str();
        match fr {
            Frame::Text(_) | Frame::Timestamp(_) if id != "TXXX" => {
                let name = match id {
                    // lofty 把序号与总数都映射到同一帧上
                    "TRCK" => "TRACKNUMBER",
                    "TPOS" => "DISCNUMBER",
                    _ => ItemKey::from_key(TagType::Id3v2, id)
                        .and_then(|i| i.map_key(TagType::VorbisComments))
                        .ok_or_else(|| format!("ID3 {id} 帧"))?,
                };
                let v = match fr {
                    Frame::Text(t) => t.value.to_string(),
                    Frame::Timestamp(t) => t.timestamp.to_string(),
                    _ => unreachable!(),
                };
                let g = v.trim_start_matches('(').trim_end_matches(')');
                if id == "TCON" && !g.is_empty() && g.bytes().all(|b| b.is_ascii_digit()) {
                    return Err("ID3 数字流派".into());
                }
                match v.split_once('/') {
                    Some((n, total)) if id == "TRCK" || id == "TPOS" => {
                        let tk = if id == "TRCK" {
                            "TRACKTOTAL"
                        } else {
                            "DISCTOTAL"
                        };
                        items.push((name.to_owned(), n.to_owned()));
                        items.push((tk.to_owned(), total.to_owned()));
                    }
                    _ => items.push((name.to_owned(), v)),
                }
            }
            Frame::Picture(p)
                if p.picture.pic_type() == PictureType::CoverFront
                    && p.picture.description().is_none_or(str::is_empty) =>
            {
                pics += 1
            }
            _ => return Err(format!("ID3 {id} 帧")),
        }
    }
    if pics > 1 {
        return Err("多张图片".into());
    }
    Ok(items)
}

/// ID3 标签里的原始帧 ID（lofty 读时会把重复帧折叠，所以在它之前自己数）：同一 ID 至多一次。
fn id3_frame_ids(b: &[u8]) -> Result<(), String> {
    let bad = || "ID3 损坏".to_string();
    let syncsafe = |x: &[u8]| x.iter().fold(0usize, |n, &c| n << 7 | (c & 0x7f) as usize);
    if b.len() < 10 || &b[..3] != b"ID3" {
        return Err(bad());
    }
    let ver = b[3];
    if !matches!(ver, 3 | 4) || b[5] != 0 {
        return Err(format!("ID3v2.{ver} 或带标志位的 ID3"));
    }
    let end = 10 + syncsafe(&b[6..10]);
    // 标签之后到块尾只能是 0
    if end > b.len() || b[end..].iter().any(|&c| c != 0) {
        return Err("ID3 标签外有多余字节".into());
    }
    let (mut ids, mut i) = (HashSet::new(), 10);
    while i + 10 <= end && b[i] != 0 {
        let id = fourcc(&b[i..i + 4]);
        let size = match ver {
            4 => syncsafe(&b[i + 4..i + 8]),
            _ => u32be(b, i + 4).ok_or_else(bad)?,
        };
        if b[i + 8..i + 10] != [0, 0] {
            return Err(format!("ID3 {id} 帧带标志位"));
        }
        if !ids.insert(id.clone()) {
            return Err(format!("ID3 {id} 帧出现多次"));
        }
        i += 10 + size;
        if i > end {
            return Err(format!("ID3 {id} 帧的长度越界"));
        }
    }
    // 帧区的 padding 之后不能再有内容
    if b[i..end].iter().any(|&c| c != 0) {
        return Err("ID3 padding 之后有多余字节".into());
    }
    Ok(())
}

/// 一层 MP4 盒子：(类型, 内容)。
type Boxes<'a> = [([u8; 4], &'a [u8])];

fn boxes(b: &[u8]) -> Result<Box<Boxes<'_>>, String> {
    let bad = || "MP4 结构损坏".to_string();
    let (mut out, mut i) = (vec![], 0);
    while i < b.len() {
        let mut size = u32be(b, i).ok_or_else(bad)?;
        let ty: [u8; 4] = b.get(i + 4..i + 8).ok_or_else(bad)?.try_into().unwrap();
        let mut hl = 8;
        if size == 1 {
            let big = b.get(i + 8..i + 16).ok_or_else(bad)?;
            size = u64::from_be_bytes(big.try_into().unwrap()) as usize;
            hl = 16;
        }
        if size < hl || size > b.len() - i {
            return Err(bad());
        }
        out.push((ty, &b[i + hl..i + size]));
        i += size;
    }
    Ok(out.into())
}

fn only(bs: &Boxes, allowed: &[&[u8; 4]], at: &str) -> Result<(), String> {
    match bs.iter().find(|(t, _)| !allowed.contains(&t)) {
        Some((t, _)) => Err(format!("{at}里的 {} 盒子", fourcc(t))),
        None => Ok(()),
    }
}

fn count(bs: &Boxes, t: &[u8; 4]) -> usize {
    bs.iter().filter(|(x, _)| x == t).count()
}

/// meta 盒子：hdlr 须是 mdir（iTunes 式标签），下面只有 hdlr、ilst 与填充。
fn mp4_meta(b: &[u8]) -> Result<(), String> {
    let kids = boxes(b.get(4..).ok_or("MP4 结构损坏")?)?;
    for (t, c) in &kids {
        if t == b"hdlr" && c.get(8..12) != Some(b"mdir") {
            return Err(format!(
                "{} 元数据",
                fourcc(c.get(8..12).unwrap_or(b"????"))
            ));
        }
    }
    only(&kids, &[b"hdlr", b"ilst", b"free", b"skip"], "meta ")?;
    if count(&kids, b"ilst") != 1 {
        return Err("meta 里不是恰好一个 ilst".into());
    }
    kids.iter()
        .filter(|(t, _)| t == b"ilst")
        .try_for_each(|(_, c)| ilst_raw(c))
}

/// ilst 里的原始原子（lofty 读时会跳过空值等）：没有自由格式原子，每个只有一个非空的 data。
fn ilst_raw(b: &[u8]) -> Result<(), String> {
    for (t, c) in boxes(b)?.iter() {
        let name = fourcc(t);
        let kids = boxes(c)?;
        if t == b"----" {
            let part = |k: &[u8; 4]| {
                kids.iter()
                    .find(|(t, _)| t == k)
                    .map_or("?".into(), |(_, c)| {
                        String::from_utf8_lossy(c.get(4..).unwrap_or(b"")).into_owned()
                    })
            };
            return Err(format!(
                "----:{}:{} 自由格式原子",
                part(b"mean"),
                part(b"name")
            ));
        }
        only(&kids, &[b"data"], &format!("{name} "))?;
        // data 头：版本 0、类型码、locale；类型须是该键的规范类型
        let want: &[usize] = match t {
            b"covr" => &[13, 14],
            b"trkn" | b"disk" => &[0],
            b"gnre" => &[21],
            _ => &[1],
        };
        if let Some((_, d)) = kids
            .iter()
            .find(|(_, d)| !u32be(d, 0).is_some_and(|c| want.contains(&c)))
        {
            return Err(format!("{name} 的数据类型 {}", u32be(d, 0).unwrap_or(0)));
        }
        match &kids[..] {
            [(_, d)] if d.len() > 8 => {}
            [_] => return Err(format!("{name} 的值为空")),
            [] => return Err(format!("{name} 没有值")),
            _ if t == b"covr" => return Err("多张封面".into()),
            _ => return Err(format!("{name} 有多个值")),
        }
    }
    Ok(())
}

fn mp4_items(f: &mut std::fs::File) -> Result<Items, String> {
    let mut moov = None;
    let end = f.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    let mut at = 0;
    while at < end {
        f.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
        let h = rd(f, 8)?;
        let mut size = u32be(&h, 0).unwrap_or(0) as u64;
        let mut hl = 8;
        if size == 1 {
            size = u64::from_be_bytes(rd(f, 8)?.try_into().unwrap());
            hl = 16;
        } else if size == 0 {
            size = end - at;
        }
        if size < hl || size > end - at {
            return Err("MP4 结构损坏".into());
        }
        match &h[4..8] {
            b"moov" if moov.is_none() => moov = Some(rd(f, (size - hl) as usize)?),
            b"ftyp" | b"mdat" | b"free" | b"skip" | b"wide" => {}
            t => return Err(format!("顶层的 {} 盒子", fourcc(t))),
        }
        at += size;
    }
    let moov = moov.ok_or("MP4 结构损坏")?;
    let kids = boxes(&moov)?;
    if let Some((_, m)) = kids.iter().find(|(t, _)| t == b"meta") {
        mp4_meta(m)?;
        return Err("moov 下的元数据".into());
    }
    only(
        &kids,
        &[b"mvhd", b"iods", b"trak", b"udta", b"free", b"skip"],
        "moov ",
    )?;
    if count(&kids, b"trak") != 1 || count(&kids, b"udta") > 1 {
        return Err("多条音轨或多个 udta".into());
    }
    for (t, c) in &kids {
        if t == b"trak" {
            only(&boxes(c)?, &[b"tkhd", b"edts", b"mdia"], "音轨")?;
        }
        if t == b"udta" {
            let u = boxes(c)?;
            only(&u, &[b"meta", b"free", b"skip"], "udta ")?;
            if count(&u, b"meta") > 1 {
                return Err("多个 meta".into());
            }
            u.iter()
                .filter(|(t, _)| t == b"meta")
                .try_for_each(|(_, m)| mp4_meta(m))?;
        }
    }
    f.rewind().map_err(|e| e.to_string())?;
    let mp4 = Mp4File::read_from(f, ParseOptions::new()).map_err(|_| "标签损坏".to_string())?;
    let mut items = vec![];
    for a in mp4.ilst().into_iter().flatten() {
        let data: Vec<&AtomData> = a.data().collect();
        let (name, shown) = match a.ident() {
            AtomIdent::Fourcc(c) => {
                let k = fourcc(c);
                let n = match &*k {
                    "covr" if data.len() > 1 => return Err("多张封面".into()),
                    "covr" => continue,
                    // lofty 把序号与总数都映射到同一个原子上
                    "trkn" => "TRACKNUMBER",
                    "disk" => "DISCNUMBER",
                    _ => ItemKey::from_key(TagType::Mp4Ilst, &k)
                        .and_then(|i| i.map_key(TagType::VorbisComments))
                        .ok_or_else(|| format!("{k} 原子"))?,
                };
                (n.to_owned(), k)
            }
            AtomIdent::Freeform { mean, name } => return Err(format!("----:{mean}:{name} 原子")),
        };
        let [d] = data[..] else {
            return Err(format!("{shown} 有多个值"));
        };
        let v = match d {
            AtomData::UTF8(s) | AtomData::UTF16(s) => s.clone(),
            AtomData::SignedInteger(n) => n.to_string(),
            AtomData::UnsignedInteger(n) => n.to_string(),
            AtomData::Bool(b) => u8::from(*b).to_string(),
            AtomData::Unknown { data, .. } if &shown == "trkn" || &shown == "disk" => {
                let (n, t) = (u16be(data, 2), u16be(data, 4));
                let tk = if &shown == "trkn" {
                    "TRACKTOTAL"
                } else {
                    "DISCTOTAL"
                };
                if t.is_some_and(|t| t > 0) {
                    items.push((tk.to_owned(), t.unwrap().to_string()));
                }
                n.map_or(String::new(), |n| n.to_string())
            }
            _ => return Err(format!("{shown} 原子")),
        };
        items.push((name, v));
    }
    Ok(items)
}

fn u16be(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn check_items(items: &[(String, String)]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for (k, v) in items {
        let up = k.to_uppercase();
        let ok = KNOWN.contains(&up.as_str())
            && ItemKey::from_key(TagType::VorbisComments, k)
                .and_then(|i| i.map_key(TagType::VorbisComments))
                .is_some_and(|c| c.eq_ignore_ascii_case(k));
        if !ok {
            return Err(format!("{up} 标签"));
        }
        if v.is_empty() {
            return Err(format!("{up} 的值为空"));
        }
        if v.contains('\0') {
            return Err(format!("{up} 有多个值"));
        }
        let key = norm_key(k);
        if NUMERIC.contains(&key.as_str())
            && !(v.parse::<u16>().is_ok_and(|n| n > 0)
                && !v.starts_with('0')
                && !v.starts_with('+'))
        {
            return Err(format!("{up}={v}"));
        }
        if !seen.insert(key) {
            return Err(format!("{up} 出现多次"));
        }
    }
    for (total, n) in [("tracktotal", "track"), ("disctotal", "disc")] {
        if seen.contains(total) && !seen.contains(n) {
            return Err(format!("只有 {total} 没有序号"));
        }
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// 测试钩子：比对前改动隐藏副本，模拟转换不保真。
    pub(crate) static TAMPER: std::cell::Cell<Option<fn(&Path)>> = const { std::cell::Cell::new(None) };
    /// 测试钩子：新文件换上后、删原文件前中断，模拟此刻退出。
    pub(crate) static HALT_AFTER_PLACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

const CHANGED: &str = "转换期间原文件被改动，未替换";

/// 原文件还是开始转换时那个：同一 inode，权限、大小、修改时间都没变。
fn untouched(m: &std::fs::Metadata, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).is_ok_and(|n| {
        (
            n.dev(),
            n.ino(),
            n.mode(),
            n.len(),
            n.mtime(),
            n.mtime_nsec(),
        ) == (
            m.dev(),
            m.ino(),
            m.mode(),
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
        )
    })
}

fn other(to: Lossless) -> Lossless {
    match to {
        Lossless::Alac => Lossless::Flac,
        Lossless::Flac => Lossless::Alac,
    }
}

fn fail(s: impl Into<String>) -> ConvertError {
    ConvertError::Failed(s.into())
}

fn io(e: std::io::Error) -> ConvertError {
    fail(format!("读写失败：{e}"))
}

/// 把 `path`（FLAC / WAV 或 ALAC .m4a）无损转成 `to`，以「同名 + 新扩展名」落在原处并删掉原文件，
/// 返回新路径。隐藏副本上转好后比对：解码后的 PCM、位深与采样率，以及经 ffmpeg 独立读到的全部标签、
/// 附图与歌词（lofty 也再比一遍）全部一致才替换；删原文件前原文件被改过（权限、大小、修改时间）则放弃；保留创建时间、权限与扩展属性
/// （Finder 标签）。元数据结构不在白名单里（见 `structure`）的宁可不转。任一步不成返回一句中文原因，
/// 原文件不变、不留副本。符号链接、只读文件不转；新文件名已被占用不覆盖——除非那个文件也在白名单里、
/// 没有别的硬链接名、且与原文件完全一致（上次换上后没来得及删原文件），那就补上属性、删原文件，算转好。
pub fn recode_lossless(
    path: &Path,
    to: Lossless,
    cancel: &CancelToken,
) -> Result<PathBuf, ConvertError> {
    let m = std::fs::symlink_metadata(path).map_err(io)?;
    if !m.is_file() {
        return Err(fail("是符号链接或不是普通文件，未转换"));
    }
    if let Err(e) = std::fs::OpenOptions::new().write(true).open(path) {
        return Err(match e.kind() {
            std::io::ErrorKind::PermissionDenied => fail("文件是只读的，未转换"),
            _ => io(e),
        });
    }
    let ff = crate::find_ffmpeg().ok_or_else(|| fail("找不到 ffmpeg，未转换"))?;
    let _g = rewrite_lock();
    let src = read_audio(path).map_err(|_| fail("读不了这首歌的音频信息，未转换"))?;
    accepts(&src, path, to).map_err(fail)?;
    let (ext, args) = match to {
        Lossless::Alac => ("m4a", ["-c:a", "alac", "-f", "ipod"]),
        Lossless::Flac => ("flac", ["-c:a", "flac", "-f", "flac"]),
    };
    let want = norm(&ff_meta(&ff, path, cancel)?);
    let dst = path.with_extension(ext);
    if dst.symlink_metadata().is_ok() {
        // 上次换上新文件后没来得及删原文件（或本来就有一模一样的）：完全一致才删原文件
        // 目标另有硬链接名时补属性会改到那个名字上：不碰
        use std::os::unix::fs::MetadataExt;
        let same = dst
            .symlink_metadata()
            .is_ok_and(|d| d.is_file() && d.nlink() == 1)
            && read_audio(&dst).is_ok_and(|d| accepts(&d, &dst, other(to)).is_ok())
            && verify(&ff, path, &src, &want, &dst, cancel).is_ok();
        if !same {
            return Err(fail(format!("已有同名的 .{ext} 文件，未覆盖")));
        }
        if !untouched(&m, path) {
            return Err(fail(CHANGED));
        }
        keep_attrs(&m, path, &dst).map_err(io)?;
        std::fs::remove_file(path).map_err(io)?;
        return Ok(dst);
    }
    let mut parts = Parts::default();
    let tmp = parts.add(path.parent().unwrap_or(Path::new(".")));
    let mut cmd = Command::new(&ff);
    cmd.args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-map_metadata", "-1"])
        .args(["-fflags", "+bitexact", "-flags:a", "+bitexact"])
        .args(args)
        .arg(&tmp);
    run(cmd, cancel).map_err(|e| match e {
        ConvertError::Cancelled => e,
        _ => fail("ffmpeg 转换失败，未替换"),
    })?;
    copy_tags(&src, &tmp, to).map_err(|e| fail(format!("写入标签失败：{e}")))?;
    #[cfg(test)]
    if let Some(f) = TAMPER.get() {
        f(&tmp);
    }
    verify(&ff, path, &src, &want, &tmp, cancel)?;
    keep_attrs(&m, path, &tmp).map_err(io)?;
    match place(&tmp, &dst) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(fail(format!("已有同名的 .{ext} 文件，未覆盖")))
        }
        Err(e) => return Err(io(e)),
    }
    #[cfg(test)]
    if HALT_AFTER_PLACE.get() {
        return Err(fail("测试：换上新文件后中断"));
    }
    if !untouched(&m, path) {
        let _ = std::fs::remove_file(&dst);
        return Err(fail(CHANGED));
    }
    if let Err(e) = std::fs::remove_file(path) {
        let _ = std::fs::remove_file(&dst);
        return Err(io(e));
    }
    Ok(dst)
}

/// 跑 ffmpeg 并收 stdout；取消时杀掉它。
fn run(mut cmd: Command, cancel: &CancelToken) -> Result<Vec<u8>, ConvertError> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| fail(format!("无法启动 ffmpeg：{e}")))?;
    // 边跑边收：输出超过管道缓冲时子进程不会卡在写上
    let mut stdout = child.stdout.take().expect("stdout 是管道");
    let reader = std::thread::spawn(move || {
        let mut out = vec![];
        stdout.read_to_end(&mut out).map(|_| out)
    });
    let st = loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(ConvertError::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io(e));
            }
        }
    };
    let out = reader.join().expect("读取线程不会 panic").map_err(io)?;
    if st.success() {
        Ok(out)
    } else {
        Err(fail(format!("ffmpeg 失败（{st}）")))
    }
}

fn is_lyrics(k: ItemKey) -> bool {
    matches!(k, ItemKey::Lyrics | ItemKey::UnsyncLyrics)
}

/// 源的全部标签项（多个标签时同一项取第一个有它的标签，主标签在前；歌词另算）、全部图片与歌词，
/// 按新格式映射后写进 `tmp`。
fn copy_tags(src: &TaggedFile, tmp: &Path, to: Lossless) -> Result<(), String> {
    let ty = read_audio(tmp)
        .map_err(|e| e.to_string())?
        .primary_tag_type();
    let mut t = Tag::new(ty);
    let tags = tags_of(src);
    let mut seen = HashSet::new();
    for s in &tags {
        for it in s.items().filter(|i| !seen.contains(&i.key())) {
            if !is_lyrics(it.key()) {
                t.push(it.clone());
            }
        }
        seen.extend(s.items().map(|i| i.key()));
    }
    for (i, p) in pictures(&tags).iter().enumerate() {
        let mut p = p.clone();
        // MP4 的图片不分类型：转回 FLAC 时第一张算封面正面
        if to == Lossless::Flac && i == 0 && p.pic_type() == PictureType::Other {
            p.set_pic_type(PictureType::CoverFront);
        }
        t.push_picture(p);
    }
    if let Some(l) = lyrics_of(&tags) {
        let k = lyrics_key(&t);
        t.insert_text(k, l);
    }
    t.save_to_path(tmp, WriteOptions::default())
        .map_err(|e| e.to_string())
}

fn pictures<'a>(tags: &[&'a Tag]) -> &'a [lofty::picture::Picture] {
    tags.iter()
        .map(|t| t.pictures())
        .find(|p| !p.is_empty())
        .unwrap_or(&[])
}

/// 标签项按键归并（取法同 `copy_tags`），只留映射得到 `ty` 的键；曲序 / 碟号按数值比。
fn view(f: &TaggedFile, ty: TagType) -> HashMap<ItemKey, Vec<String>> {
    let mut out: HashMap<ItemKey, Vec<String>> = HashMap::new();
    for s in tags_of(f) {
        let mut here: HashMap<ItemKey, Vec<String>> = HashMap::new();
        for it in s.items() {
            let k = it.key();
            if out.contains_key(&k) || is_lyrics(k) || k.map_key(ty).is_none() {
                continue;
            }
            let v = match it.value() {
                ItemValue::Text(s) | ItemValue::Locator(s) => s.clone(),
                ItemValue::Binary(b) => format!("{b:02x?}"),
            };
            let numeric = matches!(
                k,
                ItemKey::TrackNumber
                    | ItemKey::TrackTotal
                    | ItemKey::DiscNumber
                    | ItemKey::DiscTotal
            );
            let v = match v.split('/').next().map(str::trim) {
                Some(n) if numeric => n.parse::<u32>().map_or(v.clone(), |n| n.to_string()),
                _ => v,
            };
            here.entry(k).or_default().push(v);
        }
        for (k, mut v) in here {
            v.sort();
            out.insert(k, v);
        }
    }
    out
}

/// ffmpeg 读到的全局元数据（`-f ffmetadata`），键与值原样。和写入用的 lofty 互不相干，
/// 比对时用它独立看两边。
fn ff_meta(
    ff: &Path,
    p: &Path,
    cancel: &CancelToken,
) -> Result<Vec<(String, String)>, ConvertError> {
    let mut cmd = Command::new(ff);
    cmd.args(["-v", "error", "-nostdin", "-i"])
        .arg(p)
        .args(["-f", "ffmetadata", "-"]);
    let out = run(cmd, cancel).map_err(|e| match e {
        ConvertError::Cancelled => e,
        _ => fail("无法读取标签，未转换"),
    })?;
    Ok(parse_ffmeta(&String::from_utf8_lossy(&out)))
}

/// 解析 ffmetadata 文本的全局段：`\` 转义 `=`、`;`、`#`、`\` 与换行；`;` / `#` 开头是注释，
/// 遇到第一个 `[段]` 为止。
fn parse_ffmeta(s: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    let (mut k, mut v) = (String::new(), String::new());
    let (mut in_v, mut esc, mut start, mut skip) = (false, false, true, false);
    for ch in s.chars() {
        if start {
            start = false;
            match ch {
                ';' | '#' => skip = true,
                '[' => break,
                _ => {}
            }
        }
        if esc {
            if in_v { &mut v } else { &mut k }.push(ch);
            esc = false;
            continue;
        }
        match ch {
            '\\' => esc = true,
            '\n' => {
                if in_v && !skip {
                    out.push((std::mem::take(&mut k), std::mem::take(&mut v)));
                }
                (k, v) = (String::new(), String::new());
                (in_v, start, skip) = (false, true, false);
            }
            '=' if !in_v => in_v = true,
            c => if in_v { &mut v } else { &mut k }.push(c),
        }
    }
    if in_v && !skip {
        out.push((k, v));
    }
    out
}

/// 按键归一后的元数据：键小写、别名合一，「3/10」拆成序号与总数（MP4、ID3 的写法），值逐字不动；
/// 同键多值排序。ffmpeg 从 MP4 容器生成的几项（major_brand 等）不算；源文件里真有同名标签的，
/// 白名单先已拒转。
type Meta = BTreeMap<String, Vec<String>>;

const NUMERIC: [&str; 4] = ["track", "tracktotal", "disc", "disctotal"];

/// 歌词的各种键（`lyrics-<语言>`、`UNSYNCEDLYRICS`）算一个；ffmpeg 把 FLAC 的 `DESCRIPTION` 读成
/// `comment`、MP4 的 `desc` 读成 `description`，两者算一个（值按多重集比，不会丢）。
fn norm_key(k: &str) -> String {
    let k = k.to_lowercase();
    if k.starts_with("lyrics") || k == "unsyncedlyrics" {
        return "lyrics".into();
    }
    match k.as_str() {
        "description" => "comment",
        "albumartist" | "album artist" => "album_artist",
        "tracknumber" => "track",
        "discnumber" | "disk" => "disc",
        "totaltracks" => "tracktotal",
        "totaldiscs" => "disctotal",
        _ => return k,
    }
    .into()
}

fn norm(items: &[(String, String)]) -> Meta {
    let mut m = Meta::new();
    for (k, v) in items {
        let k = norm_key(k);
        if ["major_brand", "minor_version", "compatible_brands"].contains(&k.as_str()) {
            continue;
        }
        let mut put = |k: &str, v: &str| m.entry(k.to_owned()).or_default().push(v.to_owned());
        match v.split_once('/') {
            Some((n, total)) if k == "track" || k == "disc" => {
                put(&k, n);
                put(&format!("{k}total"), total);
            }
            _ => put(&k, v),
        }
    }
    m.values_mut().for_each(|v| v.sort());
    m
}

/// 附图（ffmpeg 的附带图片流）逐张的 SHA-256，按顺序。
fn ff_pics(ff: &Path, p: &Path, cancel: &CancelToken) -> Result<Vec<String>, ConvertError> {
    let mut cmd = Command::new(ff);
    cmd.args(["-v", "error", "-nostdin", "-i"])
        .arg(p)
        .args(["-map", "0:v?", "-map", "0:a:0", "-c", "copy"])
        .args(["-f", "streamhash", "-hash", "sha256", "-"]);
    let out = run(cmd, cancel).map_err(|e| match e {
        ConvertError::Cancelled => e,
        _ => fail("无法读取封面比对，未替换"),
    })?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter(|l| l.split(',').nth(1) == Some("v"))
        .filter_map(|l| l.split_once('=').map(|x| x.1.to_owned()))
        .collect())
}

/// 新文件与原文件逐项比对，不一致返回原因。`want` 是原文件经 ffmpeg 独立读到的元数据：
/// 新文件经 ffmpeg 读到的必须一项不差（多了少了都不行）；封面、歌词另由 ffmpeg 与 lofty 各比一遍。
fn verify(
    ff: &Path,
    path: &Path,
    src: &TaggedFile,
    want: &Meta,
    tmp: &Path,
    cancel: &CancelToken,
) -> Result<(), ConvertError> {
    let new = read_audio(tmp).map_err(|_| fail("读不了转换后的文件，未替换"))?;
    let shape = |f: &TaggedFile| {
        let p = f.properties();
        (p.sample_rate(), p.bit_depth(), p.channels())
    };
    if shape(src) != shape(&new) {
        return Err(fail("转换后位深或采样率不一致，未替换"));
    }
    if pcm(ff, path, cancel)? != pcm(ff, tmp, cancel)? {
        return Err(fail("转换后的音频与原文件不一致，未替换"));
    }
    let got = norm(&ff_meta(ff, tmp, cancel)?);
    let differ = |lyr: bool| {
        let keys = want.keys().chain(got.keys());
        keys.filter(|k| (*k == "lyrics") == lyr)
            .any(|k| want.get(k) != got.get(k))
    };
    let (a, b) = (tags_of(src), tags_of(&new));
    if differ(true) || lyrics_of(&a) != lyrics_of(&b) {
        return Err(fail("转换后歌词不一致，未替换"));
    }
    let ty = new.primary_tag_type();
    let (lw, lg) = (view(src, ty), view(&new, ty));
    if differ(false) || lw.iter().any(|(k, v)| lg.get(k) != Some(v)) {
        return Err(fail("转换后标签不一致，未替换"));
    }
    let bytes =
        |t: &[&Tag]| -> Vec<Vec<u8>> { pictures(t).iter().map(|p| p.data().to_vec()).collect() };
    if bytes(&a) != bytes(&b) || ff_pics(ff, path, cancel)? != ff_pics(ff, tmp, cancel)? {
        return Err(fail("转换后封面不一致，未替换"));
    }
    Ok(())
}

/// 第一条音频流解码成 32 位整数 PCM 后的 SHA-256（16 / 24 bit 都能无损放进去）。
fn pcm(ff: &Path, p: &Path, cancel: &CancelToken) -> Result<Vec<u8>, ConvertError> {
    let mut cmd = Command::new(ff);
    cmd.args(["-v", "error", "-nostdin", "-i"])
        .arg(p)
        .args(["-map", "0:a:0", "-c:a", "pcm_s32le"])
        .args(["-f", "streamhash", "-hash", "sha256", "-"]);
    let out = run(cmd, cancel).map_err(|e| match e {
        ConvertError::Cancelled => e,
        _ => fail("无法解码比对音频，未替换"),
    })?;
    if out.is_empty() {
        return Err(fail("无法解码比对音频，未替换"));
    }
    Ok(out)
}

/// 创建时间、扩展属性（含 Finder 标签）、权限照原文件。
fn keep_attrs(m: &std::fs::Metadata, path: &Path, tmp: &Path) -> std::io::Result<()> {
    if let Ok(born) = m.created() {
        use std::os::macos::fs::FileTimesExt;
        std::fs::File::options()
            .write(true)
            .open(tmp)?
            .set_times(std::fs::FileTimes::new().set_created(born))?;
    }
    copy_xattrs(path, tmp)?;
    std::fs::set_permissions(tmp, m.permissions())
}

fn copy_xattrs(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::ffi::{c_char, c_int, c_void, CString};
    use std::os::unix::ffi::OsStrExt;
    extern "C" {
        // <copyfile.h>
        fn copyfile(
            from: *const c_char,
            to: *const c_char,
            state: *mut c_void,
            flags: u32,
        ) -> c_int;
    }
    const COPYFILE_XATTR: u32 = 1 << 2;
    let c = |p: &Path| CString::new(p.as_os_str().as_bytes()).map_err(std::io::Error::other);
    let (f, t) = (c(from)?, c(to)?);
    // SAFETY: 两个以 NUL 结尾的路径在调用期间有效；state 为空表示不复用状态
    let r = unsafe { copyfile(f.as_ptr(), t.as_ptr(), std::ptr::null_mut(), COPYFILE_XATTR) };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
