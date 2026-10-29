//! Apple Music 兼容：`convert` 的 Apple 档，与已有歌的无损互转 `recode_lossless`。

use super::pipeline::{ffmpeg, gen};
use super::Tmp;
use crate::recode::TAMPER;
use crate::*;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::{Accessor, ItemKey};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const LRC: &str = "[00:00.50]第一句\n[00:01.20]第二句\n";

/// 指定颜色的 16×16 PNG。
fn png(ff: &Path, t: &Tmp, color: &str) -> Vec<u8> {
    let p = t.0.join(format!("{color}.png"));
    let st = Command::new(ff)
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("color={color}:s=16x16"))
        .args(["-frames:v", "1"])
        .arg(&p)
        .status()
        .unwrap();
    assert!(st.success());
    std::fs::read(p).unwrap()
}

/// 解码后 PCM 的哈希（素材用完整 ffmpeg 独立核对）。
fn pcm(ff: &Path, p: &Path) -> String {
    let o = Command::new(ff)
        .args(["-v", "error", "-i"])
        .arg(p)
        .args(["-map", "0:a:0", "-c:a", "pcm_s32le", "-f", "hash", "-"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", p.display());
    String::from_utf8(o.stdout).unwrap()
}

/// 写一组标签、两张图与歌词。
fn tag(p: &Path, front: &[u8], back: &[u8]) {
    let mut f = read_part(p);
    if f.primary_tag().is_none() {
        f.insert_tag(lofty::tag::Tag::new(f.primary_tag_type()));
    }
    let t = f.primary_tag_mut().unwrap();
    t.set_title("标题".into());
    t.insert_text(ItemKey::TrackArtist, "歌手甲".into());
    t.set_album("专辑".into());
    t.insert_text(ItemKey::AlbumArtist, "群星".into());
    t.insert_text(ItemKey::RecordingDate, "2021-08-19".into());
    t.set_track(3);
    t.set_track_total(12);
    t.set_disk(1);
    t.set_disk_total(2);
    t.insert_text(ItemKey::Genre, "流行".into());
    t.insert_text(ItemKey::Composer, "作曲者".into());
    let k = match t.tag_type() {
        lofty::tag::TagType::Id3v2 => ItemKey::UnsyncLyrics,
        _ => ItemKey::Lyrics,
    };
    assert!(t.insert_text(k, LRC.into()));
    for (ty, data) in [(PictureType::CoverFront, front), (PictureType::Other, back)] {
        t.push_picture(
            Picture::unchecked(data.to_vec())
                .pic_type(ty)
                .mime_type(MimeType::Png)
                .build(),
        );
    }
    f.save_to_path(p, lofty::config::WriteOptions::default())
        .unwrap();
}

struct Expect {
    ext: &'static str,
    alac: bool,
    bits: Option<u8>,
    rate: u32,
    quality: &'static str,
}

async fn apple(input: &Path, out: &Path, enricher: &dyn Enricher) -> ConvertOutcome {
    std::fs::create_dir_all(out).unwrap();
    let opts = ConvertOptions {
        output_dir: out.to_path_buf(),
        format: OutputFormat::Apple,
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

struct TransEnricher;
#[async_trait::async_trait]
impl Enricher for TransEnricher {
    async fn enrich(&self, meta: &mut TrackMeta) -> Result<(), String> {
        meta.lyrics = Some(Lyrics {
            lrc: "[00:00.50]一\n[00:01.20]二".into(),
            translated_lrc: Some("[00:00.50]one".into()),
        });
        Ok(())
    }
}

#[tokio::test]
async fn apple_output_for_every_source_kind() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-out");
    let (front, back) = (png(&ff, &t, "red"), png(&ff, &t, "blue"));
    let cases: [(&str, &[&str], Expect); 4] = [
        (
            "a.flac",
            &["-ar", "96000", "-sample_fmt", "s32", "-c:a", "flac"],
            Expect {
                ext: "m4a",
                alac: true,
                bits: Some(24),
                rate: 96000,
                quality: "ALAC 24bit/96kHz",
            },
        ),
        (
            "b.wav",
            &["-ar", "44100", "-c:a", "pcm_s16le"],
            Expect {
                ext: "m4a",
                alac: true,
                bits: Some(16),
                rate: 44100,
                quality: "ALAC 16bit/44.1kHz",
            },
        ),
        (
            "c.ogg",
            &["-ar", "48000", "-ac", "2", "-c:a", "libopus"],
            Expect {
                ext: "m4a",
                alac: false,
                bits: None,
                rate: 48000,
                quality: "AAC 256k",
            },
        ),
        (
            "d.mp3",
            &["-ar", "44100", "-c:a", "libmp3lame", "-b:a", "192k"],
            Expect {
                ext: "mp3",
                alac: false,
                bits: None,
                rate: 44100,
                quality: "MP3 192k",
            },
        ),
    ];
    for (name, args, e) in cases {
        let src = t.0.join(name);
        if e.quality.starts_with("AAC") {
            // 正弦波太简单，AAC 编码器用不满码率：换成噪声
            let st = Command::new(&ff)
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "anoisesrc=d=2:a=0.5",
                ])
                .args(args.iter())
                .arg(&src)
                .status()
                .unwrap();
            assert!(st.success());
        } else {
            gen(&ff, &src, args);
        }
        tag(&src, &front, &back);
        let out = t.0.join(format!("out-{name}"));
        let o = apple(&src, &out, &NoopEnricher).await;
        assert_eq!(o.output.extension().unwrap(), e.ext, "{name}");
        assert_eq!(o.quality, e.quality, "{name}");
        assert_eq!(is_alac(&o.output), e.alac, "{name}");
        assert_eq!(read_track(&o.output).unwrap().quality, e.quality, "{name}");
        if e.alac {
            assert_eq!(pcm(&ff, &src), pcm(&ff, &o.output), "{name}: 无损");
        }
        let f = read_part(&o.output);
        let pr = f.properties();
        assert_eq!(pr.sample_rate(), Some(e.rate), "{name}");
        if e.alac {
            assert_eq!(pr.bit_depth(), e.bits, "{name}");
        }
        let tg = f.primary_tag().unwrap();
        assert_eq!(tg.title().as_deref(), Some("标题"), "{name}");
        assert_eq!(tg.artist().as_deref(), Some("歌手甲"), "{name}");
        assert_eq!(tg.album().as_deref(), Some("专辑"), "{name}");
        assert_eq!(tg.get_string(ItemKey::AlbumArtist), Some("群星"), "{name}");
        assert_eq!(tg.date().map(|d| d.year), Some(2021), "{name}");
        assert_eq!((tg.track(), tg.disk()), (Some(3), Some(1)), "{name}");
        assert_eq!(tg.get_string(ItemKey::Composer), Some("作曲者"), "{name}");
        assert_eq!(read_lyrics(&o.output).as_deref(), Some(LRC), "{name}");
        let mut pics: Vec<&[u8]> = tg.pictures().iter().map(|p| p.data()).collect();
        pics.sort();
        let mut want = vec![&front[..], &back[..]];
        want.sort();
        assert_eq!(pics, want, "{name}");
    }
}

#[tokio::test]
async fn apple_embeds_merged_lyrics_and_writes_matching_lrc() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-lyr");
    let src = t.0.join("x.flac");
    gen(&ff, &src, &["-c:a", "flac", "-metadata", "title=歌"]);
    let out = t.0.join("out");
    let o = apple(&src, &out, &TransEnricher).await;
    let want = "[00:00.50]一\n[00:00.50]one\n[00:01.20]二\n";
    assert_eq!(read_lyrics(&o.output).as_deref(), Some(want));
    assert_eq!(o.lrc.as_deref(), Some(&*lrc_home(&out, &o.output)));
    assert!(o.lrc.as_ref().unwrap().ends_with("lrc/歌.lrc"));
    assert_eq!(std::fs::read_to_string(o.lrc.unwrap()).unwrap(), want);
}

// ---------- recode_lossless ----------

fn born(p: &Path) -> std::time::SystemTime {
    std::fs::metadata(p).unwrap().created().unwrap()
}

/// 把创建时间设成很早以前，好看出有没有保留。
fn age(p: &Path) {
    use std::os::macos::fs::FileTimesExt;
    let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_500_000_000);
    std::fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_created(old))
        .unwrap();
}

fn xattr(p: &Path, write: Option<&str>) -> Option<String> {
    let mut c = Command::new("/usr/bin/xattr");
    match write {
        Some(v) => c.args(["-w", "com.example.musicreveal-test", v]),
        None => c.args(["-p", "com.example.musicreveal-test"]),
    };
    let o = c.arg(p).output().unwrap();
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_owned())
}

fn snapshot(p: &Path) -> (Option<String>, Vec<String>, Vec<Vec<u8>>, Option<String>) {
    let f = lofty::read_from_path(p).unwrap();
    let tg = f.primary_tag().unwrap();
    let mut keys: Vec<String> = [
        tg.title().map(|s| s.to_string()),
        tg.artist().map(|s| s.to_string()),
        tg.album().map(|s| s.to_string()),
        tg.get_string(ItemKey::AlbumArtist).map(str::to_owned),
        tg.get_string(ItemKey::RecordingDate).map(str::to_owned),
        tg.get_string(ItemKey::Genre).map(str::to_owned),
        tg.get_string(ItemKey::Composer).map(str::to_owned),
        tg.track().map(|n| n.to_string()),
        tg.track_total().map(|n| n.to_string()),
        tg.disk().map(|n| n.to_string()),
        tg.disk_total().map(|n| n.to_string()),
    ]
    .into_iter()
    .map(|v| v.unwrap_or_default())
    .collect();
    keys.push(format!("{:?}", f.properties().bit_depth()));
    keys.push(format!("{:?}", f.properties().sample_rate()));
    let pics = tg.pictures().iter().map(|p| p.data().to_vec()).collect();
    (
        tg.title().map(|s| s.to_string()),
        keys,
        pics,
        read_lyrics(p),
    )
}

/// 按内容认格式（隐藏副本是 `.part`，Opus 的扩展名是 .ogg）。
fn read_part(p: &Path) -> lofty::file::TaggedFile {
    lofty::probe::Probe::open(p)
        .unwrap()
        .guess_file_type()
        .unwrap()
        .read()
        .unwrap()
}

fn no_parts(dir: &Path) {
    let left: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(PART_PREFIX))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

fn sample(ff: &Path, t: &Tmp, name: &str, args: &[&str]) -> PathBuf {
    let p = t.0.join(name);
    gen(ff, &p, args);
    tag(&p, &png(ff, t, "red"), &png(ff, t, "blue"));
    age(&p);
    p
}

#[test]
fn recode_round_trips_flac_and_wav_losslessly() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-rt");
    let cases: [(&str, &[&str]); 3] = [
        ("a16.flac", &["-ar", "44100", "-c:a", "flac"]),
        (
            "b24.flac",
            &["-ar", "96000", "-sample_fmt", "s32", "-c:a", "flac"],
        ),
        ("c.wav", &["-ar", "48000", "-c:a", "pcm_s24le"]),
    ];
    let tok = CancelToken::default();
    for (name, args) in cases {
        let src = sample(&ff, &t, name, args);
        xattr(&src, Some("红色"));
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o640)).unwrap();
        let (hash, snap, b) = (pcm(&ff, &src), snapshot(&src), born(&src));

        let m4a =
            recode_lossless(&src, Lossless::Alac, &tok).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(m4a, src.with_extension("m4a"), "{name}");
        assert!(!src.exists(), "{name}: 原文件应删除");
        assert!(is_alac(&m4a), "{name}");
        assert_eq!(pcm(&ff, &m4a), hash, "{name}");
        assert_eq!(snapshot(&m4a), snap, "{name}");
        assert_eq!(born(&m4a), b, "{name}: 创建时间");
        assert_eq!(
            xattr(&m4a, None).as_deref(),
            Some("红色"),
            "{name}: 扩展属性"
        );
        let mode = std::fs::metadata(&m4a).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "{name}: 权限");

        let flac =
            recode_lossless(&m4a, Lossless::Flac, &tok).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(flac, src.with_extension("flac"), "{name}");
        assert!(!m4a.exists(), "{name}");
        assert_eq!(pcm(&ff, &flac), hash, "{name}");
        assert_eq!(snapshot(&flac), snap, "{name}");
        assert_eq!(born(&flac), b, "{name}");
        assert_eq!(
            read_track(&flac).unwrap().quality.split(' ').next(),
            Some("FLAC")
        );
        if name.ends_with(".flac") {
            assert_eq!(read_cover(&flac), snap.2.first().cloned(), "{name}");
        }
    }
    no_parts(&t.0);
}

#[test]
fn recode_refuses_and_leaves_the_original_untouched() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-refuse");
    let tok = CancelToken::default();
    let src = sample(&ff, &t, "a.flac", &["-c:a", "flac"]);
    let bytes = std::fs::read(&src).unwrap();
    let unchanged = |why: &str| {
        assert!(std::fs::read(&src).unwrap() == bytes, "{why}");
        no_parts(&t.0);
    };

    // 目标同名文件已存在
    std::fs::write(src.with_extension("m4a"), b"keep").unwrap();
    let e = recode_lossless(&src, Lossless::Alac, &tok).unwrap_err();
    assert_eq!(e.to_string(), "已有同名的 .m4a 文件，未覆盖");
    assert_eq!(std::fs::read(src.with_extension("m4a")).unwrap(), b"keep");
    unchanged("目标已存在");
    std::fs::remove_file(src.with_extension("m4a")).unwrap();

    // 比对不一致：副本被改动
    TAMPER.set(Some(|p: &Path| {
        let mut f = read_part(p);
        f.primary_tag_mut().unwrap().set_title("被改".into());
        f.save_to_path(p, lofty::config::WriteOptions::default())
            .unwrap();
    }));
    let e = recode_lossless(&src, Lossless::Alac, &tok).unwrap_err();
    TAMPER.set(None);
    assert_eq!(e.to_string(), "转换后标签不一致，未替换");
    assert!(!src.with_extension("m4a").exists());
    unchanged("标签不一致");

    TAMPER.set(Some(|p: &Path| {
        let mut f = read_part(p);
        f.primary_tag_mut().unwrap().remove_picture(0);
        f.save_to_path(p, lofty::config::WriteOptions::default())
            .unwrap();
    }));
    let e = recode_lossless(&src, Lossless::Alac, &tok).unwrap_err();
    TAMPER.set(None);
    assert_eq!(e.to_string(), "转换后封面不一致，未替换");
    unchanged("封面不一致");

    // 方向不对、只读、符号链接
    let e = recode_lossless(&src, Lossless::Flac, &tok).unwrap_err();
    assert_eq!(e.to_string(), "不是 ALAC，未转换");
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o444)).unwrap();
    let e = recode_lossless(&src, Lossless::Alac, &tok).unwrap_err();
    assert_eq!(e.to_string(), "文件是只读的，未转换");
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o644)).unwrap();
    let link = t.0.join("link.flac");
    std::os::unix::fs::symlink(&src, &link).unwrap();
    let e = recode_lossless(&link, Lossless::Alac, &tok).unwrap_err();
    assert_eq!(e.to_string(), "是符号链接或不是普通文件，未转换");
    unchanged("拒绝");

    // AAC 不是 ALAC，不转回
    let aac = t.0.join("b.m4a");
    gen(&ff, &aac, &["-c:a", "aac", "-f", "ipod"]);
    let e = recode_lossless(&aac, Lossless::Flac, &tok).unwrap_err();
    assert_eq!(e.to_string(), "不是 ALAC，未转换");
    assert!(!is_alac(&aac));
}

/// ALAC 装不下的无损（32 bit 整数、32 bit 浮点）不转 ALAC：照原格式档保持 WAV，PCM 不变。
#[tokio::test]
async fn apple_keeps_wav_that_alac_cannot_hold_losslessly() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-32");
    for c in ["pcm_s32le", "pcm_f32le"] {
        let src = t.0.join(format!("{c}.wav"));
        gen(&ff, &src, &["-ar", "48000", "-c:a", c]);
        let out = t.0.join(format!("out-{c}"));
        let o = apple(&src, &out, &NoopEnricher).await;
        assert_eq!(o.output.extension().unwrap(), "wav", "{c}");
        assert!(!is_alac(&o.output), "{c}");
        let hash = |p: &Path| {
            let o = Command::new(&ff)
                .args(["-v", "error", "-i"])
                .arg(p)
                .args(["-map", "0:a:0", "-c:a", "copy", "-f", "hash", "-"])
                .output()
                .unwrap();
            String::from_utf8(o.stdout).unwrap()
        };
        assert_eq!(hash(&src), hash(&o.output), "{c}: 采样原样");
    }
}

/// 位深不是 16 / 24 的 FLAC、WAV 不在互转之列：`recodable` 与 `recode_lossless` 一致。
#[test]
fn recodable_matches_what_recode_would_accept() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-recodable");
    let tok = CancelToken::default();
    let cases: [(&str, &[&str], bool); 4] = [
        ("a16.flac", &["-c:a", "flac"], true),
        ("b24.wav", &["-c:a", "pcm_s24le"], true),
        ("c32.wav", &["-c:a", "pcm_s32le"], false),
        ("d.wav", &["-c:a", "pcm_f32le"], false),
    ];
    for (name, args, ok) in cases {
        let p = t.0.join(name);
        gen(&ff, &p, args);
        assert_eq!(recodable(&p, Lossless::Alac), ok, "{name}");
        assert!(!recodable(&p, Lossless::Flac), "{name}");
        let r = recode_lossless(&p, Lossless::Alac, &tok);
        assert_eq!(r.is_ok(), ok, "{name}: {r:?}");
        if let Ok(m4a) = r {
            assert!(recodable(&m4a, Lossless::Flac), "{name}");
            assert!(!recodable(&m4a, Lossless::Alac), "{name}");
        }
    }
    let ro = t.0.join("ro.flac");
    gen(&ff, &ro, &["-c:a", "flac"]);
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o444)).unwrap();
    assert!(!recodable(&ro, Lossless::Alac), "只读");
}

// ---------- recode_lossless：标签按独立读取（完整 ffmpeg）核对 ----------

/// 完整 ffmpeg 读到的全局元数据（键小写），去掉容器自带的几项与 encoder；值按出现顺序。
fn ff_meta(ff: &Path, p: &Path) -> Vec<(String, String)> {
    let o = Command::new(ff)
        .args(["-v", "error", "-i"])
        .arg(p)
        .args(["-f", "ffmetadata", "-"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", p.display());
    let text = String::from_utf8(o.stdout).unwrap();
    let mut out: Vec<(String, String)> = vec![];
    let mut cont = false;
    for l in text.lines().skip(1) {
        if cont {
            let last = out.last_mut().unwrap();
            last.1.push('\n');
            last.1 += l.trim_end_matches('\\');
            cont = l.ends_with('\\');
            continue;
        }
        if l.starts_with('[') {
            break;
        }
        let (k, v) = l.split_once('=').unwrap();
        cont = v.ends_with('\\');
        out.push((
            k.to_lowercase(),
            v.trim_end_matches('\\').replace("\\=", "="),
        ));
    }
    out.retain(|(k, _)| {
        ![
            "major_brand",
            "minor_version",
            "compatible_brands",
            "encoder",
        ]
        .contains(&k.as_str())
    });
    out
}

fn has(meta: &[(String, String)], k: &str, v: &str) -> bool {
    meta.iter().any(|(a, b)| a == k && b == v)
}

/// 审查实验一：完整 ffmpeg 以 mdta 键（`use_metadata_tags`）写标签的 ALAC，转 FLAC 后一项不少。
#[test]
fn mdta_tagged_alac_keeps_every_tag_when_recoded_to_flac() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-mdta");
    let src = t.0.join("m.m4a");
    gen(
        &ff,
        &src,
        &[
            "-c:a",
            "alac",
            "-movflags",
            "use_metadata_tags",
            "-metadata",
            "title=标题",
            "-metadata",
            "artist=歌手",
            "-metadata",
            "album=专辑",
            "-metadata",
            "date=2020",
            "-metadata",
            "track=3/10",
            "-metadata",
            "disc=1",
            "-metadata",
            "lyrics=[00:01.00]一",
            "-f",
            "ipod",
        ],
    );
    assert!(is_alac(&src));
    let before = ff_meta(&ff, &src);
    assert!(has(&before, "title", "标题"), "{before:?}");
    let flac = recode_lossless(&src, Lossless::Flac, &CancelToken::default()).unwrap();
    let m = ff_meta(&ff, &flac);
    for (k, v) in [
        ("title", "标题"),
        ("artist", "歌手"),
        ("album", "专辑"),
        ("date", "2020"),
        ("track", "3"),
        ("tracktotal", "10"),
        ("disc", "1"),
        ("lyrics", "[00:01.00]一"),
    ] {
        assert!(has(&m, k, v), "{k}={v} 丢了：{m:?}");
    }
    assert_eq!(read_lyrics(&flac).as_deref(), Some("[00:01.00]一"));
    no_parts(&t.0);
}

/// 审查实验二：FLAC 里的自定义 Vorbis 注释转 ALAC、再转回都还在。
#[test]
fn custom_vorbis_comment_survives_both_ways() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-custom");
    let src = sample(&ff, &t, "a.flac", &["-c:a", "flac"]);
    {
        use lofty::config::WriteOptions;
        use lofty::flac::FlacFile;
        use lofty::prelude::*;
        let mut f = FlacFile::read_from(
            &mut std::fs::File::open(&src).unwrap(),
            lofty::config::ParseOptions::new(),
        )
        .unwrap();
        let vc = f.vorbis_comments_mut().unwrap();
        vc.push("REVIEW_CUSTOM".into(), "自定义值".into());
        vc.push("ENCODER".into(), "某编码器 1.0".into());
        f.save_to_path(&src, WriteOptions::default()).unwrap();
    }
    let want = ff_meta(&ff, &src);
    assert!(has(&want, "review_custom", "自定义值"), "{want:?}");
    let tok = CancelToken::default();
    let m4a = recode_lossless(&src, Lossless::Alac, &tok).unwrap();
    assert!(has(&ff_meta(&ff, &m4a), "review_custom", "自定义值"));
    let flac = recode_lossless(&m4a, Lossless::Flac, &tok).unwrap();
    let back = ff_meta(&ff, &flac);
    assert!(has(&back, "review_custom", "自定义值"), "{back:?}");
}

/// 转不过去的标签（MP4 里 FLAC 表达不了的原子）：不转，原文件不变。
#[test]
fn unconvertible_mp4_atom_is_refused_and_the_original_kept() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-atom");
    let src = sample(&ff, &t, "a.flac", &["-c:a", "flac"]);
    let tok = CancelToken::default();
    let m4a = recode_lossless(&src, Lossless::Alac, &tok).unwrap();
    {
        use lofty::config::WriteOptions;
        use lofty::mp4::{Atom, AtomData, AtomIdent, Mp4File};
        use lofty::prelude::*;
        let mut f = Mp4File::read_from(
            &mut std::fs::File::open(&m4a).unwrap(),
            lofty::config::ParseOptions::new(),
        )
        .unwrap();
        f.ilst_mut().unwrap().insert(Atom::new(
            AtomIdent::Fourcc(*b"zzzz"),
            AtomData::UTF8("x".into()),
        ));
        f.save_to_path(&m4a, WriteOptions::default()).unwrap();
    }
    let bytes = std::fs::read(&m4a).unwrap();
    let e = recode_lossless(&m4a, Lossless::Flac, &tok).unwrap_err();
    assert!(e.to_string().contains("zzzz"), "{e}");
    assert_eq!(std::fs::read(&m4a).unwrap(), bytes);
    assert!(!src.exists());
    no_parts(&t.0);
}

/// 歌词只要一侧独立读到而另一侧没有或不同就不替换（副本里多出一份歌词）。
#[test]
fn extra_lyrics_on_one_side_fail_the_check() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-lyr2");
    let src = sample(&ff, &t, "a.flac", &["-c:a", "flac"]);
    let m4a = recode_lossless(&src, Lossless::Alac, &CancelToken::default()).unwrap();
    let bytes = std::fs::read(&m4a).unwrap();
    TAMPER.set(Some(|p: &Path| {
        use lofty::config::WriteOptions;
        use lofty::flac::FlacFile;
        use lofty::prelude::*;
        let mut f =
            FlacFile::read_from(&mut std::fs::File::open(p).unwrap(), Default::default()).unwrap();
        let vc = f.vorbis_comments_mut().unwrap();
        vc.push("UNSYNCEDLYRICS".into(), "多出来的".into());
        f.save_to_path(p, WriteOptions::default()).unwrap();
    }));
    let e = recode_lossless(&m4a, Lossless::Flac, &CancelToken::default()).unwrap_err();
    TAMPER.set(None);
    assert!(e.to_string().contains("不一致"), "{e}");
    assert_eq!(std::fs::read(&m4a).unwrap(), bytes);
    no_parts(&t.0);
}

struct OwnEnricher(Vec<u8>);
#[async_trait::async_trait]
impl Enricher for OwnEnricher {
    async fn enrich(&self, meta: &mut TrackMeta) -> Result<(), String> {
        meta.title = Some("歌名".into());
        meta.artists = vec!["甲".into(), "乙".into()];
        meta.album = Some("专辑".into());
        meta.album_artist = Some("甲".into());
        meta.year = Some(2019);
        meta.track = Some(4);
        meta.disc = Some(1);
        meta.cover = Some(self.0.clone());
        meta.lyrics = Some(Lyrics {
            lrc: "[00:00.50]一\n[00:01.20]二".into(),
            translated_lrc: Some("[00:00.50]one".into()),
        });
        Ok(())
    }
}

/// 我们自己产出的那类 FLAC（原格式档补全后的标题、艺人、专辑、年份、曲序、碟号、歌词、封面、
/// ENCODER 等）转 ALAC 再转回：每次都成功，独立读到的标签一项不变。
#[tokio::test]
async fn our_own_flac_output_recodes_both_ways() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-own");
    let src = t.0.join("src.flac");
    gen(
        &ff,
        &src,
        &[
            "-ar",
            "44100",
            "-c:a",
            "flac",
            "-metadata",
            "comment=163 key(Don't modify):abc",
        ],
    );
    let out = t.0.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let opts = ConvertOptions {
        output_dir: out.clone(),
        format: OutputFormat::Original,
        ffmpeg: find_ffmpeg(),
    };
    let o = convert(
        &src,
        &opts,
        &OwnEnricher(png(&ff, &t, "red")),
        None,
        &CancelToken::default(),
        &mut |_| {},
    )
    .await
    .unwrap();
    let flac = o.output;
    assert_eq!(flac.extension().unwrap(), "flac");
    let want = ff_meta(&ff, &flac);
    assert!(has(&want, "title", "歌名"), "{want:?}");
    let tok = CancelToken::default();
    let m4a = recode_lossless(&flac, Lossless::Alac, &tok).unwrap();
    let back = recode_lossless(&m4a, Lossless::Flac, &tok).unwrap();
    let mut a = want.clone();
    let mut b = ff_meta(&ff, &back);
    a.sort();
    b.sort();
    assert_eq!(a, b);
}

/// 审查插桩实验：新文件换上后、删原文件前退出，两份并存。再转一次：新文件与原文件完全一致，
/// 补上属性、删掉原文件，算成功；不一致的同名文件（用户自己的另一版）照旧不动、报同名。
#[test]
fn rerun_after_halt_between_place_and_delete_finishes_the_job() {
    use crate::recode::HALT_AFTER_PLACE;
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-halt");
    let tok = CancelToken::default();
    let src = sample(&ff, &t, "a.flac", &["-c:a", "flac"]);
    xattr(&src, Some("蓝色"));
    let (hash, snap, b) = (pcm(&ff, &src), snapshot(&src), born(&src));
    HALT_AFTER_PLACE.set(true);
    let e = recode_lossless(&src, Lossless::Alac, &tok).unwrap_err();
    HALT_AFTER_PLACE.set(false);
    assert_eq!(e.to_string(), "测试：换上新文件后中断");
    let m4a = src.with_extension("m4a");
    assert!(src.exists() && m4a.exists());

    assert_eq!(recode_lossless(&src, Lossless::Alac, &tok).unwrap(), m4a);
    assert!(!src.exists());
    assert_eq!(pcm(&ff, &m4a), hash);
    assert_eq!(snapshot(&m4a), snap);
    assert_eq!(born(&m4a), b);
    assert_eq!(xattr(&m4a, None).as_deref(), Some("蓝色"));
    no_parts(&t.0);

    // 同名的 FLAC 是另一版（标题不同）：不删、报同名
    let other = sample(&ff, &t, "a.flac", &["-c:a", "flac"]);
    {
        let mut f = read_part(&other);
        f.primary_tag_mut().unwrap().set_title("另一版".into());
        f.save_to_path(&other, lofty::config::WriteOptions::default())
            .unwrap();
    }
    let (fb, mb) = (std::fs::read(&other).unwrap(), std::fs::read(&m4a).unwrap());
    let e = recode_lossless(&m4a, Lossless::Flac, &tok).unwrap_err();
    assert_eq!(e.to_string(), "已有同名的 .flac 文件，未覆盖");
    assert_eq!(std::fs::read(&other).unwrap(), fb);
    assert_eq!(std::fs::read(&m4a).unwrap(), mb);
    no_parts(&t.0);
}
