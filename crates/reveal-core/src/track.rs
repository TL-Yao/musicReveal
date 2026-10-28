//! 资料库用：读输出文件夹里普通音频的标签、封面，以及由文件名推歌名。

use crate::convert::{front_cover, merge, quality, read_audio, tags_of};
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
