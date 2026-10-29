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

/// 写一组标签、两张图（`back` 为空则只有正面封面）与歌词。
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
    let pics = [(PictureType::CoverFront, front), (PictureType::Other, back)];
    for (ty, data) in pics.into_iter().filter(|p| !p.1.is_empty()) {
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
    tag(&p, &png(ff, t, "red"), &[]);
    if name.ends_with(".wav") {
        // WAV 的歌词是带语言的 USLT，不在互转白名单里
        let mut f = read_part(&p);
        f.primary_tag_mut()
            .unwrap()
            .remove_key(ItemKey::UnsyncLyrics);
        f.save_to_path(&p, lofty::config::WriteOptions::default())
            .unwrap();
    }
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
    let cases: [(&str, &[&str], bool); 7] = [
        ("a16.flac", &["-c:a", "flac"], true),
        ("b24.wav", &["-c:a", "pcm_s24le"], true),
        ("c32.wav", &["-c:a", "pcm_s32le"], false),
        ("d.wav", &["-c:a", "pcm_f32le"], false),
        ("e1.flac", &["-ac", "1", "-c:a", "flac"], true),
        ("f3.flac", &["-ac", "3", "-c:a", "flac"], false),
        ("g8.wav", &["-ac", "8", "-c:a", "pcm_s16le"], false),
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

/// 审查实验一：完整 ffmpeg 以 mdta 键（`use_metadata_tags`）写标签的 ALAC：元数据结构不在白名单里
/// （moov 下的 mdta meta 与 keys），宁可不转。
#[test]
fn mdta_tagged_alac_is_refused() {
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
            "lyrics=[00:01.00]一",
            "-f",
            "ipod",
        ],
    );
    assert!(is_alac(&src));
    assert!(has(&ff_meta(&ff, &src), "title", "标题"));
    refused(&t, &src, Lossless::Flac, "mdta");
}

/// 审查实验二：FLAC 里的自定义 Vorbis 注释：白名单外的键一律不转。
#[test]
fn custom_vorbis_comment_is_refused() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-custom");
    let src = sample(&ff, &t, "a.flac", &["-c:a", "flac"]);
    with_vc(&src, |vc| {
        vc.push("REVIEW_CUSTOM".into(), "自定义值".into())
    });
    refused(&t, &src, Lossless::Alac, "REVIEW_CUSTOM");
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

// ---------- 复审：元数据结构白名单，宁可不转 ----------

/// 拒转：`recodable` 为假，`recode_lossless` 报含 `why` 的原因，源文件逐字节不变（等价于 SHA-256
/// 不变），没有目标文件、没有隐藏副本。
fn refused(t: &Tmp, src: &Path, to: Lossless, why: &str) {
    let bytes = std::fs::read(src).unwrap();
    assert!(!recodable(src, to), "{why}: recodable 应为假");
    let e = recode_lossless(src, to, &CancelToken::default()).unwrap_err();
    assert!(e.to_string().contains(why), "{why}: {e}");
    assert!(e.to_string().ends_with("未转换"), "{e}");
    assert!(std::fs::read(src).unwrap() == bytes, "{why}: 源文件变了");
    let ext = if to == Lossless::Alac { "m4a" } else { "flac" };
    assert!(!src.with_extension(ext).exists(), "{why}");
    no_parts(&t.0);
}

/// 在 STREAMINFO 后插入一个 FLAC 元数据块。
fn add_flac_block(p: &Path, ty: u8, payload: &[u8]) {
    let mut b = std::fs::read(p).unwrap();
    assert_eq!(&b[..4], b"fLaC");
    let len = u32::from_be_bytes([0, b[5], b[6], b[7]]) as usize;
    let last = b[4] & 0x80;
    b[4] &= 0x7f;
    let mut blk = vec![ty | last];
    blk.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]);
    blk.extend_from_slice(payload);
    b.splice(8 + len..8 + len, blk);
    std::fs::write(p, b).unwrap();
}

/// 逐字节往 VORBIS_COMMENT 块里追加注释（不经 lofty：它会丢掉空值、合并大小写）。
fn add_vc_raw(p: &Path, entries: &[&str]) {
    let mut b = std::fs::read(p).unwrap();
    let mut i = 4;
    loop {
        let len = u32::from_be_bytes([0, b[i + 1], b[i + 2], b[i + 3]]) as usize;
        if b[i] & 0x7f == 4 {
            let body = i + 4;
            let vendor = u32::from_le_bytes(b[body..body + 4].try_into().unwrap()) as usize;
            let at = body + 4 + vendor;
            let n = u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
            b[at..at + 4].copy_from_slice(&(n + entries.len() as u32).to_le_bytes());
            let mut add = vec![];
            for e in entries {
                add.extend((e.len() as u32).to_le_bytes());
                add.extend(e.as_bytes());
            }
            let new = (len + add.len()) as u32;
            b[i + 1..i + 4].copy_from_slice(&new.to_be_bytes()[1..]);
            b.splice(body + len..body + len, add);
            break;
        }
        assert_eq!(b[i] & 0x80, 0, "没有 VORBIS_COMMENT 块");
        i += 4 + len;
    }
    std::fs::write(p, b).unwrap();
}

fn with_vc(p: &Path, f: impl FnOnce(&mut lofty::ogg::tag::VorbisComments)) {
    use lofty::flac::FlacFile;
    let mut file =
        FlacFile::read_from(&mut std::fs::File::open(p).unwrap(), Default::default()).unwrap();
    f(file.vorbis_comments_mut().unwrap());
    file.save_to_path(p, lofty::config::WriteOptions::default())
        .unwrap();
}

/// 复审实验：同一键多值、大小写不同的同名键。
#[test]
fn review_multi_value_and_case_collision_are_refused() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-dup");
    let a = sample(&ff, &t, "a.flac", &["-c:a", "flac"]);
    with_vc(&a, |vc| vc.push("ARTIST".into(), "第二位".into()));
    refused(&t, &a, Lossless::Alac, "ARTIST 出现多次");
    let b = sample(&ff, &t, "b.flac", &["-c:a", "flac"]);
    add_vc_raw(&b, &["genre=摇滚"]);
    refused(&t, &b, Lossless::Alac, "GENRE 出现多次");
}

/// 复审实验：MP4 里两个同名自由格式原子，mean 不同。
#[test]
fn review_mp4_same_freeform_name_with_different_mean_is_refused() {
    use lofty::mp4::{Atom, AtomData, AtomIdent, Mp4File};
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-mean");
    let src = sample(&ff, &t, "a.flac", &["-c:a", "flac"]);
    let m4a = recode_lossless(&src, Lossless::Alac, &CancelToken::default()).unwrap();
    let mut f =
        Mp4File::read_from(&mut std::fs::File::open(&m4a).unwrap(), Default::default()).unwrap();
    let ilst = f.ilst_mut().unwrap();
    for (mean, v) in [("com.apple.iTunes", "一"), ("org.example", "二")] {
        ilst.insert(Atom::new(
            AtomIdent::Freeform {
                mean: mean.into(),
                name: "REVIEW".into(),
            },
            AtomData::UTF8(v.into()),
        ));
    }
    f.save_to_path(&m4a, lofty::config::WriteOptions::default())
        .unwrap();
    refused(&t, &m4a, Lossless::Flac, "REVIEW 自由格式原子");
}

/// 复审实验：FLAC 里的 APPLICATION 块。
#[test]
fn review_flac_application_block_is_refused() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-app");
    let src = sample(&ff, &t, "a.flac", &["-c:a", "flac"]);
    add_flac_block(&src, 2, b"riffsome-app-data");
    refused(&t, &src, Lossless::Alac, "APPLICATION");
}

/// 一条只有导出音轨（170 号）的 CD 格式 CUESHEET。
fn cuesheet() -> Vec<u8> {
    let mut c = vec![0u8; 128]; // 目录号
    c.extend(88200u64.to_be_bytes()); // lead-in
    c.push(0x80); // 是 CD
    c.extend([0u8; 258]);
    c.push(1); // 音轨数
    c.extend(88200u64.to_be_bytes());
    c.push(170);
    c.extend([0u8; 12]); // ISRC
    c.push(0);
    c.extend([0u8; 13]);
    c.push(0); // 索引点数
    c
}

/// 复审实验：FLAC 里的 CUESHEET 块。
#[test]
fn review_flac_cuesheet_block_is_refused() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-cue");
    let src = sample(&ff, &t, "a.flac", &["-c:a", "flac"]);
    add_flac_block(&src, 5, &cuesheet());
    refused(&t, &src, Lossless::Alac, "CUESHEET");
}

/// 复审实验：正面、背面两张带描述的 PICTURE。
#[test]
fn review_two_described_pictures_are_refused() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-pics");
    let src = t.0.join("a.flac");
    gen(&ff, &src, &["-c:a", "flac"]);
    let mut f = read_part(&src);
    f.insert_tag(lofty::tag::Tag::new(f.primary_tag_type()));
    let tg = f.primary_tag_mut().unwrap();
    for (ty, c) in [
        (PictureType::CoverFront, "red"),
        (PictureType::CoverBack, "blue"),
    ] {
        tg.push_picture(
            Picture::unchecked(png(&ff, &t, c))
                .pic_type(ty)
                .mime_type(MimeType::Png)
                .description(format!("{c} 封面"))
                .build(),
        );
    }
    f.save_to_path(&src, lofty::config::WriteOptions::default())
        .unwrap();
    refused(&t, &src, Lossless::Alac, "图片");
    // 只一张正面封面但带描述：MP4 的 covr 存不下描述，也拒
    let one = t.0.join("b.flac");
    gen(&ff, &one, &["-c:a", "flac"]);
    let mut f = read_part(&one);
    f.insert_tag(lofty::tag::Tag::new(f.primary_tag_type()));
    f.primary_tag_mut().unwrap().push_picture(
        Picture::unchecked(png(&ff, &t, "red"))
            .pic_type(PictureType::CoverFront)
            .mime_type(MimeType::Png)
            .description("描述".to_string())
            .build(),
    );
    f.save_to_path(&one, lofty::config::WriteOptions::default())
        .unwrap();
    refused(&t, &one, Lossless::Alac, "图片");
}

/// 复审实验：WAV 的 ID3 里有 GEOB 帧（ffmpeg 读不到，核对不了）。
#[test]
fn review_wav_geob_frame_is_refused() {
    use lofty::id3::v2::{BinaryFrame, Frame, FrameId};
    use lofty::iff::wav::WavFile;
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-geob");
    let src = sample(&ff, &t, "a.wav", &["-c:a", "pcm_s16le"]);
    let mut f =
        WavFile::read_from(&mut std::fs::File::open(&src).unwrap(), Default::default()).unwrap();
    let mut geob = vec![0u8];
    geob.extend(b"application/octet-stream\0a.bin\0d\0payload");
    f.id3v2_mut()
        .unwrap()
        .insert(Frame::Binary(BinaryFrame::new(
            FrameId::Valid("GEOB".into()),
            geob,
        )));
    f.save_to_path(&src, lofty::config::WriteOptions::default())
        .unwrap();
    refused(&t, &src, Lossless::Alac, "GEOB");
}

/// 复审实验（续跑）：上次换上 .m4a 后中断，之后源 FLAC 多了 APPLICATION 块（PCM、标签都没变）。
/// 源已不在白名单里：不删源，两边不动。
#[test]
fn review_rerun_with_application_block_keeps_both() {
    use crate::recode::HALT_AFTER_PLACE;
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-halt-app");
    let tok = CancelToken::default();
    let src = sample(&ff, &t, "a.flac", &["-c:a", "flac"]);
    HALT_AFTER_PLACE.set(true);
    let _ = recode_lossless(&src, Lossless::Alac, &tok).unwrap_err();
    HALT_AFTER_PLACE.set(false);
    let m4a = src.with_extension("m4a");
    add_flac_block(&src, 2, b"riffsome-app-data");
    let (sb, mb) = (std::fs::read(&src).unwrap(), std::fs::read(&m4a).unwrap());
    assert!(!recodable(&src, Lossless::Alac));
    assert!(recode_lossless(&src, Lossless::Alac, &tok).is_err());
    assert!(std::fs::read(&src).unwrap() == sb && std::fs::read(&m4a).unwrap() == mb);
    no_parts(&t.0);
}

/// 复审实验（硬链接）：续跑时目标 .m4a 另有一个硬链接名，补属性会改到那个名字上：拒，两边不动。
#[test]
fn review_rerun_refuses_hard_linked_target() {
    use crate::recode::HALT_AFTER_PLACE;
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-hardlink");
    let tok = CancelToken::default();
    let src = sample(&ff, &t, "a.flac", &["-c:a", "flac"]);
    xattr(&src, Some("源"));
    HALT_AFTER_PLACE.set(true);
    let _ = recode_lossless(&src, Lossless::Alac, &tok).unwrap_err();
    HALT_AFTER_PLACE.set(false);
    let m4a = src.with_extension("m4a");
    let alias = t.0.join("alias.m4a");
    std::fs::hard_link(&m4a, &alias).unwrap();
    xattr(&alias, Some("别名"));
    age(&alias);
    std::fs::set_permissions(&alias, std::fs::Permissions::from_mode(0o600)).unwrap();
    let before = (
        born(&alias),
        xattr(&alias, None),
        std::fs::read(&alias).unwrap(),
    );
    let sb = std::fs::read(&src).unwrap();
    let e = recode_lossless(&src, Lossless::Alac, &tok).unwrap_err();
    assert_eq!(e.to_string(), "已有同名的 .m4a 文件，未覆盖");
    assert_eq!(
        (
            born(&alias),
            xattr(&alias, None),
            std::fs::read(&alias).unwrap()
        ),
        before
    );
    let mode = std::fs::metadata(&alias).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert!(std::fs::read(&src).unwrap() == sb);
    no_parts(&t.0);
}

/// 约 100KB 的内嵌歌词：读标签的 ffmpeg 输出超过管道缓冲也不卡住，双向都成功。
#[test]
fn hundred_kb_lyrics_recode_both_ways() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-biglyr");
    let src = sample(&ff, &t, "a.flac", &["-c:a", "flac"]);
    let big: String = (0..2500)
        .map(|i| {
            format!(
                "[{:02}:{:02}.00]第 {i} 句歌词，很长很长\n",
                i / 60 % 60,
                i % 60
            )
        })
        .collect();
    assert!(big.len() > 100_000, "{}", big.len());
    with_vc(&src, |vc| vc.insert("LYRICS".into(), big.clone()));
    let tok = CancelToken::default();
    let m4a = recode_lossless(&src, Lossless::Alac, &tok).unwrap();
    assert_eq!(read_lyrics(&m4a).as_deref(), Some(&*big));
    let flac = recode_lossless(&m4a, Lossless::Flac, &tok).unwrap();
    assert_eq!(read_lyrics(&flac).as_deref(), Some(&*big));
}

/// 值带首尾空白也要逐字一致：副本里标题多了个空格就不替换。
#[test]
fn trailing_space_in_a_value_fails_the_check() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-space");
    let src = sample(&ff, &t, "a.flac", &["-c:a", "flac"]);
    let bytes = std::fs::read(&src).unwrap();
    TAMPER.set(Some(|p: &Path| {
        let mut f = read_part(p);
        f.primary_tag_mut().unwrap().set_title("标题 ".into());
        f.save_to_path(p, lofty::config::WriteOptions::default())
            .unwrap();
    }));
    let e = recode_lossless(&src, Lossless::Alac, &CancelToken::default()).unwrap_err();
    TAMPER.set(None);
    assert_eq!(e.to_string(), "转换后标签不一致，未替换");
    assert!(std::fs::read(&src).unwrap() == bytes);
    no_parts(&t.0);
}

/// Apple 档新导入 3、8 声道的 FLAC：ALAC 保不住声道数与布局，照原格式档出 FLAC，声道数不变。
#[tokio::test]
async fn apple_keeps_multichannel_flac_as_flac() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-ch");
    for n in ["3", "8"] {
        let src = t.0.join(format!("c{n}.flac"));
        gen(&ff, &src, &["-ac", n, "-c:a", "flac"]);
        let o = apple(&src, &t.0.join(format!("out{n}")), &NoopEnricher).await;
        assert_eq!(o.output.extension().unwrap(), "flac", "{n}");
        let ch = read_part(&o.output).properties().channels();
        assert_eq!(ch, Some(n.parse().unwrap()), "{n}");
        assert_eq!(pcm(&ff, &src), pcm(&ff, &o.output), "{n}");
    }
}

// ---------- 第三轮复审：白名单收紧到我们自己的产物 ----------

/// 第三轮反例一：会被当成容器字段忽略的名字写成自定义 Vorbis 键。直接转、续跑都拒。
#[test]
fn review3_container_field_names_as_vorbis_keys_are_refused() {
    use crate::recode::HALT_AFTER_PLACE;
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-r3-brand");
    let put = |vc: &mut lofty::ogg::tag::VorbisComments| {
        vc.push("major_brand".into(), "M4A ".into());
        vc.push("minor_version".into(), "512".into());
        vc.push("compatible_brands".into(), "M4A isom".into());
    };
    let a = sample(&ff, &t, "a.flac", &["-c:a", "flac"]);
    with_vc(&a, put);
    refused(&t, &a, Lossless::Alac, "MAJOR_BRAND");

    // 续跑：上次换上 .m4a 后中断，源里多了这三个键（ffmpeg 两边都当容器字段丢掉）
    let tok = CancelToken::default();
    let b = sample(&ff, &t, "b.flac", &["-c:a", "flac"]);
    HALT_AFTER_PLACE.set(true);
    let _ = recode_lossless(&b, Lossless::Alac, &tok).unwrap_err();
    HALT_AFTER_PLACE.set(false);
    with_vc(&b, put);
    let m4a = b.with_extension("m4a");
    let (sb, mb) = (std::fs::read(&b).unwrap(), std::fs::read(&m4a).unwrap());
    assert!(!recodable(&b, Lossless::Alac));
    assert!(recode_lossless(&b, Lossless::Alac, &tok).is_err());
    assert!(std::fs::read(&b).unwrap() == sb && std::fs::read(&m4a).unwrap() == mb);
    no_parts(&t.0);
}

/// 第三轮反例二：空值（自定义键、白名单内的键）与空的 UTF8 自由格式原子。
#[test]
fn review3_empty_values_are_refused() {
    use lofty::mp4::{Atom, AtomData, AtomIdent, Mp4File};
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-r3-empty");
    let a = sample(&ff, &t, "a.flac", &["-c:a", "flac"]);
    add_vc_raw(&a, &["REVIEW_EMPTY="]);
    refused(&t, &a, Lossless::Alac, "REVIEW_EMPTY");
    let b = sample(&ff, &t, "b.flac", &["-c:a", "flac"]);
    add_vc_raw(&b, &["COMMENT="]);
    refused(&t, &b, Lossless::Alac, "COMMENT 的值为空");

    // 空的 UTF8 自由格式原子、空的 ©grp：lofty 照写，但读回时跳过空值
    for (name, ident, why) in [
        (
            "c.flac",
            AtomIdent::Freeform {
                mean: "com.apple.iTunes".into(),
                name: "EMPTY".into(),
            },
            "----:com.apple.iTunes:EMPTY",
        ),
        ("d.flac", AtomIdent::Fourcc(*b"\xa9grp"), "©grp 的值为空"),
    ] {
        let src = sample(&ff, &t, name, &["-c:a", "flac"]);
        let m4a = recode_lossless(&src, Lossless::Alac, &CancelToken::default()).unwrap();
        let mut f = Mp4File::read_from(&mut std::fs::File::open(&m4a).unwrap(), Default::default())
            .unwrap();
        f.ilst_mut()
            .unwrap()
            .insert(Atom::new(ident, AtomData::UTF8(String::new())));
        f.save_to_path(&m4a, lofty::config::WriteOptions::default())
            .unwrap();
        refused(&t, &m4a, Lossless::Flac, why);
    }
}

/// ID3v2.4 帧：ID、同步安全长度、无标志位。
fn id3_frame(id: &str, body: &[u8]) -> Vec<u8> {
    let n = body.len() as u32;
    let mut f = id.as_bytes().to_vec();
    f.extend([
        (n >> 21) as u8 & 0x7f,
        (n >> 14) as u8 & 0x7f,
        (n >> 7) as u8 & 0x7f,
        n as u8 & 0x7f,
    ]);
    f.extend([0, 0]);
    f.extend(body);
    f
}

/// UTF-8 文本帧。
fn id3_text(id: &str, s: &str) -> Vec<u8> {
    id3_frame(id, &[&[3u8][..], s.as_bytes()].concat())
}

/// 不带标签的 16 bit WAV，末尾加一个由 `frames` 组成的 ID3v2.4 `id3 ` 块（逐字节自己写，不经 lofty）。
fn wav_with_id3(ff: &Path, t: &Tmp, name: &str, frames: &[Vec<u8>]) -> PathBuf {
    let p = t.0.join(name);
    gen(ff, &p, &["-c:a", "pcm_s16le", "-bitexact"]);
    let body: Vec<u8> = frames.concat();
    let n = body.len() as u32;
    let mut tag = b"ID3\x04\x00\x00".to_vec();
    tag.extend([
        (n >> 21) as u8 & 0x7f,
        (n >> 14) as u8 & 0x7f,
        (n >> 7) as u8 & 0x7f,
        n as u8 & 0x7f,
    ]);
    tag.extend(body);
    if tag.len() % 2 == 1 {
        tag.push(0);
    }
    let mut b = std::fs::read(&p).unwrap();
    b.extend(b"id3 ");
    b.extend((tag.len() as u32).to_le_bytes());
    b.extend(tag);
    let riff = (b.len() - 8) as u32;
    b[4..8].copy_from_slice(&riff.to_le_bytes());
    std::fs::write(&p, b).unwrap();
    p
}

/// 第三轮反例三：WAV 里带语言 jpn 的 COMM、USLT（语言属性两边表达不了）。
#[test]
fn review3_wav_comm_and_uslt_are_refused() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-r3-lang");
    let lang = |id: &str, s: &str| id3_frame(id, &[&[3u8][..], b"jpn\0", s.as_bytes()].concat());
    let title = id3_text("TIT2", "标题");
    let a = wav_with_id3(&ff, &t, "a.wav", &[title.clone(), lang("COMM", "注释")]);
    refused(&t, &a, Lossless::Alac, "COMM");
    let b = wav_with_id3(&ff, &t, "b.wav", &[title, lang("USLT", "[00:01.00]词")]);
    refused(&t, &b, Lossless::Alac, "USLT");
}

/// 第三轮反例四：WAV 里两个 description 相同的 TXXX（lofty 读时折叠成一个）；同一标准帧出现两次同理。
#[test]
fn review3_wav_duplicate_frames_are_refused() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-r3-txxx");
    let txxx = |v: &str| id3_frame("TXXX", &[&[3u8][..], b"REVIEW\0", v.as_bytes()].concat());
    let a = wav_with_id3(&ff, &t, "a.wav", &[txxx("一"), txxx("二")]);
    refused(&t, &a, Lossless::Alac, "TXXX");
    let b = wav_with_id3(
        &ff,
        &t,
        "b.wav",
        &[id3_text("TIT2", "一"), id3_text("TIT2", "二")],
    );
    refused(&t, &b, Lossless::Alac, "TIT2");
    // 只用白名单内的标准帧、各一次：照转
    let c = wav_with_id3(
        &ff,
        &t,
        "c.wav",
        &[
            id3_text("TIT2", "标题"),
            id3_text("TPE1", "歌手"),
            id3_text("TRCK", "3/12"),
        ],
    );
    let m4a = recode_lossless(&c, Lossless::Alac, &CancelToken::default()).unwrap();
    assert!(is_alac(&m4a));
}

/// 转换期间源文件被改了权限：不删源、不留新文件，记失败。
#[test]
fn source_changed_during_recode_is_not_replaced() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-chmod");
    let src = sample(&ff, &t, "a.flac", &["-c:a", "flac"]);
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o644)).unwrap();
    let bytes = std::fs::read(&src).unwrap();
    TAMPER.set(Some(|p: &Path| {
        let src = p.parent().unwrap().join("a.flac");
        std::fs::set_permissions(src, std::fs::Permissions::from_mode(0o400)).unwrap();
    }));
    let e = recode_lossless(&src, Lossless::Alac, &CancelToken::default()).unwrap_err();
    TAMPER.set(None);
    assert_eq!(e.to_string(), "转换期间原文件被改动，未替换");
    assert!(std::fs::read(&src).unwrap() == bytes);
    let mode = std::fs::metadata(&src).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o400);
    assert!(!src.with_extension("m4a").exists());
    no_parts(&t.0);
}

/// convert(Apple) 产出的 ALAC 转 FLAC 再转回 ALAC：每次都成功，独立读到的标签不变。
#[tokio::test]
async fn our_own_apple_output_recodes_both_ways() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-own-alac");
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
        format: OutputFormat::Apple,
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
    assert!(is_alac(&o.output));
    let want = ff_meta(&ff, &o.output);
    assert!(has(&want, "title", "歌名"), "{want:?}");
    let tok = CancelToken::default();
    let flac = recode_lossless(&o.output, Lossless::Flac, &tok).unwrap();
    let back = recode_lossless(&flac, Lossless::Alac, &tok).unwrap();
    let mut a = want.clone();
    let mut b = ff_meta(&ff, &back);
    a.sort();
    b.sort();
    assert_eq!(a, b);
}

/// 复审四：导入带曲序与碟号总数的 FLAC，两档产物都照搬总数。
#[tokio::test]
async fn import_keeps_track_and_disc_totals() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("ap-totals");
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
            "TRACKNUMBER=3",
            "-metadata",
            "TRACKTOTAL=12",
            "-metadata",
            "DISCNUMBER=1",
            "-metadata",
            "DISCTOTAL=2",
        ],
    );
    for (dir, format) in [
        ("apple", OutputFormat::Apple),
        ("orig", OutputFormat::Original),
    ] {
        let out = t.0.join(dir);
        std::fs::create_dir_all(&out).unwrap();
        let opts = ConvertOptions {
            output_dir: out,
            format,
            ffmpeg: find_ffmpeg(),
        };
        let o = convert(
            &src,
            &opts,
            &NoopEnricher,
            None,
            &CancelToken::default(),
            &mut |_| {},
        )
        .await
        .unwrap();
        let f = lofty::read_from_path(&o.output).unwrap();
        let tag = f.primary_tag().unwrap();
        assert_eq!(
            (tag.track(), tag.track_total(), tag.disk(), tag.disk_total()),
            (Some(3), Some(12), Some(1), Some(2)),
            "{dir}: {}",
            o.output.display()
        );
    }
}
