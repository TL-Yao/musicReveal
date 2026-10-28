//! `embed_lyrics` / `read_lyrics` / `lrc_home`：ffmpeg 生成的正弦素材，只写临时目录。

use super::pipeline::{ffmpeg, gen, png};
use super::Tmp;
use crate::*;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::{Picture, PictureType};
use lofty::prelude::{Accessor, ItemKey};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const LRC: &str = "[ti:x]\n[00:01.00]第一句\n[00:01.00]first\n[00:02.50]第二句\n";

const FORMATS: [(&str, &[&str]); 5] = [
    ("mp3", &["-c:a", "libmp3lame", "-b:a", "128k"]),
    ("flac", &["-c:a", "flac"]),
    ("ogg", &["-ac", "2", "-c:a", "vorbis", "-strict", "-2"]),
    ("m4a", &["-c:a", "aac", "-f", "ipod"]),
    ("wav", &[]),
];

/// 带标题、封面与两份旧歌词的素材。
fn sample(ff: &Path, t: &Tmp, ext: &str, args: &[&str]) -> (PathBuf, Vec<u8>) {
    let p = t.0.join(format!("a.{ext}"));
    let mut a = args.to_vec();
    a.extend(["-metadata", "title=原标题"]);
    gen(ff, &p, &a);
    let cover = png(ff, t);
    let mut f = lofty::read_from_path(&p).unwrap();
    if f.primary_tag().is_none() {
        f.insert_tag(lofty::tag::Tag::new(f.primary_tag_type()));
    }
    let tag = f.primary_tag_mut().unwrap();
    tag.set_title("原标题".into());
    let mut pic = Picture::from_reader(&mut &cover[..]).unwrap();
    pic.set_pic_type(PictureType::CoverFront);
    tag.push_picture(pic);
    tag.insert_text(ItemKey::UnsyncLyrics, "旧歌词一".into());
    tag.insert_text(ItemKey::Lyrics, "旧歌词二".into());
    f.save_to_path(&p, lofty::config::WriteOptions::default())
        .unwrap();
    (p, cover)
}

fn lyric_fields(p: &Path) -> Vec<String> {
    let f = lofty::read_from_path(p).unwrap();
    f.tags()
        .iter()
        .flat_map(|t| {
            t.get_strings(ItemKey::Lyrics)
                .chain(t.get_strings(ItemKey::UnsyncLyrics))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn parts_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(PART_PREFIX))
        .collect()
}

#[test]
fn embed_round_trips_in_every_format_and_keeps_other_tags() {
    let Some(ff) = ffmpeg() else { return };
    for (ext, args) in FORMATS {
        let t = Tmp::new(&format!("emb-{ext}"));
        let (p, cover) = sample(&ff, &t, ext, args);
        let dur = lofty::read_from_path(&p).unwrap().properties().duration();
        assert!(read_lyrics(&p).is_some(), "{ext}: 素材应带旧歌词");

        embed_lyrics(&p, LRC).unwrap_or_else(|e| panic!("{ext}: {e}"));
        assert_eq!(read_lyrics(&p).as_deref(), Some(LRC), "{ext}");
        assert_eq!(lyric_fields(&p), [LRC], "{ext}: 只剩一份歌词");
        let f = lofty::read_from_path(&p).unwrap();
        let tag = f.primary_tag().unwrap();
        assert_eq!(tag.title().as_deref(), Some("原标题"), "{ext}");
        assert_eq!(tag.pictures().len(), 1, "{ext}");
        assert_eq!(tag.pictures()[0].data(), &cover[..], "{ext}");
        assert_eq!(f.properties().duration(), dur, "{ext}");

        // 再写一次是替换
        embed_lyrics(&p, "[00:03.00]新\n").unwrap();
        assert_eq!(lyric_fields(&p), ["[00:03.00]新\n"], "{ext}");
        assert!(parts_in(&t.0).is_empty(), "{ext}");
    }
}

#[test]
fn blank_or_missing_lyrics_read_as_none() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("emb-blank");
    let p = t.0.join("a.flac");
    gen(&ff, &p, &["-c:a", "flac"]);
    assert_eq!(read_lyrics(&p), None);
    embed_lyrics(&p, " \n ").unwrap();
    assert_eq!(read_lyrics(&p), None);
    assert_eq!(read_lyrics(&t.0.join("nope.flac")), None);
}

#[test]
fn embed_keeps_birthtime_and_permissions() {
    use std::os::macos::fs::FileTimesExt;
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("emb-meta");
    let p = t.0.join("a.mp3");
    gen(&ff, &p, &["-c:a", "libmp3lame"]);
    let born = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    std::fs::File::options()
        .write(true)
        .open(&p)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_created(born))
        .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o640)).unwrap();
    assert_eq!(std::fs::metadata(&p).unwrap().created().unwrap(), born);

    embed_lyrics(&p, LRC).unwrap();
    let m = std::fs::metadata(&p).unwrap();
    assert_eq!(read_lyrics(&p).as_deref(), Some(LRC));
    assert_eq!(m.created().unwrap(), born);
    assert_eq!(m.permissions().mode() & 0o777, 0o640);
}

#[test]
fn symlinks_are_refused_and_left_alone() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("emb-link");
    let real = t.0.join("real.mp3");
    let before = gen(&ff, &real, &["-c:a", "libmp3lame"]);
    let link = t.0.join("link.mp3");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(embed_lyrics(&link, LRC).is_err());
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(std::fs::read_link(&link).unwrap(), real);
    assert_eq!(std::fs::read(&real).unwrap(), before);
    assert!(parts_in(&t.0).is_empty());
}

#[test]
fn failed_write_leaves_the_original_and_no_part_file() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("emb-fail");
    // 只读目录：建不了副本
    let dir = t.0.join("ro");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("a.mp3");
    let before = gen(&ff, &p, &["-c:a", "libmp3lame"]);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let r = embed_lyrics(&p, LRC);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(r.is_err());
    assert_eq!(std::fs::read(&p).unwrap(), before);
    assert!(parts_in(&dir).is_empty());

    // 只读文件：尊重用户的只读设置，不替换
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o444)).unwrap();
    assert!(embed_lyrics(&p, LRC).is_err());
    assert_eq!(std::fs::read(&p).unwrap(), before);
    assert!(parts_in(&dir).is_empty());
}

#[test]
fn an_open_reader_keeps_reading_the_old_content() {
    use std::io::Read;
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("emb-open");
    let p = t.0.join("a.flac");
    let before = gen(&ff, &p, &["-c:a", "flac"]);
    let mut r = std::fs::File::open(&p).unwrap();
    let mut head = [0u8; 100];
    r.read_exact(&mut head).unwrap();
    embed_lyrics(&p, LRC).unwrap();
    let mut rest = vec![];
    r.read_to_end(&mut rest).unwrap();
    assert_eq!([&head[..], &rest].concat(), before);
    assert_ne!(std::fs::read(&p).unwrap(), before);
}

#[test]
fn lrc_home_mirrors_the_subpath_under_lrc() {
    let root = Path::new("/m/out");
    assert_eq!(
        lrc_home(root, Path::new("/m/out/a/b/歌手 - 歌.v2.flac")),
        Path::new("/m/out/lrc/a/b/歌手 - 歌.v2.lrc")
    );
    assert_eq!(
        lrc_home(root, Path::new("/m/out/x.mp3")),
        Path::new("/m/out/lrc/x.lrc")
    );
    assert_eq!(
        lrc_home(root, Path::new("/elsewhere/sub/y.m4a")),
        Path::new("/m/out/lrc/y.lrc")
    );
}
