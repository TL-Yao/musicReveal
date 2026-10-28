//! 资料库用：读输出文件夹里普通音频的标签、封面，以及由文件名推歌名。

use crate::convert::{front_cover, quality, read_audio, tag_of};
use crate::{Container, Error};
use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::prelude::{Accessor, ItemKey};
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
    let tag = tag_of(&f);
    let s = |k: ItemKey| {
        tag.and_then(|t| t.get_string(k))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let mut artists: Vec<String> = vec![];
    for a in tag
        .into_iter()
        .flat_map(|t| t.get_strings(ItemKey::TrackArtist))
        .flat_map(split_artists)
    {
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
        None => format!("{}k", f.properties().audio_bitrate().unwrap_or(0)),
    };
    Ok(TrackInfo {
        title: s(ItemKey::TrackTitle),
        artists,
        album: s(ItemKey::AlbumTitle),
        album_artist: s(ItemKey::AlbumArtist),
        year: tag.and_then(|t| t.date()).map(|d| d.year.into()),
        track: tag.and_then(|t| t.track()),
        disc: tag.and_then(|t| t.disk()),
        duration_ms: f.properties().duration().as_millis() as u64,
        quality,
        has_cover: tag.is_some_and(|t| !t.pictures().is_empty()),
    })
}

pub fn read_cover(path: &Path) -> Option<Vec<u8>> {
    front_cover(tag_of(&read_audio(path).ok()?)?)
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
