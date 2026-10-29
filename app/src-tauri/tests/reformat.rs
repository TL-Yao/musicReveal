//! 切换输出格式时资料库已有歌的互转批次：真实 ffmpeg 合成的 1 秒素材，只写临时目录。

use musicreveal_app::engine::*;
use reveal_core::{CancelToken, ConvertError, ConvertOptions, ConvertOutcome, QqStatus, Stage};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "mr-reformat-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 批次不经转换器；这里只是凑齐引擎的构造参数。
struct NoConv;

#[async_trait::async_trait]
impl Converter for NoConv {
    async fn convert(
        &self,
        _: &Path,
        _: &ConvertOptions,
        _: &CancelToken,
        _: &mut (dyn FnMut(Stage) + Send),
    ) -> Result<ConvertOutcome, ConvertError> {
        Err(ConvertError::Failed("不用".into()))
    }
    fn qq_status(&self) -> QqStatus {
        QqStatus::LoggedOut
    }
}

/// 生成素材：`REVEAL_GEN_FFMPEG` 优先（随包精简版没有 lavfi 与有损编码器）。
fn gen(p: &Path, args: &[&str]) {
    let ff = std::env::var_os("REVEAL_GEN_FFMPEG")
        .map(PathBuf::from)
        .or_else(reveal_core::find_ffmpeg)
        .expect("需要 ffmpeg");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let st = std::process::Command::new(ff)
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=f=440:d=1"])
        .args(args)
        .arg(p)
        .status()
        .unwrap();
    assert!(st.success(), "{}", p.display());
}

fn flac(p: &Path, title: &str) -> PathBuf {
    gen(p, &["-c:a", "flac", "-metadata", &format!("title={title}")]);
    p.to_path_buf()
}

type Moves = Arc<Mutex<Vec<(String, String)>>>;

async fn engine(data: &Path, moves: &Moves) -> Engine {
    let m = moves.clone();
    let e = Engine::new(data.to_path_buf(), Arc::new(NoConv), Box::new(|_| {}))
        .with_poll_interval(None)
        .on_moved(Box::new(move |a, b| {
            m.lock().unwrap().push((a.into(), b.into()))
        }));
    e.start().await;
    e
}

/// 等批次结束。
async fn finished(e: &Engine) -> Reformat {
    let end = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(r) = e.snapshot().reformat.filter(|r| r.finished) {
            return r;
        }
        assert!(
            Instant::now() < end,
            "批次没结束：{:?}",
            e.snapshot().reformat
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn exts(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = walk(dir)
        .into_iter()
        .map(|p| p.strip_prefix(dir).unwrap().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_converts_the_library_both_ways() {
    let t = Tmp::new("both");
    let out = t.0.join("out");
    flac(&out.join("a.flac"), "歌一");
    flac(&out.join("sub/b.flac"), "歌二");
    gen(&out.join("c.wav"), &["-c:a", "pcm_s16le"]);
    // 有损与 AAC 不算
    gen(&out.join("d.ogg"), &["-c:a", "libopus"]);
    gen(&out.join("e.mp3"), &["-c:a", "libmp3lame"]);
    gen(&out.join("f.m4a"), &["-c:a", "aac", "-f", "ipod"]);
    let moves = Moves::default();
    let e = engine(&t.0.join("data"), &moves).await;
    e.complete_onboarding(vec![], out.to_string_lossy().into(), "original")
        .unwrap();
    assert!(e.snapshot().reformat.is_none());

    let p = e.preview_format("apple").unwrap();
    assert_eq!(p.count, 3);
    assert!(p.eta_secs >= 1);
    assert_eq!(e.preview_format("original").unwrap().count, 0);
    assert!(e.preview_format("mp3").is_err());

    e.set_format("apple").unwrap();
    let s = e.snapshot();
    assert_eq!(s.format, Fmt::Apple);
    let r = s.reformat.clone().expect("批次开始");
    assert_eq!((r.target, r.total, r.finished), (Fmt::Apple, 3, false));
    assert_eq!(
        e.set_format("original").unwrap_err(),
        "正在转换，完成前不能更改"
    );
    let json = serde_json::to_value(&s).unwrap();
    assert_eq!(json["reformat"]["target"], "apple");
    assert!(json["reformat"]["failed"].is_array());

    let r = finished(&e).await;
    assert_eq!((r.done, r.total), (3, 3));
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    assert_eq!((r.current, r.eta_secs), (None, None));
    assert_eq!(
        exts(&out),
        ["a.m4a", "c.m4a", "d.ogg", "e.mp3", "f.m4a", "sub/b.m4a"]
    );
    assert!(reveal_core::is_alac(&out.join("a.m4a")));
    assert!(!reveal_core::is_alac(&out.join("f.m4a")));
    let mut m = moves.lock().unwrap().clone();
    m.sort();
    assert_eq!(
        m,
        [
            ("a.flac", "a.m4a"),
            ("c.wav", "c.m4a"),
            ("sub/b.flac", "sub/b.m4a")
        ]
        .map(|(a, b)| (
            out.join(a).to_string_lossy().into_owned(),
            out.join(b).to_string_lossy().into_owned()
        ))
    );
    let text = std::fs::read_to_string(t.0.join("data/settings.json")).unwrap();
    assert!(
        text.contains("\"apple\"") && !text.contains("reformatting"),
        "{text}"
    );

    // 关掉：ALAC 转回 FLAC（WAV 也成了 FLAC），AAC 不动
    assert_eq!(e.preview_format("original").unwrap().count, 3);
    e.set_format("original").unwrap();
    let r = finished(&e).await;
    assert_eq!((r.target, r.done, r.failed.len()), (Fmt::Original, 3, 0));
    assert_eq!(
        exts(&out),
        ["a.flac", "c.flac", "d.ogg", "e.mp3", "f.m4a", "sub/b.flac"]
    );
    e.dismiss_reformat();
    assert!(e.snapshot().reformat.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_song_is_listed_and_the_rest_still_convert() {
    let t = Tmp::new("partial");
    let out = t.0.join("out");
    flac(&out.join("a.flac"), "第一首");
    flac(&out.join("b.flac"), "撞名的歌");
    flac(&out.join("c.flac"), "第三首");
    std::fs::write(out.join("b.m4a"), b"not mine").unwrap();
    let moves = Moves::default();
    let e = engine(&t.0.join("data"), &moves).await;
    e.complete_onboarding(vec![], out.to_string_lossy().into(), "original")
        .unwrap();
    e.set_format("apple").unwrap();
    let r = finished(&e).await;
    assert_eq!(r.done, 3);
    assert_eq!(r.failed.len(), 1);
    assert_eq!(r.failed[0].title, "撞名的歌");
    assert_eq!(r.failed[0].reason, "已有同名的 .m4a 文件，未覆盖");
    assert_eq!(exts(&out), ["a.m4a", "b.flac", "b.m4a", "c.m4a"]);
    assert_eq!(std::fs::read(out.join("b.m4a")).unwrap(), b"not mine");
    assert_eq!(moves.lock().unwrap().len(), 2);
    // 结束后能再切
    e.dismiss_reformat();
    e.set_format("original").unwrap();
    finished(&e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_batch_resumes_on_next_start() {
    let t = Tmp::new("resume");
    let out = t.0.join("out");
    for i in 0..4 {
        flac(&out.join(format!("{i}.flac")), &format!("歌{i}"));
    }
    let data = t.0.join("data");
    let moves = Moves::default();
    let e = engine(&data, &moves).await;
    e.complete_onboarding(vec![], out.to_string_lossy().into(), "original")
        .unwrap();
    e.set_format("apple").unwrap();
    e.shutdown().await;
    let text = std::fs::read_to_string(data.join("settings.json")).unwrap();
    assert!(text.contains("\"reformatting\":true"), "{text}");
    let left = walk(&out)
        .iter()
        .filter(|p| p.extension().unwrap() == "flac")
        .count();
    assert!(left > 0, "退出时应还没转完");
    assert!(
        !walk(&out)
            .iter()
            .any(|p| p.file_name().unwrap().to_string_lossy().starts_with('.')),
        "不留隐藏副本"
    );

    let e = engine(&data, &moves).await;
    let r = e.snapshot().reformat.expect("启动后接着转");
    assert_eq!((r.target, r.total), (Fmt::Apple, left as u32));
    let r = finished(&e).await;
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    assert_eq!(exts(&out), ["0.m4a", "1.m4a", "2.m4a", "3.m4a"]);
    let text = std::fs::read_to_string(data.join("settings.json")).unwrap();
    assert!(!text.contains("reformatting"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn onboarding_with_apple_and_old_mp3_setting_start_no_batch() {
    let t = Tmp::new("onboard");
    let out = t.0.join("out");
    flac(&out.join("a.flac"), "歌");
    let data = t.0.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let settings = serde_json::json!({
        "onboarded": true, "folders": [], "outputDir": out, "format": "mp3"
    });
    std::fs::write(data.join("settings.json"), settings.to_string()).unwrap();
    let moves = Moves::default();
    let e = engine(&data, &moves).await;
    let s = e.snapshot();
    assert_eq!(s.format, Fmt::Apple, "旧的 mp3 读作 apple");
    assert!(s.reformat.is_none());
    assert!(Fmt::parse("mp3").is_err());
    e.complete_onboarding(vec![], out.to_string_lossy().into(), "apple")
        .unwrap();
    assert!(e.snapshot().reformat.is_none());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(exts(&out), ["a.flac"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn preview_counts_only_songs_the_batch_would_convert() {
    let t = Tmp::new("count");
    let out = t.0.join("out");
    flac(&out.join("a.flac"), "能转");
    gen(&out.join("b.wav"), &["-c:a", "pcm_s32le"]);
    gen(&out.join("c.wav"), &["-c:a", "pcm_f32le"]);
    gen(&out.join("d.wav"), &["-c:a", "pcm_s24le"]);
    // 3、8 声道：ALAC 往返保不住声道数与布局，不在待转之列
    gen(&out.join("e.flac"), &["-ac", "3", "-c:a", "flac"]);
    gen(&out.join("f.wav"), &["-ac", "8", "-c:a", "pcm_s16le"]);
    let moves = Moves::default();
    let e = engine(&t.0.join("data"), &moves).await;
    e.complete_onboarding(vec![], out.to_string_lossy().into(), "original")
        .unwrap();
    assert_eq!(e.preview_format("apple").unwrap().count, 2);
    e.set_format("apple").unwrap();
    let r = finished(&e).await;
    assert_eq!((r.total, r.done, r.failed.len()), (2, 2, 0));
    assert_eq!(
        exts(&out),
        ["a.m4a", "b.wav", "c.wav", "d.m4a", "e.flac", "f.wav"]
    );
}

/// 看到批次开始的快照时，settings.json 已是新格式、标着未完成。记录很多（落盘慢）也一样。
#[tokio::test(flavor = "multi_thread")]
async fn batch_start_is_on_disk_before_it_is_broadcast() {
    let t = Tmp::new("order");
    let out = t.0.join("out");
    flac(&out.join("a.flac"), "歌一");
    let data = t.0.join("data");
    std::fs::create_dir_all(&data).unwrap();
    // 50 万条记录：落盘要上百毫秒，比快照推送的 50ms 合并窗口长，先广播后落盘就会露馅
    let ignored: Vec<_> = (0..500_000)
        .map(|i| serde_json::json!({"path": format!("/nowhere/{i}.flac"), "size": i, "mtime": i}))
        .collect();
    std::fs::write(
        data.join("history.json"),
        serde_json::json!({"done": [], "ignored": ignored, "failed": [], "pending": []})
            .to_string(),
    )
    .unwrap();
    let seen: Arc<Mutex<Vec<String>>> = Default::default();
    let (s, path) = (seen.clone(), data.join("settings.json"));
    let e = Engine::new(
        data.clone(),
        Arc::new(NoConv),
        Box::new(move |snap| {
            if snap.reformat.as_ref().is_some_and(|r| !r.finished) {
                s.lock()
                    .unwrap()
                    .push(std::fs::read_to_string(&path).unwrap_or_default());
            }
        }),
    )
    .with_poll_interval(None);
    e.start().await;
    e.complete_onboarding(vec![], out.to_string_lossy().into(), "original")
        .unwrap();
    e.set_format("apple").unwrap();
    finished(&e).await;
    let seen = seen.lock().unwrap();
    assert!(!seen.is_empty(), "没看到批次开始");
    let v: serde_json::Value = serde_json::from_str(&seen[0]).unwrap();
    assert_eq!(
        (&v["format"], &v["reformatting"]),
        (&"apple".into(), &true.into()),
        "{v}"
    );
}
