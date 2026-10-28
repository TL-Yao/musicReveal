//! `convert` 端到端：ffmpeg 运行时生成明文，合成器包成 NCM / QMC v2 / musicex。

use super::{build_ncm, enc, make_ekey_v1, musicex, qmc2, qtag, Tmp};
use crate::*;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::{Accessor, ItemKey};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

pub(super) fn ffmpeg() -> Option<PathBuf> {
    let f = find_ffmpeg();
    if f.is_none() {
        eprintln!("跳过：找不到 ffmpeg（可设 REVEAL_FFMPEG）");
    }
    f
}

pub(super) fn gen(ff: &Path, out: &Path, args: &[&str]) -> Vec<u8> {
    let st = Command::new(ff)
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=f=440:d=2"])
        .args(args)
        .arg(out)
        .status()
        .unwrap();
    assert!(st.success());
    std::fs::read(out).unwrap()
}

fn flac24(ff: &Path, t: &Tmp, tags: &[&str]) -> Vec<u8> {
    let mut a = vec!["-ar", "48000", "-sample_fmt", "s32", "-c:a", "flac"];
    a.extend(tags);
    gen(ff, &t.0.join("plain.flac"), &a)
}

pub(super) fn png(ff: &Path, t: &Tmp) -> Vec<u8> {
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

const META: &str = r#"{"musicId":"42","musicName":"测试歌","artist":[["歌手甲",1],["歌手乙",2]],"album":"测试专辑","format":"flac"}"#;

struct Run {
    stages: Vec<Stage>,
    res: Result<ConvertOutcome, ConvertError>,
}

async fn run(
    input: &Path,
    out: &Path,
    format: OutputFormat,
    enricher: &dyn Enricher,
    cancel_at: Option<Stage>,
) -> Run {
    let opts = ConvertOptions {
        output_dir: out.to_path_buf(),
        format,
        ffmpeg: find_ffmpeg(),
    };
    let cancel = CancelToken::default();
    let c = cancel.clone();
    let stages = Arc::new(Mutex::new(vec![]));
    let s = stages.clone();
    let mut progress = move |st: Stage| {
        if Some(st) == cancel_at {
            c.cancel();
        }
        s.lock().unwrap().push(st);
    };
    let before = std::fs::read(input).unwrap();
    let res = convert(input, &opts, enricher, None, &cancel, &mut progress).await;
    assert!(std::fs::read(input).unwrap() == before, "源文件被修改");
    let leftovers: Vec<_> = std::fs::read_dir(out)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
        .collect();
    assert!(leftovers.is_empty(), "残留 .part: {leftovers:?}");
    let stages = stages.lock().unwrap().clone();
    Run { stages, res }
}

fn read_back(p: &Path) -> lofty::file::TaggedFile {
    lofty::read_from_path(p).unwrap()
}

fn name(p: &Path) -> &str {
    p.file_name().unwrap().to_str().unwrap()
}

fn out_dir(t: &Tmp) -> PathBuf {
    let d = t.0.join("out");
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[tokio::test]
async fn ncm_flac_original_keeps_flac_with_tags_and_cover() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("pl-ncm");
    // 末帧后带垃圾（真实 QQ mflac 即如此），输出应被修剪干净
    let plain = [flac24(&ff, &t, &[]), vec![0x47; 30]].concat();
    let cover = png(&ff, &t);
    let input = t.write("src.ncm", &build_ncm(&plain, b"k123", META, Some(&cover)));
    let out = out_dir(&t);
    let r = run(&input, &out, OutputFormat::Original, &NoopEnricher, None).await;
    let o = r.res.unwrap();
    assert_eq!(r.stages, vec![Stage::Decrypt, Stage::Metadata]);
    assert_eq!(name(&o.output), "歌手甲、歌手乙 - 测试歌.flac");
    let dec = Command::new(&ff)
        .args(["-v", "error", "-i"])
        .arg(&o.output)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(
        dec.status.success() && dec.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&dec.stderr)
    );
    assert_eq!(o.quality, "FLAC 24bit/48kHz");
    assert!(o.has_cover && !o.has_lyrics && o.lrc.is_none() && o.renamed.is_none());
    assert_eq!(o.meta.platform, Platform::Netease);
    assert_eq!(o.meta.song_id, Some(SongId::Netease(42)));
    let f = read_back(&o.output);
    let tag = f.primary_tag().unwrap();
    assert_eq!(tag.title().as_deref(), Some("测试歌"));
    assert_eq!(
        tag.get_strings(ItemKey::TrackArtist).collect::<Vec<_>>(),
        ["歌手甲, 歌手乙"]
    );
    assert_eq!(tag.album().as_deref(), Some("测试专辑"));
    assert_eq!(tag.pictures().len(), 1);
    assert_eq!(tag.pictures()[0].data(), &cover[..]);

    // 再跑一次：加编号
    let r2 = run(&input, &out, OutputFormat::Original, &NoopEnricher, None).await;
    let o2 = r2.res.unwrap();
    assert_eq!(name(&o2.output), "歌手甲、歌手乙 - 测试歌 (2).flac");
    assert_eq!(o2.renamed, Some(2));
}

#[tokio::test]
async fn ncm_flac_to_mp3_is_cbr_320_with_tags() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("pl-mp3");
    let plain = flac24(&ff, &t, &[]);
    let cover = png(&ff, &t);
    let input = t.write("src.ncm", &build_ncm(&plain, b"k123", META, Some(&cover)));
    let out = out_dir(&t);
    let r = run(&input, &out, OutputFormat::Mp3, &NoopEnricher, None).await;
    let o = r.res.unwrap();
    assert_eq!(
        r.stages,
        vec![Stage::Decrypt, Stage::Transcode, Stage::Metadata]
    );
    assert_eq!(name(&o.output), "歌手甲、歌手乙 - 测试歌.mp3");
    assert_eq!(o.quality, "MP3 320k");
    let f = read_back(&o.output);
    let kbps = f.properties().audio_bitrate().unwrap();
    assert!((310..=330).contains(&kbps), "{kbps}");
    let tag = f.primary_tag().unwrap();
    assert_eq!(tag.title().as_deref(), Some("测试歌"));
    assert_eq!(tag.artist().as_deref(), Some("歌手甲, 歌手乙"));
    assert_eq!(tag.album().as_deref(), Some("测试专辑"));
    assert_eq!(tag.pictures().len(), 1);
}

fn ffprobe_tags(ff: &Path, p: &Path) -> Vec<(String, String)> {
    let out = Command::new(ff.with_file_name("ffprobe"))
        .args(["-v", "error", "-show_entries", "format_tags", "-of", "flat"])
        .arg(p)
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| {
            let (k, v) = l.strip_prefix("format.tags.")?.split_once('=')?;
            Some((k.to_lowercase(), v.trim_matches('"').to_string()))
        })
        .collect()
}

#[tokio::test]
async fn mp3_keeps_every_source_tag() {
    let Some(ff) = ffmpeg() else { return };
    if !ff.with_file_name("ffprobe").exists() {
        return eprintln!("跳过：ffmpeg 旁没有 ffprobe");
    }
    let t = Tmp::new("pl-mp3keep");
    let plain = flac24(
        &ff,
        &t,
        &[
            "-metadata",
            "composer=Composer",
            "-metadata",
            "comment=KeepMe",
            "-metadata",
            "artist=原歌手",
            "-metadata",
            "title=原标题",
            "-metadata",
            "lyrics=[00:01.00]旧歌词",
        ],
    );
    let input = t.write("src.ncm", &build_ncm(&plain, b"k", META, None));
    let mut keys = vec![];
    for fmt in [OutputFormat::Original, OutputFormat::Mp3] {
        let out = t.0.join(format!("out-{fmt:?}"));
        std::fs::create_dir_all(&out).unwrap();
        let o = run(&input, &out, fmt, &LyricEnricher, None)
            .await
            .res
            .unwrap();
        let tags = ffprobe_tags(&ff, &o.output);
        let get = |k: &str| {
            tags.iter()
                .filter(|(n, _)| n == k)
                .map(|(_, v)| v.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(get("composer"), ["Composer"], "{fmt:?} {tags:?}");
        assert_eq!(get("comment"), ["KeepMe"], "{fmt:?} {tags:?}");
        assert_eq!(get("artist"), ["歌手甲, 歌手乙"], "{fmt:?} {tags:?}");
        assert_eq!(get("title"), ["测试歌"], "{fmt:?} {tags:?}");
        assert!(
            !tags.iter().any(|(_, v)| v.contains("旧歌词")),
            "{fmt:?} {tags:?}"
        );
        // ID3 的 USLT 在 ffprobe 里叫 lyrics-<语言>
        let mut k: Vec<String> = tags
            .into_iter()
            .map(|(k, _)| {
                if k.starts_with("lyrics") {
                    "lyrics".into()
                } else {
                    k
                }
            })
            .collect();
        k.sort();
        keys.push(k);
    }
    assert_eq!(keys[0], keys[1], "MP3 与原格式的标签集合不同");
}

#[tokio::test]
async fn mp3_source_is_not_reencoded() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("pl-mp3src");
    let plain = gen(
        &ff,
        &t.0.join("p.mp3"),
        &["-c:a", "libmp3lame", "-b:a", "128k"],
    );
    let input = t.write("src.ncm", &build_ncm(&plain, b"k9", META, None));
    let out = out_dir(&t);
    let r = run(&input, &out, OutputFormat::Mp3, &NoopEnricher, None).await;
    let o = r.res.unwrap();
    assert_eq!(r.stages, vec![Stage::Decrypt, Stage::Metadata]);
    assert_eq!(o.quality, "MP3 128k");
    assert!(!o.has_cover);
}

#[tokio::test]
async fn qmc_v2_inner_flac_tags_are_used() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("pl-qmc");
    let plain = flac24(
        &ff,
        &t,
        &[
            "-metadata",
            "title=二次元凶",
            "-metadata",
            "artist=hanser",
            "-metadata",
            "album=二次元凶专辑",
        ],
    );
    let master: Vec<u8> = (0..256).map(|i| (i * 7 + 1) as u8).collect();
    let input = t.write(
        "hanser-x.mflac",
        &qtag(&qmc2(&master, &plain), &make_ekey_v1(&master)),
    );
    let out = out_dir(&t);
    for (fmt, ext) in [(OutputFormat::Original, "flac"), (OutputFormat::Mp3, "mp3")] {
        let o = run(&input, &out, fmt, &NoopEnricher, None)
            .await
            .res
            .unwrap();
        assert_eq!(name(&o.output), format!("hanser - 二次元凶.{ext}"));
        assert_eq!(o.meta.platform, Platform::Qq);
        let f = read_back(&o.output);
        let tag = f.primary_tag().unwrap();
        assert_eq!(tag.artist().as_deref(), Some("hanser"));
        assert_eq!(tag.album().as_deref(), Some("二次元凶专辑"));
    }
}

struct LyricEnricher;
#[async_trait::async_trait]
impl Enricher for LyricEnricher {
    async fn enrich(&self, meta: &mut TrackMeta) -> Result<(), String> {
        meta.year = Some(2021);
        meta.track = Some(3);
        meta.disc = Some(1);
        meta.album_artist = Some("群星".into());
        meta.lyrics = Some(Lyrics {
            lrc: "[ar:x]\n[00:01.00]第一句\n[00:02.50]second".into(),
            translated_lrc: Some("[00:02.50]第二句".into()),
        });
        Err("后续步骤失败也不影响转换".into())
    }
}

/// `LyricEnricher` 的原文与译文按时刻合并后的 LRC。
const MERGED: &str = "[ar:x]\n[00:01.00]第一句\n[00:02.50]second\n[00:02.50]第二句\n";

#[tokio::test]
async fn enricher_fields_and_lyrics_are_written() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("pl-lrc");
    let plain = flac24(&ff, &t, &[]);
    let input = t.write("src.ncm", &build_ncm(&plain, b"k", META, None));
    let out = out_dir(&t);
    let o = run(&input, &out, OutputFormat::Original, &LyricEnricher, None)
        .await
        .res
        .unwrap();
    assert!(o.has_lyrics);
    let lrc = o.lrc.unwrap();
    assert_eq!(lrc, out.join("lrc/歌手甲、歌手乙 - 测试歌.lrc"));
    assert_eq!(std::fs::read_to_string(&lrc).unwrap(), MERGED);
    let f = read_back(&o.output);
    let tag = f.primary_tag().unwrap();
    assert_eq!(tag.get_string(ItemKey::Lyrics), Some(MERGED));
    assert_eq!(tag.date().map(|d| d.year), Some(2021));
    assert_eq!((tag.track(), tag.disk()), (Some(3), Some(1)));
    assert_eq!(tag.get_string(ItemKey::AlbumArtist), Some("群星"));
}

#[tokio::test]
async fn cancel_during_decrypt_or_transcode_leaves_nothing() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("pl-cancel");
    let plain = flac24(&ff, &t, &[]);
    let input = t.write("src.ncm", &build_ncm(&plain, b"k", META, None));
    let out = out_dir(&t);
    for at in [Stage::Decrypt, Stage::Transcode] {
        let r = run(&input, &out, OutputFormat::Mp3, &NoopEnricher, Some(at)).await;
        assert!(matches!(r.res, Err(ConvertError::Cancelled)), "{at:?}");
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0, "{at:?}");
    }
}

#[tokio::test]
async fn musicex_without_credentials_needs_login() {
    let t = Tmp::new("pl-mx");
    let master: Vec<u8> = (0..256).map(|i| (i * 3 + 1) as u8).collect();
    let input = t.write(
        "a.mflac",
        &musicex(&qmc2(&master, &super::audio(b"fLaC", 50_000))),
    );
    let out = out_dir(&t);
    let r = run(&input, &out, OutputFormat::Original, &NoopEnricher, None).await;
    assert!(
        matches!(r.res, Err(ConvertError::NeedsLogin)),
        "{:?}",
        r.res
    );
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
}

#[tokio::test]
async fn incomplete_source_is_reported() {
    let t = Tmp::new("pl-inc");
    let input = t.write("a.ncm", b"CTENF");
    let out = out_dir(&t);
    let r = run(&input, &out, OutputFormat::Original, &NoopEnricher, None).await;
    assert!(
        matches!(r.res, Err(ConvertError::Incomplete)),
        "{:?}",
        r.res
    );
}

#[tokio::test]
async fn lyrics_are_embedded_in_every_format() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("pl-lyr4");
    let cases: [(&str, &[&str]); 4] = [
        ("flac", &["-c:a", "flac"]),
        ("ogg", &["-ac", "2", "-c:a", "vorbis", "-strict", "-2"]),
        ("m4a", &["-c:a", "aac", "-f", "ipod"]),
        ("mp3", &["-c:a", "libmp3lame", "-b:a", "128k"]),
    ];
    for (ext, args) in cases {
        let plain = gen(&ff, &t.0.join(format!("p.{ext}")), args);
        let input = t.write(&format!("{ext}.ncm"), &build_ncm(&plain, b"k", META, None));
        let out = t.0.join(format!("out-{ext}"));
        std::fs::create_dir_all(&out).unwrap();
        let o = run(&input, &out, OutputFormat::Original, &LyricEnricher, None)
            .await
            .res
            .unwrap();
        assert_eq!(o.output.extension().unwrap(), ext);
        let f = read_back(&o.output);
        let tag = f.primary_tag().unwrap();
        let lyr = tag
            .get_string(ItemKey::Lyrics)
            .or(tag.get_string(ItemKey::UnsyncLyrics));
        assert_eq!(lyr, Some(MERGED), "{ext}");
    }
}

#[tokio::test]
async fn blank_embedded_meta_does_not_hide_inner_tags() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("pl-blank");
    let plain = flac24(
        &ff,
        &t,
        &[
            "-metadata",
            "title=内层歌名",
            "-metadata",
            "artist=内层歌手",
            "-metadata",
            "album=内层专辑",
        ],
    );
    let meta =
        r#"{"musicId":"1","musicName":"  ","artist":[["",0],[" ",0]],"album":"","format":"flac"}"#;
    let input = t.write("src.ncm", &build_ncm(&plain, b"k", meta, None));
    let out = out_dir(&t);
    let o = run(&input, &out, OutputFormat::Original, &NoopEnricher, None)
        .await
        .res
        .unwrap();
    assert_eq!(o.meta.title.as_deref(), Some("内层歌名"));
    assert_eq!(o.meta.artists, ["内层歌手"]);
    assert_eq!(o.meta.album.as_deref(), Some("内层专辑"));
    assert_eq!(name(&o.output), "内层歌手 - 内层歌名.flac");
}

struct StuckEnricher;
#[async_trait::async_trait]
impl Enricher for StuckEnricher {
    async fn enrich(&self, _meta: &mut TrackMeta) -> Result<(), String> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn cancel_interrupts_a_stuck_enricher() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("pl-stuck");
    let plain = flac24(&ff, &t, &[]);
    let input = t.write("src.ncm", &build_ncm(&plain, b"k", META, None));
    let out = out_dir(&t);
    let opts = ConvertOptions {
        output_dir: out.clone(),
        format: OutputFormat::Original,
        ffmpeg: None,
    };
    // 1) enrich 进行中被取消
    let cancel = CancelToken::default();
    let c = cancel.clone();
    let start = std::time::Instant::now();
    let mut entered = None;
    let mut progress = |s: Stage| {
        if s == Stage::Metadata {
            entered = Some(std::time::Instant::now());
            let c = c.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                c.cancel();
            });
        }
    };
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        convert(&input, &opts, &StuckEnricher, None, &cancel, &mut progress),
    )
    .await;
    assert!(matches!(r, Ok(Err(ConvertError::Cancelled))), "{r:?}");
    let since = entered.unwrap().elapsed();
    assert!(
        since < std::time::Duration::from_millis(300),
        "{since:?} / {:?}",
        start.elapsed()
    );
    // 2) 进入 enrich 前已取消
    let cancel = CancelToken::default();
    let c = cancel.clone();
    let mut progress = |s: Stage| {
        if s == Stage::Metadata {
            c.cancel();
        }
    };
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        convert(&input, &opts, &StuckEnricher, None, &cancel, &mut progress),
    )
    .await;
    assert!(matches!(r, Ok(Err(ConvertError::Cancelled))), "{r:?}");
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
}

/// 外层 future 在解密任务排队时被丢弃：后台任务事后运行也必须自行清理 `.part`。
#[test]
fn dropped_convert_future_leaves_no_part_files() {
    let t = Tmp::new("pl-drop");
    let input = t.write(
        "src.ncm",
        &build_ncm(&super::audio(b"fLaC", 3_000_000), b"k", META, None),
    );
    let out = out_dir(&t);
    let rt = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let occupy = tokio::task::spawn_blocking(move || rx.recv().ok());
        let opts = ConvertOptions {
            output_dir: out.clone(),
            format: OutputFormat::Original,
            ffmpeg: None,
        };
        let cancel = CancelToken::default();
        let mut progress = |_| {};
        let fut = convert(&input, &opts, &NoopEnricher, None, &cancel, &mut progress);
        let r = tokio::time::timeout(std::time::Duration::from_millis(100), fut).await;
        assert!(r.is_err(), "转换不应在阻塞池被占满时完成");
        tx.send(()).unwrap();
        occupy.await.unwrap();
        // 阻塞池只有一个线程且先进先出：这个标记任务完成时，被遗弃的解密任务已经跑完
        tokio::task::spawn_blocking(|| ()).await.unwrap();
    });
    let left: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_converts_with_same_name_never_overwrite() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("pl-race");
    let plain = flac24(&ff, &t, &[]);
    let input = t.write("src.ncm", &build_ncm(&plain, b"k", META, None));
    let out = out_dir(&t);
    const N: usize = 8;
    let mut tasks = vec![];
    for _ in 0..N {
        let (input, out) = (input.clone(), out.clone());
        tasks.push(tokio::spawn(async move {
            let opts = ConvertOptions {
                output_dir: out,
                format: OutputFormat::Original,
                ffmpeg: None,
            };
            convert(
                &input,
                &opts,
                &LyricEnricher,
                None,
                &CancelToken::default(),
                &mut |_| {},
            )
            .await
            .unwrap()
        }));
    }
    let mut outputs = vec![];
    for t in tasks {
        let o = t.await.unwrap();
        outputs.push(o.output.clone());
        outputs.push(o.lrc.unwrap());
    }
    outputs.sort();
    outputs.dedup();
    assert_eq!(outputs.len(), 2 * N, "有输出路径重复（被覆盖）");
    let count = |d: &Path| std::fs::read_dir(d).unwrap().count();
    assert_eq!(count(&out), N + 1, "N 首歌 + lrc 文件夹");
    assert_eq!(count(&out.join("lrc")), N);
}

#[tokio::test]
async fn lrc_in_lrc_folder_is_overwritten_and_never_affects_audio_numbering() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("pl-lrc-exist");
    let plain = flac24(&ff, &t, &[]);
    let input = t.write("src.ncm", &build_ncm(&plain, b"k", META, None));
    let out = out_dir(&t);
    let lrcs = out.join("lrc");
    std::fs::create_dir_all(&lrcs).unwrap();
    // 旧版留在音频旁的 .lrc、lrc 文件夹里的同名 .lrc 都不再让音频换编号
    std::fs::write(out.join("歌手甲、歌手乙 - 测试歌.lrc"), "旁边").unwrap();
    std::fs::write(lrcs.join("歌手甲、歌手乙 - 测试歌.lrc"), "旧").unwrap();
    let o = run(&input, &out, OutputFormat::Original, &LyricEnricher, None)
        .await
        .res
        .unwrap();
    assert_eq!(name(&o.output), "歌手甲、歌手乙 - 测试歌.flac");
    assert_eq!(o.renamed, None);
    assert_eq!(o.lrc, Some(lrcs.join("歌手甲、歌手乙 - 测试歌.lrc")));
    assert_eq!(std::fs::read_to_string(o.lrc.unwrap()).unwrap(), MERGED);
    assert_eq!(
        std::fs::read_to_string(out.join("歌手甲、歌手乙 - 测试歌.lrc")).unwrap(),
        "旁边"
    );

    // 音频照旧不覆盖、换编号；编号对应的 .lrc 若是符号链接，换成普通文件，不写穿到目标
    let victim = t.write("victim.txt", b"victim");
    std::os::unix::fs::symlink(&victim, lrcs.join("歌手甲、歌手乙 - 测试歌 (2).lrc")).unwrap();
    let o = run(&input, &out, OutputFormat::Original, &LyricEnricher, None)
        .await
        .res
        .unwrap();
    assert_eq!(name(&o.output), "歌手甲、歌手乙 - 测试歌 (2).flac");
    assert_eq!(o.renamed, Some(2));
    let lrc = o.lrc.unwrap();
    assert_eq!(lrc, lrcs.join("歌手甲、歌手乙 - 测试歌 (2).lrc"));
    assert!(!std::fs::symlink_metadata(&lrc).unwrap().is_symlink());
    assert_eq!(std::fs::read_to_string(&lrc).unwrap(), MERGED);
    assert_eq!(std::fs::read(&victim).unwrap(), b"victim");
    let hidden: Vec<_> = std::fs::read_dir(&lrcs)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
        .collect();
    assert!(hidden.is_empty(), "{hidden:?}");
}

/// 普通音频，旁边放 `lrc`（为 None 不放），内嵌歌词 `embedded`；返回转换结果。
async fn plain_with_sidecar(
    tag: &str,
    lrc: Option<&str>,
    embedded: Option<&str>,
) -> (Tmp, PathBuf, ConvertOutcome) {
    let ff = find_ffmpeg().unwrap();
    let t = Tmp::new(tag);
    let src = t.0.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let input = src.join("旁带歌词.flac");
    let mut args = vec!["-c:a", "flac", "-metadata", "title=旁带"];
    let kv = embedded.map(|l| format!("lyrics={l}"));
    if let Some(kv) = &kv {
        args.extend(["-metadata", kv]);
    }
    gen(&ff, &input, &args);
    if let Some(l) = lrc {
        std::fs::write(input.with_extension("lrc"), l).unwrap();
    }
    let out = out_dir(&t);
    let o = run(&input, &out, OutputFormat::Original, &NoopEnricher, None)
        .await
        .res
        .unwrap();
    if let Some(l) = lrc {
        assert_eq!(
            std::fs::read_to_string(input.with_extension("lrc")).unwrap(),
            l,
            "源旁的 .lrc 不动"
        );
    }
    (t, out, o)
}

#[tokio::test]
async fn plain_audio_takes_the_lrc_beside_it() {
    if ffmpeg().is_none() {
        return;
    }
    let text = "[ti:旁]\n[00:01.00]旁边的词\n";
    let (_t, out, o) = plain_with_sidecar("pl-side", Some(text), None).await;
    assert!(o.has_lyrics);
    assert_eq!(read_lyrics(&o.output).as_deref(), Some(text));
    assert_eq!(o.lrc, Some(out.join("lrc/旁带.lrc")));
    assert_eq!(std::fs::read_to_string(o.lrc.unwrap()).unwrap(), text);

    // 只有纯文本内嵌歌词：同样换成带时间的 .lrc
    let (_t, _, o) = plain_with_sidecar("pl-side-plain", Some(text), Some("纯文本")).await;
    assert_eq!(read_lyrics(&o.output).as_deref(), Some(text));
    assert!(o.lrc.is_some());
}

#[tokio::test]
async fn sidecar_without_timed_lines_or_with_timed_embedded_lyrics_is_ignored() {
    if ffmpeg().is_none() {
        return;
    }
    let (_t, out, o) =
        plain_with_sidecar("pl-side-untimed", Some("[ti:只有标签]\n纯文本\n"), None).await;
    assert!(!o.has_lyrics && o.lrc.is_none());
    assert_eq!(read_lyrics(&o.output), None);
    assert!(!out.join("lrc").exists());

    let (_t, _, o) = plain_with_sidecar(
        "pl-side-timed",
        Some("[00:01.00]旁边\n"),
        Some("[00:02.00]内嵌"),
    )
    .await;
    assert_eq!(read_lyrics(&o.output).as_deref(), Some("[00:02.00]内嵌"));
    assert!(o.lrc.is_none());
}

fn decodes_cleanly(ff: &Path, p: &Path) {
    let dec = Command::new(ff)
        .args(["-v", "error", "-i"])
        .arg(p)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(
        dec.status.success() && dec.stderr.is_empty(),
        "{}: {}",
        p.display(),
        String::from_utf8_lossy(&dec.stderr)
    );
}

/// WMA / APE 解密后总要转码：APE → FLAC（原格式）或 MP3，WMA → MP3；文件自带标签与封面保留。
async fn legacy_container_case(
    ff: &Path,
    t: &Tmp,
    src: &str,
    plain: &[u8],
    cover: &[u8],
    expect: [&str; 2],
) {
    let input = t.write(src, &enc(&crate::qmc::Cipher::static_v1(), plain));
    for (fmt, ext) in [OutputFormat::Original, OutputFormat::Mp3]
        .into_iter()
        .zip(expect)
    {
        let out = t.0.join(format!("out-{src}-{fmt:?}"));
        std::fs::create_dir_all(&out).unwrap();
        let r = run(&input, &out, fmt, &NoopEnricher, None).await;
        let o = r.res.unwrap_or_else(|e| panic!("{src} {fmt:?}: {e}"));
        assert_eq!(
            name(&o.output),
            format!("歌手 - 标题.{ext}"),
            "{src} {fmt:?}"
        );
        assert_eq!(
            r.stages,
            vec![Stage::Decrypt, Stage::Transcode, Stage::Metadata]
        );
        decodes_cleanly(ff, &o.output);
        let f = read_back(&o.output);
        let tag = f.primary_tag().unwrap();
        assert_eq!(tag.title().as_deref(), Some("标题"), "{src} {fmt:?}");
        assert_eq!(tag.artist().as_deref(), Some("歌手"), "{src} {fmt:?}");
        assert_eq!(
            tag.get_string(ItemKey::Composer),
            Some("作曲"),
            "{src} {fmt:?}"
        );
        assert!(o.has_cover, "{src} {fmt:?}");
        assert_eq!(tag.pictures().len(), 1, "{src} {fmt:?}");
        assert_eq!(tag.pictures()[0].data(), cover, "{src} {fmt:?}");
    }
}

#[tokio::test]
async fn bkcwma_is_transcoded_to_mp3_in_both_modes() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("pl-wma");
    let cover = png(&ff, &t);
    let p = t.0.join("p.wma");
    // ASF 的 WM/Picture 在 ffmpeg 里是一条附带图片的视频流
    let st = Command::new(&ff)
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "sine=f=440:d=2",
            "-i",
        ])
        .arg(t.0.join("c.png"))
        .args(["-map", "0:a", "-map", "1:v", "-c:a", "wmav2", "-b:a", "64k"])
        .args(["-c:v", "copy", "-disposition:v", "attached_pic"])
        .args(["-metadata", "title=标题", "-metadata", "artist=歌手"])
        .args(["-metadata", "composer=作曲"])
        .arg(&p)
        .status()
        .unwrap();
    assert!(st.success());
    let plain = std::fs::read(&p).unwrap();
    legacy_container_case(&ff, &t, "a.bkcwma", &plain, &cover, ["mp3", "mp3"]).await;
}

/// `tests/fixtures/sine.ape`：1 秒 440Hz 正弦波（ffmpeg lavfi 合成，22.05kHz 单声道），
/// 用 Monkey's Audio SDK 1343 的 `mac s.wav s.ape -c2000` 编码；ffmpeg 不能编码 APE。
#[tokio::test]
async fn bkcape_becomes_flac_or_mp3() {
    let Some(ff) = ffmpeg() else { return };
    let t = Tmp::new("pl-ape");
    let p = t.0.join("p.ape");
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sine.ape"),
        &p,
    )
    .unwrap();
    // 直接写 APEv2：通用 Tag 存 APE 时不会写图片
    use lofty::ape::{ApeItem, ApeTag};
    use lofty::tag::{ItemValue, TagExt};
    let cover = png(&ff, &t);
    let mut tag = ApeTag::new();
    let item = |k: &str, v: ItemValue| ApeItem::new(k.into(), v).unwrap();
    tag.insert(item("Title", ItemValue::Text("标题".into())));
    tag.insert(item("Artist", ItemValue::Text("歌手".into())));
    tag.insert(item("Composer", ItemValue::Text("作曲".into())));
    let pic = [&b"cover.png\0"[..], &cover].concat();
    tag.insert(item("Cover Art (Front)", ItemValue::Binary(pic)));
    tag.save_to_path(&p, lofty::config::WriteOptions::default())
        .unwrap();
    let plain = std::fs::read(&p).unwrap();
    legacy_container_case(&ff, &t, "a.bkcape", &plain, &cover, ["flac", "mp3"]).await;
}

#[tokio::test]
async fn corrupt_source_is_reported_as_corrupt_with_the_same_text() {
    let t = Tmp::new("pl-corrupt");
    let input = t.write("a.ncm", b"definitely not an ncm file");
    let out = out_dir(&t);
    let r = run(&input, &out, OutputFormat::Original, &NoopEnricher, None).await;
    let e = r.res.unwrap_err();
    assert!(matches!(e, ConvertError::Corrupt(_)), "{e:?}");
    assert_eq!(e.to_string(), "文件已损坏：不是 NCM 文件（魔数不符）");
}

#[test]
fn part_files_carry_the_app_prefix() {
    let mut parts = crate::convert::Parts::default();
    let p = parts.add(Path::new("/tmp"));
    let n = p.file_name().unwrap().to_str().unwrap();
    let hex = n
        .strip_prefix(".musicreveal-")
        .and_then(|n| n.strip_suffix(".part"))
        .unwrap_or_else(|| panic!("{n}"));
    assert!(
        !hex.is_empty()
            && hex
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "{n}"
    );
}
