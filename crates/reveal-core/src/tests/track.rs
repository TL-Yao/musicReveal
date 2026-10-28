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
    png_of(ff, t, "red")
}

fn png_of(ff: &Path, t: &Tmp, color: &str) -> Vec<u8> {
    let p = t.0.join(format!("{color}.png"));
    let src = format!("color={color}:s=16x16");
    let st = Command::new(ff)
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", &src])
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
        (
            "a.mp3",
            b"ID3\x03\x00\x00\x00\x00\x00\x00\xFF\xFB\x90\x64",
            Container::Mp3,
        ),
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
    convert_with(input, out, OutputFormat::Original, &NoopEnricher).await
}

async fn convert_with(
    input: &Path,
    out: &Path,
    format: OutputFormat,
    enricher: &dyn Enricher,
) -> ConvertOutcome {
    std::fs::create_dir_all(out).unwrap();
    let opts = ConvertOptions {
        output_dir: out.to_path_buf(),
        format,
        ffmpeg: find_ffmpeg(),
    };
    convert(
        input,
        &opts,
        enricher,
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

/// ID3v2.4 头：`size` 字节的标签体（synchsafe 长度），体内容填 0。
fn id3(size: u32) -> Vec<u8> {
    let ss = [
        (size >> 21) & 0x7F,
        (size >> 14) & 0x7F,
        (size >> 7) & 0x7F,
        size & 0x7F,
    ];
    let mut v = b"ID3\x04\x00\x00".to_vec();
    v.extend(ss.map(|b| b as u8));
    v.extend(vec![0u8; size as usize]);
    v
}

#[test]
fn plain_sniff_skips_id3_and_zero_padding_and_knows_rf64() {
    let t = Tmp::new("tr-lenient");
    let frame = audio(&[0xFF, 0xFB, 0x90, 0x64], 5000);
    let cases: [(&str, Vec<u8>, Container); 6] = [
        (
            "a.flac",
            [id3(0), audio(b"fLaC", 5000)].concat(),
            Container::Flac,
        ),
        (
            "b.flac",
            [id3(300), audio(b"fLaC", 5000)].concat(),
            Container::Flac,
        ),
        ("c.mp3", [&[0u8; 64][..], &frame].concat(), Container::Mp3),
        (
            "d.mp3",
            [id3(40), vec![0u8; 64], frame.clone()].concat(),
            Container::Mp3,
        ),
        (
            "e.wav",
            audio(b"RF64\xff\xff\xff\xffWAVE", 5000),
            Container::Wav,
        ),
        ("f.mp3", [id3(40), frame].concat(), Container::Mp3),
    ];
    for (name, data, c) in cases {
        let p = t.write(name, &data);
        let kind = sniff(&p).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(kind, SourceKind::Plain(c.clone()), "{name}");
        let mut out = Vec::new();
        let d = decrypt(&p, &kind, None, &mut out).unwrap();
        assert_eq!(d.container, c, "{name}");
        assert!(out == data, "{name}");
    }
    // 标签声称的长度超出已到数据：还在下载
    let p = t.write("g.flac", &id3(1000)[..200]);
    assert!(
        matches!(sniff(&p), Err(Error::Incomplete)),
        "{:?}",
        sniff(&p)
    );
    let p = t.write("h.mp3", &[0u8; 30]);
    assert!(
        matches!(sniff(&p), Err(Error::Incomplete)),
        "{:?}",
        sniff(&p)
    );
    let p = t.write("i.mp3", &[id3(10), audio(b"", 5000)].concat());
    assert!(
        matches!(sniff(&p), Err(Error::Corrupt(_))),
        "{:?}",
        sniff(&p)
    );
}

#[tokio::test]
async fn lenient_plain_sources_convert() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("tr-lenient-cv");
    let flac = t.0.join("s.flac");
    gen(&ff, &flac, &["-c:a", "flac", "-metadata", "title=前置标签"]);
    let id3flac = t.write(
        "a.flac",
        &[id3(100), std::fs::read(&flac).unwrap()].concat(),
    );
    let mp3 = t.0.join("s.mp3");
    gen(&ff, &mp3, &["-c:a", "libmp3lame", "-write_id3v2", "0"]);
    let zmp3 = t.write(
        "b.mp3",
        &[&[0u8; 64][..], &std::fs::read(&mp3).unwrap()].concat(),
    );
    let rf64 = t.0.join("c.wav");
    gen(
        &ff,
        &rf64,
        &["-rf64", "always", "-metadata", "title=大文件"],
    );
    for (src, want) in [(&id3flac, "flac"), (&zmp3, "mp3"), (&rf64, "flac")] {
        let o = convert_noop(src, &t.0.join("out")).await;
        assert_eq!(o.output.extension().unwrap(), want, "{src:?}");
        let r = read_track(&o.output).unwrap();
        assert!(
            r.duration_ms.abs_diff(2000) <= 50,
            "{src:?} {}",
            r.duration_ms
        );
    }
    let o = convert_with(
        &rf64,
        &t.0.join("out-mp3"),
        OutputFormat::Mp3,
        &NoopEnricher,
    )
    .await;
    assert_eq!(o.output.extension().unwrap(), "mp3");
    assert_eq!(o.meta.title.as_deref(), Some("大文件"));
}

fn push_pics(p: &Path, pics: &[(lofty::picture::PictureType, &[u8])]) {
    use lofty::picture::{MimeType, Picture};
    let mut f = lofty::probe::Probe::open(p)
        .unwrap()
        .guess_file_type()
        .unwrap()
        .read()
        .unwrap();
    if f.primary_tag().is_none() {
        f.insert_tag(lofty::tag::Tag::new(f.primary_tag_type()));
    }
    let t = f.primary_tag_mut().unwrap();
    for (ty, data) in pics {
        let pic = Picture::unchecked(data.to_vec())
            .pic_type(*ty)
            .mime_type(MimeType::Png);
        t.push_picture(pic.build());
    }
    f.save_to_path(p, lofty::config::WriteOptions::default())
        .unwrap();
}

fn pics_of(p: &Path) -> Vec<Vec<u8>> {
    let f = lofty::probe::Probe::open(p)
        .unwrap()
        .guess_file_type()
        .unwrap()
        .read()
        .unwrap();
    let mut v: Vec<Vec<u8>> = f
        .primary_tag()
        .unwrap()
        .pictures()
        .iter()
        .map(|p| p.data().to_vec())
        .collect();
    v.sort();
    v
}

struct CoverEnricher(Vec<u8>);

#[async_trait::async_trait]
impl Enricher for CoverEnricher {
    async fn enrich(&self, meta: &mut TrackMeta) -> Result<(), String> {
        meta.cover = Some(self.0.clone());
        Ok(())
    }
}

#[tokio::test]
async fn every_source_picture_is_kept() {
    use lofty::picture::PictureType::{CoverBack, CoverFront};
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("tr-pics");
    let (front, back, new) = (
        png_of(&ff, &t, "red"),
        png_of(&ff, &t, "blue"),
        png_of(&ff, &t, "green"),
    );
    let fmts: [(&str, &[&str]); 4] = [
        ("a.mp3", &["-c:a", "libmp3lame"]),
        ("b.flac", &["-c:a", "flac"]),
        ("c.ogg", &["-c:a", "libopus"]),
        ("d.m4a", &["-c:a", "aac"]),
    ];
    for (name, args) in fmts {
        let src = t.0.join(name);
        gen(&ff, &src, args);
        push_pics(&src, &[(CoverFront, &front), (CoverBack, &back)]);
        let mut want = vec![front.clone(), back.clone()];
        want.sort();
        let o = convert_noop(&src, &t.0.join(format!("out-{name}"))).await;
        assert_eq!(pics_of(&o.output), want, "{name} noop");
        let o = convert_with(
            &src,
            &t.0.join(format!("out2-{name}")),
            OutputFormat::Original,
            &CoverEnricher(new.clone()),
        )
        .await;
        let mut want = vec![new.clone(), back.clone()];
        want.sort();
        assert_eq!(pics_of(&o.output), want, "{name} enriched");
    }
}

#[tokio::test]
async fn full_source_date_is_kept() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("tr-date");
    for (name, codec) in [("a.flac", "flac"), ("b.mp3", "libmp3lame")] {
        let src = t.0.join(name);
        gen(&ff, &src, &["-c:a", codec, "-metadata", "date=2021-08-19"]);
        let o = convert_noop(&src, &t.0.join(format!("out-{name}"))).await;
        assert_eq!(o.meta.year, Some(2021));
        let f = lofty::read_from_path(&o.output).unwrap();
        let d = f.primary_tag().unwrap().date().unwrap();
        assert_eq!(
            (d.year, d.month, d.day),
            (2021, Some(8), Some(19)),
            "{name}"
        );
    }
}

#[tokio::test]
async fn embedded_lyrics_survive_wav_to_mp3() {
    use lofty::prelude::ItemKey;
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("tr-lyr");
    let src = t.0.join("a.wav");
    gen(&ff, &src, &[]);
    let mut f = lofty::read_from_path(&src).unwrap();
    let mut tag = lofty::tag::Tag::new(lofty::tag::TagType::Id3v2);
    tag.insert_text(ItemKey::UnsyncLyrics, "第一句\n第二句".into());
    f.insert_tag(tag);
    f.save_to_path(&src, lofty::config::WriteOptions::default())
        .unwrap();
    let o = convert_with(&src, &t.0.join("out"), OutputFormat::Mp3, &NoopEnricher).await;
    assert_eq!(o.output.extension().unwrap(), "mp3");
    let f = lofty::read_from_path(&o.output).unwrap();
    assert_eq!(
        f.primary_tag().unwrap().get_string(ItemKey::UnsyncLyrics),
        Some("第一句\n第二句")
    );
}

#[test]
fn read_track_falls_back_to_secondary_tags() {
    use lofty::prelude::ItemKey;
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("tr-wavtags");
    let cover = png(&ff, &t);
    let src = t.0.join("a.wav");
    // RIFF INFO：标题、日期
    gen(
        &ff,
        &src,
        &["-metadata", "title=INFO 标题", "-metadata", "date=2020"],
    );
    // ID3v2（WAV 的主标签）：歌手、封面
    let mut f = lofty::read_from_path(&src).unwrap();
    assert!(f.primary_tag().is_none());
    let mut tag = lofty::tag::Tag::new(lofty::tag::TagType::Id3v2);
    tag.insert_text(ItemKey::TrackArtist, "甲/乙".into());
    f.insert_tag(tag);
    f.save_to_path(&src, lofty::config::WriteOptions::default())
        .unwrap();
    push_pics(&src, &[(lofty::picture::PictureType::CoverFront, &cover)]);
    let r = read_track(&src).unwrap();
    assert_eq!(r.title.as_deref(), Some("INFO 标题"));
    assert_eq!(r.year, Some(2020));
    assert_eq!(r.artists, ["甲", "乙"]);
    assert!(r.has_cover);
    assert_eq!(read_cover(&src).as_deref(), Some(&cover[..]));
}

#[test]
fn vorbis_without_nominal_bitrate_has_no_zero_quality() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("tr-vorbis");
    let src = t.0.join("a.ogg");
    // ffmpeg 自带的 vorbis 编码器不写标称码率
    gen(&ff, &src, &["-ac", "2", "-c:a", "vorbis", "-strict", "-2"]);
    let q = read_track(&src).unwrap().quality;
    assert!(q.starts_with("OGG"), "{q}");
    assert!(!q.contains(" 0k"), "{q}");
}
