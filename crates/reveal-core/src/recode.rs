//! 输出文件夹里已有的歌在 FLAC / WAV 与 ALAC 之间无损互转：隐藏副本上转好、逐项比对一致才替换。

use crate::convert::{
    lyrics_key, lyrics_of, place, read_audio, tags_of, CancelToken, ConvertError, Parts,
};
use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, FileType, TaggedFile, TaggedFileExt};
use lofty::flac::FlacFile;
use lofty::mp4::{Atom, AtomData, AtomIdent, Mp4Codec, Mp4File};
use lofty::ogg::tag::VorbisComments;
use lofty::picture::PictureType;
use lofty::prelude::ItemKey;
use lofty::tag::{ItemValue, Tag, TagExt, TagType};
use std::collections::{BTreeMap, HashMap, HashSet};
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

/// `src`（读自 `path`）能不能无损转成 `to`，不能时给出原因。ALAC 只收 16 / 24 bit 整数、1–8 声道。
fn accepts(src: &TaggedFile, path: &Path, to: Lossless) -> Result<(), String> {
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
            if !matches!(p.channels(), Some(1..=8)) {
                return Err("声道数无法无损转成 ALAC，未转换".into());
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

#[cfg(test)]
thread_local! {
    /// 测试钩子：比对前改动隐藏副本，模拟转换不保真。
    pub(crate) static TAMPER: std::cell::Cell<Option<fn(&Path)>> = const { std::cell::Cell::new(None) };
    /// 测试钩子：新文件换上后、删原文件前中断，模拟此刻退出。
    pub(crate) static HALT_AFTER_PLACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
/// 返回新路径。隐藏副本上转好后比对：解码后的 PCM、位深与采样率，以及经 ffmpeg 独立读到的全部标签
/// （含自定义键）、附图与歌词（lofty 也再比一遍）全部一致才替换；保留创建时间、权限与扩展属性
/// （Finder 标签）。表达不了的标签宁可不转。任一步不成返回一句中文原因，原文件不变、不留副本。
/// 符号链接、只读文件不转；新文件名已被占用不覆盖——除非那个文件与原文件完全一致（上次换上后
/// 没来得及删原文件），那就补上属性、删原文件，算转好。
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
    let raw = ff_meta(&ff, path, cancel)?;
    let want = norm(&raw);
    if to == Lossless::Flac {
        let bad = unconvertible(path, &want);
        if !bad.is_empty() {
            return Err(fail(format!(
                "有无法转换的标签（{}），未转换",
                bad.join("、")
            )));
        }
    }
    let dst = path.with_extension(ext);
    if dst.symlink_metadata().is_ok() {
        // 上次换上新文件后没来得及删原文件（或本来就有一模一样的）：完全一致才删原文件
        let same = dst.symlink_metadata().is_ok_and(|d| d.is_file())
            && read_audio(&dst).is_ok_and(|d| accepts(&d, &dst, other(to)).is_ok())
            && verify(&ff, path, &src, &want, &dst, cancel).is_ok();
        if !same {
            return Err(fail(format!("已有同名的 .{ext} 文件，未覆盖")));
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
    top_up(&ff, &raw, &want, &tmp, to, cancel)?;
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
    if let Err(e) = std::fs::remove_file(path) {
        let _ = std::fs::remove_file(&dst);
        return Err(io(e));
    }
    Ok(dst)
}

/// 跑 ffmpeg 并收 stdout；取消时杀掉它。
fn run(mut cmd: Command, cancel: &CancelToken) -> Result<Vec<u8>, ConvertError> {
    use std::io::Read;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| fail(format!("无法启动 ffmpeg：{e}")))?;
    loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ConvertError::Cancelled);
        }
        match child.try_wait().map_err(io)? {
            Some(st) => {
                let mut out = vec![];
                if let Some(mut o) = child.stdout.take() {
                    o.read_to_end(&mut out).map_err(io)?;
                }
                return if st.success() {
                    Ok(out)
                } else {
                    Err(fail(format!("ffmpeg 失败（{st}）")))
                };
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
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
                ItemValue::Text(s) | ItemValue::Locator(s) => s.trim().to_owned(),
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

/// 按键归一后的元数据：键小写、别名合一，「3/10」拆成序号与总数，序号去前导零，值去首尾空白；
/// 同键多值排序。容器自带的几项不算。
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
        let mut put = |k: &str, v: &str| {
            let v = v.trim();
            let v = match NUMERIC.contains(&k) {
                true => v.parse::<u64>().map_or(v.to_owned(), |n| n.to_string()),
                false => v.to_owned(),
            };
            m.entry(k.to_owned()).or_default().push(v);
        };
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

/// MP4 里 FLAC 表达不了、也没被 ffmpeg 读出来的标签原子（转过去会丢）。
fn unconvertible(path: &Path, seen: &Meta) -> Vec<String> {
    let Ok(mut f) = std::fs::File::open(path) else {
        return vec![];
    };
    let Ok(mp4) = Mp4File::read_from(&mut f, ParseOptions::new()) else {
        return vec![];
    };
    let Some(ilst) = mp4.ilst() else {
        return vec![];
    };
    ilst.into_iter()
        .filter_map(|a| match a.ident() {
            AtomIdent::Fourcc([b'c', b'o', b'v', b'r']) => None,
            AtomIdent::Fourcc(f) => {
                let key: String = f.iter().map(|&b| b as char).collect();
                let ok = ItemKey::from_key(TagType::Mp4Ilst, &key)
                    .is_some_and(|k| k.map_key(TagType::VorbisComments).is_some());
                (!ok).then_some(key)
            }
            AtomIdent::Freeform { mean, name } => {
                (!seen.contains_key(&norm_key(name))).then(|| format!("----:{mean}:{name}"))
            }
        })
        .collect()
}

/// ffmpeg 在 MP4 里从固定原子读出的键：这些只能由 lofty 写进对应原子，不拿自由格式原子凑。
const MP4_FIXED: &[&str] = &[
    "title",
    "artist",
    "album_artist",
    "album",
    "date",
    "comment",
    "genre",
    "composer",
    "copyright",
    "grouping",
    "lyrics",
    "encoder",
    "description",
    "synopsis",
    "show",
    "episode_id",
    "network",
    "track",
    "tracktotal",
    "disc",
    "disctotal",
    "compilation",
    "gapless_playback",
    "media_type",
    "rating",
    "hd_video",
    "podcast",
    "category",
    "keywords",
    "sort_name",
    "sort_artist",
    "sort_album",
    "sort_album_artist",
    "sort_composer",
];

fn vorbis_key(k: &str) -> String {
    match k {
        "track" => "TRACKNUMBER".into(),
        "disc" => "DISCNUMBER".into(),
        "album_artist" => "ALBUMARTIST".into(),
        _ => k.to_uppercase(),
    }
}

/// lofty 按通用标签写过之后，ffmpeg 读到源里有、新文件里没有或不同的项照原值补上：
/// FLAC 写成 Vorbis 注释，MP4 写成 `----:com.apple.iTunes:<原键名>` 自由格式原子。
fn top_up(
    ff: &Path,
    raw: &[(String, String)],
    want: &Meta,
    tmp: &Path,
    to: Lossless,
    cancel: &CancelToken,
) -> Result<(), ConvertError> {
    let have = norm(&ff_meta(ff, tmp, cancel)?);
    let missing: Vec<&String> = want
        .keys()
        .filter(|k| have.get(*k) != want.get(*k))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    fn werr(e: impl std::fmt::Display) -> ConvertError {
        fail(format!("写入标签失败：{e}"))
    }
    let mut file = std::fs::File::open(tmp).map_err(io)?;
    match to {
        Lossless::Flac => {
            let mut f = FlacFile::read_from(&mut file, ParseOptions::new()).map_err(werr)?;
            if f.vorbis_comments().is_none() {
                f.set_vorbis_comments(VorbisComments::default());
            }
            let vc = f.vorbis_comments_mut().expect("刚补上");
            for k in missing {
                let old: Vec<String> = vc
                    .items()
                    .filter(|(x, _): &(&str, &str)| norm_key(x) == **k)
                    .map(|(x, _)| x.to_owned())
                    .collect();
                for x in old {
                    vc.remove(&x).for_each(drop);
                }
                for v in &want[k] {
                    vc.push(vorbis_key(k), v.clone());
                }
            }
            f.save_to_path(tmp, WriteOptions::default()).map_err(werr)
        }
        Lossless::Alac => {
            let mut f = Mp4File::read_from(&mut file, ParseOptions::new()).map_err(werr)?;
            if f.ilst().is_none() {
                f.set_ilst(Default::default());
            }
            let ilst = f.ilst_mut().expect("刚补上");
            for k in missing
                .into_iter()
                .filter(|k| !MP4_FIXED.contains(&k.as_str()))
            {
                let name = raw
                    .iter()
                    .find(|(x, _)| norm_key(x) == *k)
                    .map_or(k.clone(), |(x, _)| x.clone());
                let ident = AtomIdent::Freeform {
                    mean: "com.apple.iTunes".into(),
                    name: name.into(),
                };
                ilst.remove(&ident).for_each(drop);
                let data = want[k].iter().map(|v| AtomData::UTF8(v.clone())).collect();
                ilst.insert(Atom::from_collection(ident, data).expect("至少一个值"));
            }
            f.save_to_path(tmp, WriteOptions::default()).map_err(werr)
        }
    }
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
    // lofty 也比一遍；它读不了原文件的歌词（如 mdta 键）时，新文件里的须是 ffmpeg 在原文件里读到的
    let lofty_ok = match (lyrics_of(&a), lyrics_of(&b)) {
        (Some(x), y) => y.as_ref() == Some(&x),
        (None, None) => true,
        (None, Some(y)) => want
            .get("lyrics")
            .is_some_and(|v| v.iter().any(|w| *w == y.trim())),
    };
    if differ(true) || !lofty_ok {
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
