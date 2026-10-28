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
        format!("{} - {title}", artists.join(", "))
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

/// 目录里第一个不存在的 `stem.ext` / `stem (n).ext`，整个文件名 ≤ 200 字节。
pub(crate) fn unique_path(dir: &Path, stem: &str, ext: &str) -> (PathBuf, Option<u32>) {
    let mut n = 1u32;
    loop {
        let suffix = if n == 1 {
            format!(".{ext}")
        } else {
            format!(" ({n}).{ext}")
        };
        let p = dir.join(format!(
            "{}{suffix}",
            truncate(stem, MAX_NAME - suffix.len())
        ));
        if !p.exists() {
            return (p, (n > 1).then_some(n));
        }
        n += 1;
    }
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

#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
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
    Failed(String),
}

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConvertError::NeedsLogin => f.write_str("需要登录 QQ 音乐（未登录或登录已过期）"),
            ConvertError::Cancelled => f.write_str("已取消"),
            ConvertError::Incomplete => f.write_str("文件不完整（可能仍在下载或已被截断）"),
            ConvertError::Unsupported(s) => write!(f, "不支持的格式：{s}"),
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
            E::NeedsEkey | E::BadEkey => ConvertError::NeedsLogin,
            e => ConvertError::Failed(e.to_string()),
        }
    }
}

fn io_err(e: std::io::Error) -> ConvertError {
    ConvertError::Failed(format!("读写失败：{e}"))
}

/// 本次转换创建的临时文件；无论成功失败，离开作用域时全部删除（成功时已被 rename 走）。
struct Parts(Vec<PathBuf>);

impl Parts {
    fn add(&mut self, dir: &Path) -> PathBuf {
        use std::sync::atomic::AtomicU64;
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let p = dir.join(format!(
            ".{:x}{nanos:x}{:x}.part",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        self.0.push(p.clone());
        p
    }
}

impl Drop for Parts {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// 每次写之前看一眼取消标志，让流式解密能中途停下。
struct CancelWriter(std::fs::File, CancelToken);

impl Write for CancelWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.1.is_cancelled() {
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
    }
}

fn read_audio(p: &Path) -> Result<TaggedFile, ConvertError> {
    lofty::probe::Probe::open(p)
        .and_then(|p| Ok(p.guess_file_type()?))
        .and_then(|p| p.read())
        .map_err(|e| ConvertError::Failed(format!("无法读取解密后的音频：{e}")))
}

fn tag_of(f: &TaggedFile) -> Option<&Tag> {
    f.primary_tag().or_else(|| f.first_tag())
}

/// `EmbeddedMeta` 非空字段优先，其余取音频自带标签。
fn merge(e: EmbeddedMeta, tag: Option<&Tag>) -> TrackMeta {
    let s = |k: ItemKey| tag.and_then(|t| t.get_string(k)).map(str::to_owned);
    let artists = if e.artists.is_empty() {
        tag.map(|t| {
            t.get_strings(ItemKey::TrackArtist)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
    } else {
        e.artists
    };
    TrackMeta {
        title: e.title.or_else(|| s(ItemKey::TrackTitle)),
        artists,
        album: e.album.or_else(|| s(ItemKey::AlbumTitle)),
        album_artist: s(ItemKey::AlbumArtist),
        year: tag.and_then(|t| t.date()).map(|d| d.year),
        track: tag.and_then(|t| t.track()),
        disc: tag.and_then(|t| t.disk()),
        cover: e.cover.or_else(|| {
            let pics = tag?.pictures();
            pics.iter()
                .find(|p| p.pic_type() == PictureType::CoverFront)
                .or(pics.first())
                .map(|p| p.data().to_vec())
        }),
        lyrics: None,
        song_id: e.song_id,
        platform: e.platform,
    }
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
        t.insert_text(ItemKey::Lyrics, strip_timestamps(&l.lrc));
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

fn quality(f: &TaggedFile, c: &Container) -> String {
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

/// 去掉 LRC 时间轴与 `[ar:..]` 等标记，只留歌词正文。
fn strip_timestamps(lrc: &str) -> String {
    lrc.lines()
        .map(|l| {
            let mut r = l.trim();
            while let (true, Some(end)) = (r.starts_with('['), r.find(']')) {
                r = r[end + 1..].trim_start();
            }
            r
        })
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn time_tag(l: &str) -> Option<&str> {
    let end = l.find(']')?;
    l[1..end]
        .starts_with(|c: char| c.is_ascii_digit())
        .then(|| &l[..=end])
        .filter(|_| l.starts_with('['))
}

/// 原文每行之后紧跟同时间戳的译文。
fn merge_lrc(l: &Lyrics) -> String {
    let tr: std::collections::HashMap<&str, &str> = l
        .translated_lrc
        .iter()
        .flat_map(|t| t.lines())
        .filter_map(|x| {
            let tag = time_tag(x.trim())?;
            let text = x.trim()[tag.len()..].trim();
            (!text.is_empty()).then_some((tag, text))
        })
        .collect();
    let mut out = String::new();
    for line in l.lrc.lines() {
        out += line;
        out += "\n";
        if let Some(text) = time_tag(line.trim()).and_then(|t| tr.get(t).map(|x| (t, x))) {
            out += &format!("{}{}\n", text.0, text.1);
        }
    }
    out
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
            Some(
                fetch_ekey(song_mid, media_filename, creds)
                    .await
                    .map_err(|e| match e {
                        EkeyError::Rejected => ConvertError::NeedsLogin,
                        EkeyError::Network(s) => ConvertError::Failed(format!("网络错误：{s}")),
                    })?,
            )
        }
        _ => None,
    };
    let dir = &opts.output_dir;
    let mut parts = Parts(vec![]);
    let mut cur = parts.add(dir);

    progress(Stage::Decrypt);
    let d = {
        let (input, part, c) = (input.to_path_buf(), cur.clone(), cancel.clone());
        blocking(move || {
            let mut w = CancelWriter(std::fs::File::create_new(&part).map_err(io_err)?, c.clone());
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
    let src_tags = read_audio(&cur)?;
    let mut meta = merge(d.meta, tag_of(&src_tags));
    drop(src_tags);

    let mut container = d.container;
    if opts.format == OutputFormat::Mp3 && container != Container::Mp3 {
        let ff = opts
            .ffmpeg
            .clone()
            .ok_or_else(|| ConvertError::Failed("缺少 ffmpeg".into()))?;
        progress(Stage::Transcode);
        let mp3 = parts.add(dir);
        let mut child = tokio::process::Command::new(ff)
            .args(["-v", "error", "-nostdin", "-y", "-i"])
            .arg(&cur)
            .args(["-map_metadata", "-1", "-vn", "-codec:a", "libmp3lame"])
            .args(["-b:a", "320k", "-f", "mp3"])
            .arg(&mp3)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ConvertError::Failed(format!("无法启动 ffmpeg：{e}")))?;
        let status = loop {
            if cancel.is_cancelled() {
                let _ = child.kill().await;
                return Err(ConvertError::Cancelled);
            }
            tokio::select! {
                s = child.wait() => break s.map_err(io_err)?,
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
            }
        };
        if !status.success() {
            return Err(ConvertError::Failed(format!("ffmpeg 转码失败（{status}）")));
        }
        cur = mp3;
        container = Container::Mp3;
    }

    progress(Stage::Metadata);
    if let Err(e) = enricher.enrich(&mut meta).await {
        log::warn!("元数据补全失败：{e}");
    }
    if cancel.is_cancelled() {
        return Err(ConvertError::Cancelled);
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
    let (output, renamed) = unique_path(dir, &stem, ext(&container));
    std::fs::rename(&cur, &output).map_err(io_err)?;
    let lrc = match &meta.lyrics {
        Some(l) => {
            let p = output.with_extension("lrc");
            std::fs::write(&p, merge_lrc(l)).map_err(io_err)?;
            Some(p)
        }
        None => None,
    };
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
    c.into_iter().find(|p| p.is_file())
}
