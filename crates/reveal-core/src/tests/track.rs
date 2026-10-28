//! 普通音频入库与资料库读标签：`is_candidate` / `sniff` / `convert` / `read_track` 等。

use super::{aes_ecb_pkcs7, audio, b64, Tmp, NCM_META_KEY};
use crate::*;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::Accessor;
use std::path::{Path, PathBuf};
use std::process::Command;

fn ffmpeg() -> Option<PathBuf> {
    let f = find_ffmpeg();
    if f.is_none() {
        eprintln!("跳过：找不到 ffmpeg（可设 REVEAL_FFMPEG）");
    }
    f
}

/// 2 秒 440Hz 正弦波，按 `args` 编码到 `out`。
fn gen(ff: &Path, out: &Path, args: &[&str]) {
    let st = Command::new(ff)
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=f=440:d=2"])
        .args(args)
        .arg(out)
        .status()
        .unwrap();
    assert!(st.success());
}

fn png(ff: &Path, t: &Tmp) -> Vec<u8> {
    let p = t.0.join("c.png");
    let st = Command::new(ff)
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=red:s=16x16",
        ])
        .args(["-frames:v", "1"])
        .arg(&p)
        .status()
        .unwrap();
    assert!(st.success());
    std::fs::read(p).unwrap()
}

fn add_cover(p: &Path, png: &[u8]) {
    use lofty::picture::{MimeType, Picture, PictureType};
    let mut f = lofty::read_from_path(p).unwrap();
    let t = f.primary_tag_mut().unwrap();
    for (ty, data) in [
        (PictureType::Other, &b"other"[..]),
        (PictureType::CoverFront, png),
    ] {
        let pic = Picture::unchecked(data.to_vec())
            .pic_type(ty)
            .mime_type(MimeType::Png);
        t.push_picture(pic.build());
    }
    f.save_to_path(p, lofty::config::WriteOptions::default())
        .unwrap();
}

#[test]
fn plain_audio_extensions_are_candidates_and_sniff_as_plain() {
    let t = Tmp::new("tr-sniff");
    let cases: [(&str, &[u8], Container); 6] = [
        ("a.mp3", b"ID3\x03\x00\x00\x00\x00\x00\x00", Container::Mp3),
        ("b.MP3", &[0xFF, 0xFB, 0x90, 0x64], Container::Mp3),
        ("c.flac", b"fLaC", Container::Flac),
        ("d.ogg", b"OggS", Container::Ogg),
        ("e.m4a", b"\0\0\0\x20ftypM4A ", Container::M4a),
        ("f.Wav", b"RIFF\x24\0\0\0WAVE", Container::Wav),
    ];
    for (name, magic, c) in cases {
        let p = t.write(name, &audio(magic, 5000));
        assert!(is_candidate(&p), "{name}");
        assert_eq!(sniff(&p).unwrap(), SourceKind::Plain(c), "{name}");
    }
    assert!(!is_candidate(Path::new("a.txt")));
    assert!(!is_candidate(Path::new("a.lrc")));
}

#[test]
fn plain_audio_with_wrong_or_no_content() {
    let t = Tmp::new("tr-bad");
    let p = t.write("a.mp3", &audio(b"", 5000));
    assert!(
        matches!(sniff(&p), Err(Error::Corrupt(_))),
        "{:?}",
        sniff(&p)
    );
    // 内容是 FLAC 但扩展名是 .mp3：与容器不符
    let p = t.write("b.mp3", &audio(b"fLaC", 5000));
    assert!(
        matches!(sniff(&p), Err(Error::Corrupt(_))),
        "{:?}",
        sniff(&p)
    );
    for (name, data) in [("c.mp3", &b""[..]), ("d.flac", b"fLa"), ("e.wav", b"RIFF")] {
        let p = t.write(name, data);
        assert!(matches!(sniff(&p), Err(Error::Incomplete)), "{name}");
    }
}

fn key163(json: &str) -> String {
    let enc = aes_ecb_pkcs7(NCM_META_KEY, format!("music:{json}").as_bytes());
    format!("163 key(Don't modify):{}", b64(&enc))
}

async fn convert_noop(input: &Path, out: &Path) -> ConvertOutcome {
    let opts = ConvertOptions {
        output_dir: out.to_path_buf(),
        format: OutputFormat::Original,
        ffmpeg: find_ffmpeg(),
    };
    convert(
        input,
        &opts,
        &NoopEnricher,
        None,
        &CancelToken::default(),
        &mut |_| {},
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn plain_mp3_with_163_key_is_netease() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("tr-163");
    let out = t.0.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let src = t.0.join("x.mp3");
    gen(
        &ff,
        &src,
        &["-c:a", "libmp3lame", "-metadata", "title=自带"],
    );
    // 网易云的 mp3 把 163 key 放在 ID3 COMM；新版 ffmpeg 的 comment 写成 TXXX，所以用 lofty 写
    let mut f = lofty::read_from_path(&src).unwrap();
    f.primary_tag_mut().unwrap().set_comment(key163(
        r#"{"musicId":123,"musicName":"歌","artist":[["甲",1]],"album":"辑"}"#,
    ));
    f.save_to_path(&src, lofty::config::WriteOptions::default())
        .unwrap();
    let o = convert_noop(&src, &out).await;
    assert_eq!(o.meta.song_id, Some(SongId::Netease(123)));
    assert_eq!(o.meta.platform, Platform::Netease);
    assert_eq!(o.meta.title.as_deref(), Some("歌"));
    assert_eq!(o.meta.artists, ["甲"]);
    assert_eq!(o.meta.album.as_deref(), Some("辑"));
    assert!(o.output.ends_with("甲 - 歌.mp3"), "{:?}", o.output);
}

#[tokio::test]
async fn plain_mp3_without_163_key_is_other_and_keeps_tags() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("tr-other");
    let out = t.0.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let src = t.0.join("y.mp3");
    let args = [
        "-c:a",
        "libmp3lame",
        "-metadata",
        "title=自带标题",
        "-metadata",
        "artist=自带歌手",
        "-metadata",
        "comment=普通注释",
    ];
    gen(&ff, &src, &args);
    let o = convert_noop(&src, &out).await;
    assert_eq!(o.meta.platform, Platform::Other);
    assert_eq!(o.meta.song_id, None);
    assert_eq!(o.meta.title.as_deref(), Some("自带标题"));
    assert_eq!(o.meta.artists, ["自带歌手"]);
    let r = read_track(&o.output).unwrap();
    assert_eq!(r.title.as_deref(), Some("自带标题"));
    assert_eq!(r.artists, ["自带歌手"]);
}

#[test]
fn encrypted_extension_with_plain_content_stays_qq() {
    let t = Tmp::new("tr-qq");
    let p = t.write("a.mflac", &audio(b"fLaC", 5000));
    let kind = sniff(&p).unwrap();
    let d = decrypt(&p, &kind, None, &mut std::io::sink()).unwrap();
    assert_eq!(d.meta.platform, Platform::Qq);
}

#[test]
fn read_track_flac24_and_mp3() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("tr-read");
    let cover = png(&ff, &t);
    let flac = t.0.join("a.flac");
    gen(
        &ff,
        &flac,
        &[
            "-ar",
            "48000",
            "-sample_fmt",
            "s32",
            "-c:a",
            "flac",
            "-metadata",
            "title=标题一",
            "-metadata",
            "artist=A, B",
            "-metadata",
            "album=专辑一",
            "-metadata",
            "album_artist=群星",
            "-metadata",
            "date=2021",
            "-metadata",
            "track=3",
            "-metadata",
            "disc=2",
        ],
    );
    add_cover(&flac, &cover);
    let mp3 = t.0.join("b.mp3");
    gen(
        &ff,
        &mp3,
        &[
            "-c:a",
            "libmp3lame",
            "-b:a",
            "320k",
            "-metadata",
            "title=标题二",
            "-metadata",
            "artist=A/B",
            "-metadata",
            "album=专辑二",
            "-metadata",
            "date=1999",
            "-metadata",
            "track=7",
        ],
    );

    let f = read_track(&flac).unwrap();
    assert_eq!(f.title.as_deref(), Some("标题一"));
    assert_eq!(f.artists, ["A", "B"]);
    assert_eq!(f.album.as_deref(), Some("专辑一"));
    assert_eq!(f.album_artist.as_deref(), Some("群星"));
    assert_eq!((f.year, f.track, f.disc), (Some(2021), Some(3), Some(2)));
    assert!(f.duration_ms.abs_diff(2000) <= 50, "{}", f.duration_ms);
    assert_eq!(f.quality, "FLAC 24bit/48kHz");
    assert!(f.has_cover);
    assert_eq!(read_cover(&flac).as_deref(), Some(&cover[..]));

    let m = read_track(&mp3).unwrap();
    assert_eq!(m.title.as_deref(), Some("标题二"));
    assert_eq!(m.artists, ["A", "B"]);
    assert_eq!(m.album.as_deref(), Some("专辑二"));
    assert_eq!((m.year, m.track, m.disc), (Some(1999), Some(7), None));
    assert!(m.duration_ms.abs_diff(2000) <= 50, "{}", m.duration_ms);
    assert_eq!(m.quality, "MP3 320k");
    assert!(!m.has_cover);
    assert_eq!(read_cover(&mp3), None);
}

#[test]
fn read_track_rejects_non_audio() {
    let t = Tmp::new("tr-nonaudio");
    let p = t.write("a.mp3", &audio(b"", 5000));
    assert!(read_track(&p).is_err());
    assert_eq!(read_cover(&p), None);
    assert!(read_track(&t.0.join("missing.flac")).is_err());
}

#[test]
fn split_artists_handles_every_separator() {
    assert_eq!(split_artists("A, B"), ["A", "B"]);
    assert_eq!(split_artists("A/B"), ["A", "B"]);
    assert_eq!(split_artists("A、B"), ["A", "B"]);
    assert_eq!(split_artists("A;B"), ["A", "B"]);
    assert_eq!(
        split_artists(" HOYO-MiX / 陶典、Hanser ;; , "),
        ["HOYO-MiX", "陶典", "Hanser"]
    );
    assert!(split_artists("  ").is_empty());
}

#[test]
fn title_from_file_name_examples() {
    for (name, want) in [
        ("Hanser - 不需要陪伴的闲暇.ncm", "不需要陪伴的闲暇"),
        ("hanser,七海Nanami-元气女孩.mflac", "元气女孩"),
        ("DMYoung, Hanser - 「交织together」.mp3", "「交织together」"),
        ("单名.flac", "单名"),
        ("A - B - C.mp3", "B - C"),
        ("Artist - .mp3", "Artist - "),
        ("无扩展名", "无扩展名"),
    ] {
        assert_eq!(title_from_file_name(name), want, "{name}");
    }
}
