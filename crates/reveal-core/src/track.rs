//! 资料库用：读输出文件夹里普通音频的标签、封面，以及由文件名推歌名。

use crate::convert::{front_cover, lyrics_of, merge, quality, read_audio, tags_of, Parts};
use crate::{Container, EmbeddedMeta, Error, Platform};
use lofty::file::{AudioFile, FileType, TaggedFileExt};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct TrackInfo {
    pub title: Option<String>,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub year: Option<u32>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub duration_ms: u64,
    pub quality: String,
    pub has_cover: bool,
}

pub fn read_track(path: &Path) -> Result<TrackInfo, Error> {
    std::fs::File::open(path)?;
    let f = read_audio(path).map_err(|e| Error::Corrupt(e.to_string()))?;
    let empty = EmbeddedMeta {
        title: None,
        artists: vec![],
        album: None,
        cover: None,
        song_id: None,
        platform: Platform::Other,
    };
    let m = merge(empty, &tags_of(&f));
    let mut artists: Vec<String> = vec![];
    for a in m.artists.iter().flat_map(|a| split_artists(a)) {
        if !artists.contains(&a) {
            artists.push(a);
        }
    }
    let container = match f.file_type() {
        FileType::Flac => Some(Container::Flac),
        FileType::Mpeg => Some(Container::Mp3),
        FileType::Mp4 => Some(Container::M4a),
        FileType::Vorbis | FileType::Opus | FileType::Speex => Some(Container::Ogg),
        FileType::Wav => Some(Container::Wav),
        FileType::Ape => Some(Container::Ape),
        _ => None,
    };
    let quality = match container {
        Some(c) => quality(&f, &c),
        None => format!("{:?}", f.file_type()),
    };
    Ok(TrackInfo {
        title: m.title.map(|s| s.trim().to_owned()),
        artists,
        album: m.album.map(|s| s.trim().to_owned()),
        album_artist: m.album_artist.map(|s| s.trim().to_owned()),
        year: m.year.map(u32::from),
        track: m.track,
        disc: m.disc,
        duration_ms: f.properties().duration().as_millis() as u64,
        quality,
        has_cover: m.cover.is_some(),
    })
}

pub fn read_cover(path: &Path) -> Option<Vec<u8>> {
    tags_of(&read_audio(path).ok()?)
        .into_iter()
        .find_map(front_cover)
}

/// 按 `, ` `/` `、` `;` 拆歌手。
pub fn split_artists(s: &str) -> Vec<String> {
    s.replace(", ", "/")
        .split(['/', '、', ';'])
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `歌手 - 歌名.ext` / `歌手-歌名.ext` → 歌名；没有分隔符就是去掉扩展名的原名。
pub fn title_from_file_name(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_owned());
    let t = stem
        .split_once(" - ")
        .or_else(|| stem.split_once('-'))
        .map_or(stem.as_str(), |(_, t)| t)
        .trim();
    if t.is_empty() {
        stem.clone()
    } else {
        t.to_owned()
    }
}

/// 内嵌歌词原文；没有或空白为 None。
pub fn read_lyrics(path: &Path) -> Option<String> {
    lyrics_of(&tags_of(&read_audio(path).ok()?))
}

/// 把 `lrc` 写进歌词字段（先删掉已有的歌词字段）。同目录隐藏副本写好、经 ffmpeg 核对除歌词外的
/// 元数据与附图都没变之后 rename 换上：任一步失败原文件不变；保留创建时间与权限。
/// 符号链接、只读文件、找不到 ffmpeg 时不写。
pub fn embed_lyrics(path: &Path, lrc: &str) -> Result<(), Error> {
    use std::io::{Error as IoError, ErrorKind};
    let m = std::fs::symlink_metadata(path)?;
    if !m.is_file() {
        return Err(IoError::new(ErrorKind::InvalidInput, "不是普通文件（符号链接不写入）").into());
    }
    // 用户设为只读的不改
    std::fs::OpenOptions::new().write(true).open(path)?;
    let ff =
        crate::find_ffmpeg().ok_or_else(|| IoError::other("找不到 ffmpeg，无法核对，不写入"))?;
    let mut parts = Parts::default();
    let tmp = parts.add(path.parent().unwrap_or(Path::new(".")));
    std::fs::copy(path, &tmp)?;
    set_lyrics(&tmp, lrc)?;
    if survey(&ff, path)? != survey(&ff, &tmp)? {
        return Err(IoError::other("改写会丢失或改动歌词以外的元数据，已放弃").into());
    }
    if let Ok(born) = m.created() {
        use std::os::macos::fs::FileTimesExt;
        std::fs::File::options()
            .write(true)
            .open(&tmp)?
            .set_times(std::fs::FileTimes::new().set_created(born))?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 用各格式自己的标签类型只改歌词项，其余帧 / atom / 注释原样写回。
fn set_lyrics(p: &Path, lrc: &str) -> Result<(), Error> {
    use lofty::config::{ParseOptions, WriteOptions};
    use lofty::flac::FlacFile;
    use lofty::id3::v2::{Frame, FrameId, Id3v2Tag, Id3v2Version, UnsynchronizedTextFrame};
    use lofty::iff::wav::WavFile;
    use lofty::mp4::{Atom, AtomData, AtomIdent, Mp4File};
    use lofty::mpeg::MpegFile;
    use lofty::ogg::tag::VorbisComments;
    use lofty::ogg::{OpusFile, SpeexFile, VorbisFile};
    use lofty::tag::TagExt;
    fn bad(e: impl std::fmt::Display) -> Error {
        Error::Corrupt(e.to_string())
    }
    let id3 = |tag: Option<&Id3v2Tag>| -> Result<(), Error> {
        let mut tag = tag.cloned().unwrap_or_default();
        // ID3v2.2 里升级不了的帧 lofty 写不出来：不改
        if tag.iter().any(|f| !f.id().is_valid()) {
            return Err(Error::Unsupported("ID3v2.2 标签里有无法保留的帧".into()));
        }
        tag.remove(&FrameId::Valid("USLT".into())).for_each(drop);
        tag.insert(Frame::UnsynchronizedText(UnsynchronizedTextFrame::new(
            lofty::TextEncoding::UTF8,
            *b"XXX",
            "",
            lrc.to_owned(),
        )));
        let v23 = tag.original_version() == Id3v2Version::V3;
        tag.save_to_path(p, WriteOptions::default().use_id3v23(v23))
            .map_err(bad)
    };
    let vorbis = |mut tag: VorbisComments| -> Result<VorbisComments, Error> {
        tag.remove("LYRICS").for_each(drop);
        tag.remove("UNSYNCEDLYRICS").for_each(drop);
        tag.insert("LYRICS".into(), lrc.to_owned());
        Ok(tag)
    };
    let ty = lofty::probe::Probe::open(p)
        .and_then(|p| Ok(p.guess_file_type()?))
        .map_err(bad)?
        .file_type();
    let f = &mut std::fs::File::open(p)?;
    let po = ParseOptions::new();
    let wo = WriteOptions::default();
    match ty {
        Some(FileType::Mpeg) => id3(MpegFile::read_from(f, po).map_err(bad)?.id3v2()),
        Some(FileType::Wav) => id3(WavFile::read_from(f, po).map_err(bad)?.id3v2()),
        Some(FileType::Flac) => {
            // 图片在 FlacFile 里另存，整个文件一起写回才不丢
            let mut flac = FlacFile::read_from(f, po).map_err(bad)?;
            let tag = flac.remove_vorbis_comments().unwrap_or_default();
            flac.set_vorbis_comments(vorbis(tag)?);
            let mut w = std::fs::File::options().read(true).write(true).open(p)?;
            flac.save_to(&mut w, wo)
                .map_err(|e| Error::Corrupt(e.to_string()))
        }
        Some(FileType::Vorbis) => vorbis(
            VorbisFile::read_from(f, po)
                .map_err(bad)?
                .vorbis_comments()
                .clone(),
        )?
        .save_to_path(p, wo)
        .map_err(bad),
        Some(FileType::Opus) => vorbis(
            OpusFile::read_from(f, po)
                .map_err(bad)?
                .vorbis_comments()
                .clone(),
        )?
        .save_to_path(p, wo)
        .map_err(bad),
        Some(FileType::Speex) => vorbis(
            SpeexFile::read_from(f, po)
                .map_err(bad)?
                .vorbis_comments()
                .clone(),
        )?
        .save_to_path(p, wo)
        .map_err(bad),
        Some(FileType::Mp4) => {
            let mut ilst = Mp4File::read_from(f, po)
                .map_err(bad)?
                .ilst()
                .cloned()
                .unwrap_or_default();
            let lyr = AtomIdent::Fourcc(*b"\xa9lyr");
            ilst.remove(&lyr).for_each(drop);
            ilst.insert(Atom::new(lyr, AtomData::UTF8(lrc.to_owned())));
            ilst.save_to_path(p, wo).map_err(bad)
        }
        t => Err(Error::Unsupported(format!("{t:?}"))),
    }
}

/// ffmpeg 眼里的文件：各段（全局 / 每条流 / 章节）元数据去掉歌词键后排序，加上每条流的包哈希
/// （附图字节、音频数据都在内）。
fn survey(ff: &Path, p: &Path) -> Result<(Vec<Vec<String>>, Vec<u8>), Error> {
    let o = std::process::Command::new(ff)
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(p)
        .args([
            "-map",
            "0",
            "-c",
            "copy",
            "-f",
            "streamhash",
            "-hash",
            "md5",
            "-",
        ])
        .stdin(std::process::Stdio::null())
        .output()?;
    if !o.status.success() {
        return Err(std::io::Error::other(format!("ffmpeg 读不了 {}", p.display())).into());
    }
    Ok((sections(&String::from_utf8_lossy(&o.stderr)), o.stdout))
}

fn is_lyrics_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    k.starts_with("lyrics") || k == "unsyncedlyrics" || k == "uslt"
}

/// ffmpeg 输入信息里的 `Metadata:` 段。续行（键为空）拼进上一条。
fn sections(dump: &str) -> Vec<Vec<String>> {
    let mut secs: Vec<Vec<String>> = vec![vec![]];
    let mut meta: Option<usize> = None;
    let mut skip = false;
    for l in dump.lines() {
        if l.starts_with("Output #") || l.starts_with("Stream mapping") {
            break;
        }
        let t = l.trim();
        let indent = l.len() - l.trim_start().len();
        if meta.is_some_and(|m| indent > m) {
            let sec = secs.last_mut().expect("至少一段");
            let (k, v) = t.split_once(':').unwrap_or((t, ""));
            let (k, v) = (k.trim(), v.trim());
            if !k.is_empty() {
                skip = is_lyrics_key(k);
                if !skip {
                    sec.push(format!("{k}={v}"));
                }
            } else if let Some(e) = sec.last_mut().filter(|_| !skip) {
                *e += "\n";
                *e += v;
            }
            continue;
        }
        meta = None;
        if t == "Metadata:" {
            meta = Some(indent);
        } else if ["Input #", "Stream #", "Chapter #"]
            .iter()
            .any(|h| t.starts_with(h))
        {
            secs.push(vec![]);
        }
    }
    for s in &mut secs {
        s.sort();
    }
    secs
}
