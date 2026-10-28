//! 引擎行为测试：假 `Converter` 驱动，`tick()` 手动推进轮询（一次 = 2s）。

use musicreveal_app::engine::*;
use reveal_core::{
    CancelToken, ConvertError, ConvertOptions, ConvertOutcome, Platform as CorePlatform, QqStatus,
    Stage, TrackMeta,
};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::AtomicU32;
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "mr-engine-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn dir(&self, name: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 默认：写出 `<输出目录>/<源文件名去扩展名>.<源扩展名>` 并成功。
#[derive(Default)]
struct Fake {
    script: Mutex<HashMap<String, VecDeque<ConvertError>>>,
    status: Mutex<Option<QqStatus>>,
    hold: AtomicBool,
    release: AtomicBool,
    calls: Mutex<Vec<String>>,
}

impl Fake {
    fn fail(&self, name: &str, e: ConvertError) {
        self.script
            .lock()
            .unwrap()
            .entry(name.into())
            .or_default()
            .push_back(e);
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Converter for Fake {
    async fn convert(
        &self,
        input: &Path,
        opts: &ConvertOptions,
        cancel: &CancelToken,
        progress: &mut (dyn FnMut(Stage) + Send),
    ) -> Result<ConvertOutcome, ConvertError> {
        let name = input.file_name().unwrap().to_string_lossy().to_string();
        self.calls.lock().unwrap().push(name.clone());
        progress(Stage::Decrypt);
        while self.hold.load(Ordering::SeqCst) && !self.release.load(Ordering::SeqCst) {
            if cancel.is_cancelled() {
                return Err(ConvertError::Cancelled);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        if let Some(e) = self
            .script
            .lock()
            .unwrap()
            .get_mut(&name)
            .and_then(|q| q.pop_front())
        {
            return Err(e);
        }
        progress(Stage::Metadata);
        let output = opts.output_dir.join(input.file_name().unwrap());
        std::fs::write(&output, b"audio").unwrap();
        Ok(ConvertOutcome {
            output,
            lrc: None,
            meta: TrackMeta {
                title: Some(format!("T-{name}")),
                artists: vec!["歌手".into()],
                album: Some("专辑".into()),
                album_artist: None,
                year: None,
                track: None,
                disc: None,
                cover: None,
                lyrics: None,
                song_id: None,
                platform: CorePlatform::Qq,
            },
            renamed: None,
            quality: "FLAC 16bit/44.1kHz".into(),
            has_cover: false,
            has_lyrics: false,
        })
    }
    fn qq_status(&self) -> QqStatus {
        self.status
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(QqStatus::LoggedOut)
    }
}

struct Rig {
    t: Tmp,
    fake: Arc<Fake>,
    eng: Engine,
    watch: PathBuf,
    out: PathBuf,
}

impl Rig {
    async fn new(tag: &str) -> Self {
        let t = Tmp::new(tag);
        let fake = Arc::new(Fake::default());
        let (watch, out) = (t.dir("watch"), t.dir("out"));
        let eng = engine(&t, &fake).await;
        eng.complete_onboarding(
            vec![watch.to_string_lossy().into()],
            out.to_string_lossy().into(),
            "original",
        )
        .unwrap();
        Rig {
            t,
            fake,
            eng,
            watch,
            out,
        }
    }
    fn job(&self, name: &str) -> Option<Job> {
        self.eng
            .snapshot()
            .jobs
            .into_iter()
            .find(|j| j.file_name == name)
    }
    fn state(&self, name: &str) -> Option<JobState> {
        self.job(name).map(|j| j.state)
    }
    async fn wait(&self, name: &str, s: JobState) -> Job {
        for _ in 0..500 {
            if self.state(name) == Some(s) {
                return self.job(name).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{name} 没等到 {s:?}，当前 {:?}", self.eng.snapshot().jobs);
    }
}

async fn engine(t: &Tmp, fake: &Arc<Fake>) -> Engine {
    let e = Engine::new(t.0.join("data"), fake.clone(), Box::new(|_| {})).with_poll_interval(None);
    e.start().await;
    e
}

fn put(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, data).unwrap();
    p
}

fn append(p: &Path, data: &[u8]) {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(p)
        .unwrap()
        .write_all(data)
        .unwrap();
}

#[tokio::test]
async fn growing_file_stays_downloading_until_two_polls_unchanged() {
    let r = Rig::new("stable").await;
    let p = put(&r.watch, "a.ncm", b"1");
    put(&r.watch, "plain.mp3", b"x");
    put(&r.watch, ".hidden.ncm", b"x");
    r.eng.tick();
    assert_eq!(r.state("a.ncm"), Some(JobState::Downloading));
    append(&p, b"2");
    r.eng.tick();
    assert_eq!(r.state("a.ncm"), Some(JobState::Downloading));
    r.eng.tick();
    let j = r.wait("a.ncm", JobState::Done).await;
    assert_eq!(j.title.as_deref(), Some("T-a.ncm"));
    assert_eq!(j.artists, vec!["歌手".to_string()]);
    assert_eq!(j.platform, Platform::Netease);
    assert!(j.output.is_some());
    assert_eq!(r.eng.snapshot().jobs.len(), 1, "普通音频与隐藏文件不处理");
    assert_eq!(r.fake.calls(), vec!["a.ncm"]);
}

/// 稳定两次轮询：首次看见 + 一次不变。
fn settle(e: &Engine) {
    e.tick();
    e.tick();
}

#[tokio::test]
async fn incomplete_goes_back_to_downloading_and_fails_after_60s_unchanged() {
    let r = Rig::new("incomplete").await;
    r.fake.fail("b.mflac", ConvertError::Incomplete);
    r.fake.fail("b.mflac", ConvertError::Incomplete);
    let p = put(&r.watch, "b.mflac", b"1");
    settle(&r.eng);
    r.wait("b.mflac", JobState::Downloading).await;
    assert_eq!(r.fake.calls().len(), 1);
    // 不变时不重试
    for _ in 0..5 {
        r.eng.tick();
    }
    assert_eq!(r.fake.calls().len(), 1);
    // 文件再变化 → 稳定后重试，又不完整
    append(&p, b"2");
    r.eng.tick();
    r.eng.tick();
    r.wait("b.mflac", JobState::Downloading).await;
    assert_eq!(r.fake.calls().len(), 2);
    // 60s（30 次轮询）不变 → 失败
    for _ in 0..29 {
        r.eng.tick();
    }
    assert_eq!(r.state("b.mflac"), Some(JobState::Downloading));
    r.eng.tick();
    let j = r.job("b.mflac").unwrap();
    assert_eq!(j.state, JobState::Failed);
    assert_eq!(j.error.as_deref(), Some("文件不完整"));
    // 之后文件又变了：回到下载中，稳定后转换成功，仍是同一条任务
    append(&p, b"3");
    r.eng.tick();
    assert_eq!(r.state("b.mflac"), Some(JobState::Downloading));
    r.eng.tick();
    r.wait("b.mflac", JobState::Done).await;
    assert_eq!(r.eng.snapshot().jobs.len(), 1);
}

#[tokio::test]
async fn failures_carry_short_chinese_reasons_and_retry_requeues() {
    let r = Rig::new("fail").await;
    r.fake.fail("u.qmc0", ConvertError::Unsupported("x".into()));
    r.fake.fail("n.ncm", ConvertError::Failed("网络错误：timeout".into()));
    put(&r.watch, "u.qmc0", b"1");
    put(&r.watch, "n.ncm", b"1");
    settle(&r.eng);
    let u = r.wait("u.qmc0", JobState::Failed).await;
    assert_eq!(u.error.as_deref(), Some("不支持的格式"));
    let n = r.wait("n.ncm", JobState::Failed).await;
    assert_eq!(n.error.as_deref(), Some("网络错误"));
    r.eng.retry_job(&n.id);
    let n = r.wait("n.ncm", JobState::Done).await;
    assert_eq!(n.error, None);
    r.eng.remove_job(&u.id);
    assert!(r.job("u.qmc0").is_none());
}

#[tokio::test]
async fn queue_is_serial_with_queue_pos_in_enqueue_order() {
    let r = Rig::new("serial").await;
    r.fake.hold.store(true, Ordering::SeqCst);
    let files: Vec<PathBuf> = ["1.ncm", "2.ncm", "3.ncm"]
        .iter()
        .map(|n| r.t.0.join(n))
        .collect();
    for f in &files {
        std::fs::write(f, b"x").unwrap();
    }
    r.eng
        .add_files(files.iter().map(|f| f.to_string_lossy().into()).collect());
    r.wait("1.ncm", JobState::Decrypt).await;
    let s = r.eng.snapshot();
    assert_eq!(s.jobs[0].file_name, "3.ncm", "新任务在最上面");
    assert_eq!(r.job("2.ncm").unwrap().queue_pos, Some(1));
    assert_eq!(r.job("3.ncm").unwrap().queue_pos, Some(2));
    assert_eq!(r.job("1.ncm").unwrap().queue_pos, None);
    assert_eq!(s.busy, 3);
    assert_eq!(r.fake.calls(), vec!["1.ncm"]);
    r.fake.release.store(true, Ordering::SeqCst);
    r.wait("3.ncm", JobState::Done).await;
    assert_eq!(r.fake.calls(), vec!["1.ncm", "2.ncm", "3.ncm"]);
    assert_eq!(r.eng.snapshot().busy, 0);
}

#[tokio::test]
async fn same_file_again_is_skipped_until_output_is_gone() {
    let r = Rig::new("skip").await;
    let src = r.t.0.join("s.ncm");
    std::fs::write(&src, b"x").unwrap();
    let add = || r.eng.add_files(vec![src.to_string_lossy().into()]);
    add();
    let first = r.wait("s.ncm", JobState::Done).await;
    add();
    for _ in 0..500 {
        if r.eng.snapshot().jobs.iter().any(|j| j.state == JobState::Skipped) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let s = r.eng.snapshot();
    assert_eq!(s.jobs[0].state, JobState::Skipped);
    assert_eq!(s.jobs[0].output, first.output);
    assert_eq!(r.fake.calls().len(), 1);
    // 换格式不算同一条件
    r.eng.set_format("mp3").unwrap();
    add();
    for _ in 0..500 {
        if r.fake.calls().len() == 2 && r.eng.snapshot().jobs[0].state == JobState::Done {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(r.fake.calls().len(), 2);
    r.eng.set_format("original").unwrap();
    std::fs::remove_file(first.output.unwrap()).unwrap();
    add();
    for _ in 0..500 {
        if r.fake.calls().len() == 3 && r.eng.snapshot().jobs[0].state == JobState::Done {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(r.fake.calls().len(), 3, "输出被删后重新转换");
}

#[tokio::test]
async fn output_settings_only_affect_later_jobs_and_dir_is_created() {
    let r = Rig::new("settings").await;
    r.fake.hold.store(true, Ordering::SeqCst);
    let a = put(&r.t.0, "a.ncm", b"x");
    let b = put(&r.t.0, "b.ncm", b"x");
    r.eng.add_files(vec![a.to_string_lossy().into()]);
    r.wait("a.ncm", JobState::Decrypt).await;
    r.eng.add_files(vec![b.to_string_lossy().into()]);
    let out2 = r.t.0.join("new/out");
    r.eng.set_output_dir(&out2.to_string_lossy()).unwrap();
    let c = put(&r.t.0, "c.ncm", b"x");
    r.eng.add_files(vec![c.to_string_lossy().into()]);
    r.fake.release.store(true, Ordering::SeqCst);
    let jb = r.wait("b.ncm", JobState::Done).await;
    let jc = r.wait("c.ncm", JobState::Done).await;
    assert!(Path::new(&jb.output.unwrap()).starts_with(&r.out));
    assert!(Path::new(&jc.output.unwrap()).starts_with(&out2));
    assert_eq!(r.eng.snapshot().output_dir, out2.to_string_lossy());
    assert!(r.eng.set_format("wav").is_err());
}
