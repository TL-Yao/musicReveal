//! 一首歌的完整转换：解密 → 可选转码 → 标签 → 命名落盘。

use std::path::{Path, PathBuf};

const MAX_NAME: usize = 200;

fn clean(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '.')
}

/// `<歌手> - <歌名>`，非法字符换成 `_`，未截断。
pub(crate) fn file_stem(title: Option<&str>, artists: &[String], fallback: &str) -> String {
    let title = title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(fallback);
    let artists: Vec<&str> = artists
        .iter()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty())
        .collect();
    let raw = if artists.is_empty() {
        title.to_string()
    } else {
        format!("{} - {title}", artists.join("、"))
    };
    let s: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || r#"/\:*?"<>|"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    clean(&s).to_string()
}

fn truncate(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    clean(&s[..end])
}

/// 第 n 个候选名：`stem.ext`、`stem (n).ext`，整个文件名 ≤ 200 字节。
pub(crate) fn numbered(dir: &Path, stem: &str, n: u32, ext: &str) -> PathBuf {
    let suffix = if n == 1 {
        format!(".{ext}")
    } else {
        format!(" ({n}).{ext}")
    };
    dir.join(format!(
        "{}{suffix}",
        truncate(stem, MAX_NAME - suffix.len())
    ))
}

/// 不覆盖地把 `src` 落到 `dst`；目标已存在（含悬空符号链接）返回 AlreadyExists。
fn place(src: &Path, dst: &Path) -> std::io::Result<()> {
    match std::fs::hard_link(src, dst) {
        Ok(()) => {
            let _ = std::fs::remove_file(src);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(e),
        // 不支持硬链接的文件系统（exFAT 等）：先独占创建占位，再 rename 换进去
        Err(_) => {
            std::fs::File::create_new(dst)?;
            std::fs::rename(src, dst).inspect_err(|_| {
                let _ = std::fs::remove_file(dst);
            })
        }
    }
}

/// 依次试候选名，音频与同名 `.lrc`（有歌词时）都不覆盖任何已有文件。
fn land(
    dir: &Path,
    stem: &str,
    ext: &str,
    audio: &Path,
    lrc: Option<&str>,
) -> std::io::Result<(PathBuf, Option<u32>, Option<PathBuf>)> {
    use std::io::ErrorKind::AlreadyExists;
    for n in 1.. {
        let out = numbered(dir, stem, n, ext);
        let lrc_out = out.with_extension("lrc");
        if let Some(text) = lrc {
            match std::fs::File::create_new(&lrc_out) {
                Ok(mut f) => f.write_all(text.as_bytes()).inspect_err(|_| {
                    let _ = std::fs::remove_file(&lrc_out);
                })?,
                Err(e) if e.kind() == AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        match place(audio, &out) {
            Ok(()) => return Ok((out, (n > 1).then_some(n), lrc.map(|_| lrc_out))),
            Err(e) => {
                if lrc.is_some() {
                    let _ = std::fs::remove_file(&lrc_out);
                }
                if e.kind() != AlreadyExists {
                    return Err(e);
                }
            }
        }
    }
    unreachable!("候选编号用尽")
}

use crate::{
    decrypt, fetch_ekey, sniff, Container, Credentials, EkeyError, EmbeddedMeta, Platform, SongId,
    SourceKind,
};
use lofty::file::{AudioFile, TaggedFile, TaggedFileExt};
use lofty::picture::{Picture, PictureType};
use lofty::prelude::{Accessor, ItemKey};
use lofty::tag::{items::Timestamp, Tag};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub struct Lyrics {
    pub lrc: String,
    pub translated_lrc: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrackMeta {
    pub title: Option<String>,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub year: Option<u16>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub cover: Option<Vec<u8>>,
    pub lyrics: Option<Lyrics>,
    pub song_id: Option<SongId>,
    pub platform: Platform,
}

#[async_trait::async_trait]
pub trait Enricher: Send + Sync {
    async fn enrich(&self, meta: &mut TrackMeta) -> Result<(), String>;
}

pub struct NoopEnricher;

#[async_trait::async_trait]
impl Enricher for NoopEnricher {
    async fn enrich(&self, _meta: &mut TrackMeta) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputFormat {
    Original,
    Mp3,
}

#[derive(Debug, Clone)]
pub struct ConvertOptions {
    pub output_dir: PathBuf,
    pub format: OutputFormat,
    pub ffmpeg: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stage {
    Decrypt,
    Transcode,
    Metadata,
}

#[derive(Debug, Default)]
struct CancelInner {
    flag: AtomicBool,
    notify: tokio::sync::Notify,
}

#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<CancelInner>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.flag.store(true, Ordering::SeqCst);
        self.0.notify.notify_waiters();
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.flag.load(Ordering::SeqCst)
    }
    /// 取消时完成；已取消则立即完成。
    pub(crate) async fn cancelled(&self) {
        loop {
            let n = self.0.notify.notified();
            tokio::pin!(n);
            n.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            n.await;
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConvertOutcome {
    pub output: PathBuf,
    pub lrc: Option<PathBuf>,
    pub meta: TrackMeta,
    pub renamed: Option<u32>,
    pub quality: String,
    pub has_cover: bool,
    pub has_lyrics: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConvertError {
    NeedsLogin,
    Cancelled,
    Incomplete,
    Unsupported(String),
    /// 源文件本身坏了（解密 / 容器层面），重试也不会好。
    Corrupt(String),
    /// 其余失败，可能是暂时性或环境性的（网络、磁盘、ffmpeg 等）。
    Failed(String),
}

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConvertError::NeedsLogin => f.write_str("需要登录 QQ 音乐（未登录或登录已过期）"),
            ConvertError::Cancelled => f.write_str("已取消"),
            ConvertError::Incomplete => f.write_str("文件不完整（可能仍在下载或已被截断）"),
            ConvertError::Unsupported(s) => write!(f, "不支持的格式：{s}"),
            ConvertError::Corrupt(s) => write!(f, "文件已损坏：{s}"),
            ConvertError::Failed(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for ConvertError {}

impl From<crate::Error> for ConvertError {
    fn from(e: crate::Error) -> Self {
        use crate::Error as E;
        match e {
            E::Incomplete => ConvertError::Incomplete,
            E::Unsupported(s) => ConvertError::Unsupported(s),
            E::Corrupt(s) => ConvertError::Corrupt(s),
            E::NeedsEkey | E::BadEkey => ConvertError::NeedsLogin,
            e => ConvertError::Failed(e.to_string()),
        }
    }
}

fn io_err(e: std::io::Error) -> ConvertError {
    ConvertError::Failed(format!("读写失败：{e}"))
}

/// 半成品文件名前缀：`<前缀><小写十六进制>.part`，清理时只认这个形状。
pub const PART_PREFIX: &str = ".musicreveal-";

/// 本次转换创建的临时文件；无论成功失败，离开作用域时全部删除（成功时已落盘移走）。
/// 被丢弃时先置 `gone`，让仍在后台跑的阻塞任务知道没人收尾了、自行删除产物。
#[derive(Default)]
pub(crate) struct Parts(Vec<PathBuf>, CancelToken);

impl Parts {
    pub(crate) fn add(&mut self, dir: &Path) -> PathBuf {
        use std::sync::atomic::AtomicU64;
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let p = dir.join(format!(
            "{PART_PREFIX}{:x}{nanos:x}{:x}.part",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        self.0.push(p.clone());
        p
    }
}

impl Drop for Parts {
    fn drop(&mut self) {
        self.1.cancel();
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// 阻塞任务里的产物：外层已放弃（`Parts` 被丢弃）时由任务自己删除。
struct Orphan(PathBuf, CancelToken);

impl Drop for Orphan {
    fn drop(&mut self) {
        if self.1.is_cancelled() {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

/// 每次写之前看一眼取消 / 放弃标志，让流式解密能中途停下。
struct CancelWriter(std::fs::File, CancelToken, CancelToken);

impl Write for CancelWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.1.is_cancelled() || self.2.is_cancelled() {
            return Err(std::io::Error::other("cancelled"));
        }
        self.0.write(b)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ConvertError> + Send + 'static,
) -> Result<T, ConvertError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ConvertError::Failed(format!("内部错误：{e}")))?
}

fn ext(c: &Container) -> &'static str {
    match c {
        Container::Flac => "flac",
        Container::Ogg => "ogg",
        Container::M4a => "m4a",
        Container::Mp3 => "mp3",
        Container::Wav => "wav",
        Container::Wma => "wma",
        Container::Ape => "ape",
    }
}

/// ffmpeg 从 MP4 容器 / 流信息里带出的、不属于歌曲标签的键。
const CONTAINER_KEYS: [&str; 6] = [
    "major_brand",
    "minor_version",
    "compatible_brands",
    "handler_name",
    "vendor_id",
    "language",
];

pub(crate) fn read_audio(p: &Path) -> Result<TaggedFile, ConvertError> {
    lofty::probe::Probe::open(p)
        .and_then(|p| Ok(p.guess_file_type()?))
        .and_then(|p| p.read())
        .map_err(|e| ConvertError::Failed(format!("无法读取解密后的音频：{e}")))
}

pub(crate) fn tag_of(f: &TaggedFile) -> Option<&Tag> {
    f.primary_tag().or_else(|| f.first_tag())
}

fn non_blank(s: Option<String>) -> Option<String> {
    s.filter(|s| !s.trim().is_empty())
}

/// `EmbeddedMeta` 非空字段优先，其余取音频自带标签；空白字符串视同缺失。
fn merge(e: EmbeddedMeta, tag: Option<&Tag>) -> TrackMeta {
    let s = |k: ItemKey| non_blank(tag.and_then(|t| t.get_string(k)).map(str::to_owned));
    let clean = |v: Vec<String>| -> Vec<String> {
        v.into_iter().filter(|a| !a.trim().is_empty()).collect()
    };
    let mut artists = clean(e.artists);
    if artists.is_empty() {
        artists = clean(
            tag.map(|t| {
                t.get_strings(ItemKey::TrackArtist)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        );
    }
    TrackMeta {
        title: non_blank(e.title).or_else(|| s(ItemKey::TrackTitle)),
        artists,
        album: non_blank(e.album).or_else(|| s(ItemKey::AlbumTitle)),
        album_artist: s(ItemKey::AlbumArtist),
        year: tag.and_then(|t| t.date()).map(|d| d.year),
        track: tag.and_then(|t| t.track()),
        disc: tag.and_then(|t| t.disk()),
        cover: e.cover.or_else(|| front_cover(tag?)),
        lyrics: None,
        song_id: e.song_id,
        platform: e.platform,
    }
}

/// 优先封面正面，否则第一张图。
pub(crate) fn front_cover(tag: &Tag) -> Option<Vec<u8>> {
    let pics = tag.pictures();
    pics.iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or(pics.first())
        .map(|p| p.data().to_vec())
}

/// 写入标签并返回音质描述。
fn write_tags(p: &Path, m: &TrackMeta, c: &Container) -> Result<String, ConvertError> {
    let mut f = read_audio(p)?;
    if f.primary_tag().is_none() {
        f.insert_tag(Tag::new(f.primary_tag_type()));
    }
    let t = f.primary_tag_mut().expect("刚插入");
    if let Some(v) = &m.title {
        t.set_title(v.clone());
    }
    if !m.artists.is_empty() {
        t.remove_key(ItemKey::TrackArtists);
        t.insert_text(ItemKey::TrackArtist, m.artists.join(", "));
    }
    if let Some(v) = &m.album {
        t.set_album(v.clone());
    }
    if let Some(v) = &m.album_artist {
        t.insert_text(ItemKey::AlbumArtist, v.clone());
    }
    if let Some(year) = m.year {
        t.set_date(Timestamp {
            year,
            ..Default::default()
        });
    }
    if let Some(v) = m.track {
        t.set_track(v);
    }
    if let Some(v) = m.disc {
        t.set_disk(v);
    }
    if let Some(l) = &m.lyrics {
        // ID3v2 只认 USLT（UnsyncLyrics），其余格式用 Lyrics
        let key = if t.tag_type() == lofty::tag::TagType::Id3v2 {
            ItemKey::UnsyncLyrics
        } else {
            ItemKey::Lyrics
        };
        t.insert_text(key, strip_timestamps(&l.lrc));
    }
    if let Some(cover) = &m.cover {
        if let Ok(mut pic) = Picture::from_reader(&mut &cover[..]) {
            while !t.pictures().is_empty() {
                t.remove_picture(0);
            }
            pic.set_pic_type(PictureType::CoverFront);
            t.push_picture(pic);
        }
    }
    f.save_to_path(p, lofty::config::WriteOptions::default())
        .map_err(|e| ConvertError::Failed(format!("写入标签失败：{e}")))?;
    Ok(quality(&f, c))
}

pub(crate) fn quality(f: &TaggedFile, c: &Container) -> String {
    let pr = f.properties();
    match c {
        Container::Flac | Container::Wav => {
            let rate = pr.sample_rate().unwrap_or(0);
            let khz = if rate.is_multiple_of(1000) {
                (rate / 1000).to_string()
            } else {
                format!("{:.1}", rate as f64 / 1000.0)
            };
            let name = if *c == Container::Flac { "FLAC" } else { "WAV" };
            format!("{name} {}bit/{khz}kHz", pr.bit_depth().unwrap_or(16))
        }
        _ => {
            let k = pr.audio_bitrate().unwrap_or(0);
            let k = [96, 128, 192, 256, 320]
                .into_iter()
                .find(|&t| k.abs_diff(t) * 100 <= t * 3)
                .unwrap_or(k);
            format!("{} {k}k", ext(c).to_ascii_uppercase())
        }
    }
}

/// `mm:ss[.f…]` → 毫秒；小数位按毫秒对齐（`.5` = `.50` = `.500`）。
fn parse_time(s: &str) -> Option<u64> {
    let (m, rest) = s.split_once(':')?;
    let (sec, frac) = rest.split_once('.').unwrap_or((rest, ""));
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    if !digits(m) || !digits(sec) || !(frac.is_empty() || digits(frac)) {
        return None;
    }
    let ms: u64 = format!("{frac:0<3}")[..3].parse().ok()?;
    // 溢出就不是时间标签
    m.parse::<u64>()
        .ok()?
        .checked_mul(60_000)?
        .checked_add(sec.parse::<u64>().ok()?.checked_mul(1000)?)?
        .checked_add(ms)
}

/// 行首的时间标签（毫秒, 原文）与剩余正文。
pub(crate) fn split_line(l: &str) -> (Vec<(u64, &str)>, &str) {
    let mut tags = vec![];
    let mut r = l.trim_start();
    while let Some((inner, rest)) = r.strip_prefix('[').and_then(|x| x.split_once(']')) {
        let Some(ms) = parse_time(inner) else { break };
        tags.push((ms, &r[..inner.len() + 2]));
        r = rest;
    }
    (tags, r)
}

/// `[ar:xx]` 这类元信息行。
fn is_meta(l: &str) -> bool {
    let l = l.trim();
    l.strip_prefix('[')
        .and_then(|x| x.strip_suffix(']'))
        .and_then(|x| x.split_once(':'))
        .is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphabetic()))
}

/// 去掉 LRC 时间轴与元信息行，只留歌词正文。
pub fn strip_timestamps(lrc: &str) -> String {
    lrc.lines()
        .filter_map(|l| {
            let (tags, text) = split_line(l);
            (!tags.is_empty() || !is_meta(l)).then(|| text.trim())
        })
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// 原文每个时间点之后紧跟同一时刻（按毫秒）的译文；一行多时间戳展开成多行。
pub(crate) fn merge_lrc(l: &Lyrics) -> String {
    let mut tr = std::collections::HashMap::new();
    for line in l.translated_lrc.iter().flat_map(|t| t.lines()) {
        let (tags, text) = split_line(line);
        if !text.trim().is_empty() {
            for (ms, _) in tags {
                tr.insert(ms, text.trim());
            }
        }
    }
    let mut out = String::new();
    for line in l.lrc.lines() {
        let (tags, text) = split_line(line);
        if tags.is_empty() {
            out += line;
            out += "\n";
        }
        for (ms, tag) in tags {
            out += &format!("{tag}{text}\n");
            if let Some(t) = tr.get(&ms) {
                out += &format!("{tag}{t}\n");
            }
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct LyricLine {
    pub ms: u64,
    pub text: String,
    pub trans: Option<String>,
}

/// 带时间的歌词行按时间升序；同一时刻紧挨的第二行是第一行的译文（`merge_lrc` 的输出形状）。
pub fn parse_lrc(text: &str) -> Vec<LyricLine> {
    let mut v: Vec<(u64, &str)> = text
        .lines()
        .flat_map(|l| {
            let (tags, body) = split_line(l);
            let body = body.trim();
            tags.into_iter()
                .filter(move |_| !body.is_empty())
                .map(move |(ms, _)| (ms, body))
        })
        .collect();
    v.sort_by_key(|&(ms, _)| ms);
    let mut out: Vec<LyricLine> = vec![];
    for (ms, t) in v {
        match out.last_mut() {
            Some(l) if l.ms == ms && l.trans.is_none() => l.trans = Some(t.into()),
            _ => out.push(LyricLine {
                ms,
                text: t.into(),
                trans: None,
            }),
        }
    }
    out
}

/// 第一条附带图片流的原始字节；没有或抽不出来为 None。
async fn attached_picture(
    ff: &Path,
    input: &Path,
    cancel: &CancelToken,
) -> Result<Option<Vec<u8>>, ConvertError> {
    let child = tokio::process::Command::new(ff)
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(input)
        .args(["-an", "-map", "0:v:0", "-c", "copy", "-frames:v", "1"])
        .args(["-f", "image2pipe", "-"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| ConvertError::Failed(format!("无法启动 ffmpeg：{e}")))?;
    let out = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(ConvertError::Cancelled),
        o = child.wait_with_output() => o.map_err(io_err)?,
    };
    Ok((out.status.success() && !out.stdout.is_empty()).then_some(out.stdout))
}

pub async fn convert(
    input: &Path,
    opts: &ConvertOptions,
    enricher: &dyn Enricher,
    creds: Option<&Credentials>,
    cancel: &CancelToken,
    progress: &mut (dyn FnMut(Stage) + Send),
) -> Result<ConvertOutcome, ConvertError> {
    let kind = sniff(input)?;
    let ekey = match &kind {
        SourceKind::MusicEx {
            song_mid,
            media_filename,
        } => {
            let creds = creds.ok_or(ConvertError::NeedsLogin)?;
            let r = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(ConvertError::Cancelled),
                r = fetch_ekey(song_mid, media_filename, creds) => r,
            };
            Some(r.map_err(|e| match e {
                EkeyError::Rejected => ConvertError::NeedsLogin,
                EkeyError::Network(s) => ConvertError::Failed(format!("网络错误：{s}")),
            })?)
        }
        _ => None,
    };
    let dir = &opts.output_dir;
    let mut parts = Parts::default();
    let mut cur = parts.add(dir);

    progress(Stage::Decrypt);
    let d = {
        let (input, part, c) = (input.to_path_buf(), cur.clone(), cancel.clone());
        let gone = parts.1.clone();
        blocking(move || {
            let _orphan = Orphan(part.clone(), gone.clone());
            let f = std::fs::File::create_new(&part).map_err(io_err)?;
            let mut w = CancelWriter(f, c.clone(), gone);
            let d = match decrypt(&input, &kind, ekey.as_deref(), &mut w) {
                Err(_) if c.is_cancelled() => return Err(ConvertError::Cancelled),
                r => r?,
            };
            drop(w);
            if d.container == Container::Flac {
                crate::flac::trim_tail(&part).map_err(io_err)?;
            }
            Ok(d)
        })
        .await?
    };
    // lofty 读不了 ASF：WMA 的自带标签等转码后从 MP3 里读
    let src_tags = match d.container {
        Container::Wma => None,
        _ => Some(read_audio(&cur)?),
    };

    let mut container = d.container;
    let mut own_cover = None;
    // 已是 MP3 不重编码；WMA / APE 的标签 lofty 写不全，总要转成 MP3 / FLAC
    let target = match (&container, opts.format) {
        (Container::Mp3, _) => None,
        (Container::Wma, _) | (_, OutputFormat::Mp3) => Some(Container::Mp3),
        (Container::Ape, OutputFormat::Original) => Some(Container::Flac),
        (_, OutputFormat::Original) => None,
    };
    if let Some(target) = target {
        let ff = opts
            .ffmpeg
            .clone()
            .ok_or_else(|| ConvertError::Failed("缺少 ffmpeg".into()))?;
        progress(Stage::Transcode);
        // 转码用 -vn：WMA / APE 自带的封面（附带图片流）先原样抽出来
        if matches!(container, Container::Wma | Container::Ape) {
            own_cover = attached_picture(&ff, &cur, cancel).await?;
        }
        let next = parts.add(dir);
        let codec: &[&str] = match target {
            Container::Flac => &["-codec:a", "flac", "-f", "flac"],
            _ => &["-codec:a", "libmp3lame", "-b:a", "320k", "-f", "mp3"],
        };
        let mut child = tokio::process::Command::new(ff)
            .args(["-v", "error", "-nostdin", "-y", "-i"])
            .arg(&cur)
            // 原标签全部带上（Ogg 的标签在音频流上）；建模字段随后由 lofty 覆盖，封面也由 lofty 写
            .args(["-map_metadata", "0", "-map_metadata", "0:s:a:0"])
            .args(
                CONTAINER_KEYS
                    .iter()
                    .flat_map(|k| ["-metadata".into(), format!("{k}=")]),
            )
            .args(["-vn"])
            .args(codec)
            .arg(&next)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ConvertError::Failed(format!("无法启动 ffmpeg：{e}")))?;
        let status = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                return Err(ConvertError::Cancelled);
            }
            s = child.wait() => s.map_err(io_err)?,
        };
        if !status.success() {
            return Err(ConvertError::Failed(format!("ffmpeg 转码失败（{status}）")));
        }
        cur = next;
        container = target;
    }
    let src_tags = match src_tags {
        Some(t) => t,
        None => read_audio(&cur)?,
    };
    let mut meta = merge(d.meta, tag_of(&src_tags));
    drop(src_tags);
    if meta.cover.is_none() {
        meta.cover = own_cover;
    }

    progress(Stage::Metadata);
    tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(ConvertError::Cancelled),
        r = enricher.enrich(&mut meta) => if let Err(e) = r {
            log::warn!("元数据补全失败：{e}");
        },
    }
    let quality = {
        let (p, m, c) = (cur.clone(), meta.clone(), container.clone());
        blocking(move || write_tags(&p, &m, &c)).await?
    };
    if cancel.is_cancelled() {
        return Err(ConvertError::Cancelled);
    }

    let fallback = input.file_stem().unwrap_or_default().to_string_lossy();
    let stem = file_stem(meta.title.as_deref(), &meta.artists, &fallback);
    let lrc_text = meta.lyrics.as_ref().map(merge_lrc);
    let (output, renamed, lrc) =
        land(dir, &stem, ext(&container), &cur, lrc_text.as_deref()).map_err(io_err)?;
    Ok(ConvertOutcome {
        output,
        lrc,
        renamed,
        quality,
        has_cover: meta.cover.is_some(),
        has_lyrics: meta.lyrics.is_some(),
        meta,
    })
}

/// 依次：`REVEAL_FFMPEG`、可执行文件旁或 `../Resources/`、Homebrew、/usr/local、PATH。
pub fn find_ffmpeg() -> Option<PathBuf> {
    let mut c: Vec<PathBuf> = std::env::var_os("REVEAL_FFMPEG")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    if let Some(d) = std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::parent)
    {
        c.push(d.join("ffmpeg"));
        c.push(d.join("../Resources/ffmpeg"));
    }
    c.push("/opt/homebrew/bin/ffmpeg".into());
    c.push("/usr/local/bin/ffmpeg".into());
    if let Some(path) = std::env::var_os("PATH") {
        c.extend(std::env::split_paths(&path).map(|d| d.join("ffmpeg")));
    }
    first_executable(c)
}

pub(crate) fn first_executable(c: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    c.into_iter().find(|p| {
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}
