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

/// 默认：把五个字节写到 `<输出目录>/<源文件名>` 并成功。
#[derive(Default)]
struct Fake {
    script: Mutex<HashMap<String, VecDeque<ConvertError>>>,
    status: Mutex<Option<QqStatus>>,
    stamp: Mutex<Option<std::time::SystemTime>>,
    cover: Mutex<Option<Vec<u8>>>,
    hold: AtomicBool,
    release: AtomicBool,
    /// 返回脚本里的 Incomplete 之前先给源文件追加一个字节（转换期间文件还在长）。
    grow: AtomicBool,
    /// 不理会取消：留下 .part，5s 后照样成功；future 被丢弃时删掉 .part。
    stubborn: AtomicBool,
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
        if self.stubborn.load(Ordering::SeqCst) {
            struct Part(PathBuf);
            impl Drop for Part {
                fn drop(&mut self) {
                    let _ = std::fs::remove_file(&self.0);
                }
            }
            let part = Part(opts.output_dir.join(format!(".{name}.part")));
            std::fs::write(&part.0, b"half").unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(part);
        }
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
            if e == ConvertError::Incomplete && self.grow.load(Ordering::SeqCst) {
                append(input, b"+");
            }
            return Err(e);
        }
        progress(Stage::Metadata);
        // mgg 的真实产物是 ogg；其余与源同名
        let output = match input.extension().and_then(|e| e.to_str()) {
            Some("mgg") => opts
                .output_dir
                .join(input.with_extension("ogg").file_name().unwrap()),
            _ => opts.output_dir.join(input.file_name().unwrap()),
        };
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
                cover: self.cover.lock().unwrap().clone(),
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
    fn qq_stamp(&self) -> Option<std::time::SystemTime> {
        *self.stamp.lock().unwrap()
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

/// 内容带上文件名：不同文件内容不同，免得被当成同一首歌跳过。
fn put(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, [data, name.as_bytes()].concat()).unwrap();
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
    put(&r.watch, "notes.txt", b"x");
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
    assert_eq!(r.eng.snapshot().jobs.len(), 1, "非音频与隐藏文件不处理");
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
    r.fake
        .fail("n.ncm", ConvertError::Failed("网络错误：timeout".into()));
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
        std::fs::write(f, f.to_string_lossy().as_bytes()).unwrap();
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
        if r.eng
            .snapshot()
            .jobs
            .iter()
            .any(|j| j.state == JobState::Skipped)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let s = r.eng.snapshot();
    assert_eq!(s.jobs[0].state, JobState::Skipped);
    assert_eq!(s.jobs[0].output, first.output);
    assert_eq!(r.fake.calls().len(), 1);
    // 换格式不算同一条件
    r.eng.set_format("apple").unwrap();
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

fn notices(e: &Engine, kind: NoticeKind) -> Vec<Notice> {
    e.snapshot()
        .notices
        .into_iter()
        .filter(|n| n.kind == kind)
        .collect()
}

#[tokio::test]
async fn new_folder_asks_about_existing_songs_convert_branch() {
    let r = Rig::new("existing").await;
    let lib = r.t.dir("lib");
    put(&lib, "old1.ncm", b"x");
    put(&lib, "old2.mflac", b"x");
    put(&lib, "cover.jpg", b"x");
    let res = r.eng.add_watch_folder(&lib.to_string_lossy()).unwrap();
    assert_eq!(res.existing, 2);
    let n = notices(&r.eng, NoticeKind::ExistingSongs);
    assert_eq!(n.len(), 1);
    assert_eq!(n[0].count, 2);
    assert_eq!(n[0].folder.as_deref(), Some(&*lib.to_string_lossy()));
    // 用户选择前不处理旧歌，但之后的新文件照常处理
    put(&lib, "new.ncm", b"x");
    settle(&r.eng);
    r.wait("new.ncm", JobState::Done).await;
    assert!(r.job("old1.ncm").is_none());
    r.eng.convert_existing(&lib.to_string_lossy());
    r.wait("old1.ncm", JobState::Done).await;
    r.wait("old2.mflac", JobState::Done).await;
    assert!(notices(&r.eng, NoticeKind::ExistingSongs).is_empty());
    assert_eq!(r.fake.calls().len(), 3);
}

#[tokio::test]
async fn ignored_existing_songs_are_never_asked_or_caught_up_again() {
    let r = Rig::new("ignore").await;
    let lib = r.t.dir("lib");
    put(&lib, "old.ncm", b"x");
    assert_eq!(
        r.eng
            .add_watch_folder(&lib.to_string_lossy())
            .unwrap()
            .existing,
        1
    );
    r.eng.ignore_existing(&lib.to_string_lossy());
    assert!(r.eng.snapshot().notices.is_empty());
    settle(&r.eng);
    assert!(r.job("old.ncm").is_none());
    // 移除再加回：不再提示
    r.eng.remove_watch_folder(&lib.to_string_lossy());
    assert_eq!(
        r.eng
            .add_watch_folder(&lib.to_string_lossy())
            .unwrap()
            .existing,
        0
    );
    // 重启：不补转
    r.eng.shutdown().await;
    let e2 = engine(&r.t, &r.fake).await;
    assert!(e2.snapshot().jobs.is_empty());
    assert!(e2.snapshot().notices.is_empty());
    assert!(r.fake.calls().is_empty());
}

#[tokio::test]
async fn onboarding_folders_follow_the_existing_songs_rule() {
    let t = Tmp::new("onboard");
    let fake = Arc::new(Fake::default());
    let (a, out) = (t.dir("a"), t.dir("out"));
    put(&a, "x.ncm", b"x");
    let e = engine(&t, &fake).await;
    assert!(!e.snapshot().onboarded);
    e.complete_onboarding(
        vec![a.to_string_lossy().into()],
        out.to_string_lossy().into(),
        "apple",
    )
    .unwrap();
    let s = e.snapshot();
    assert!(s.onboarded);
    assert_eq!(s.format, Fmt::Apple);
    assert_eq!(s.folders.len(), 1);
    assert_eq!(notices(&e, NoticeKind::ExistingSongs)[0].count, 1);
}

#[tokio::test]
async fn restart_catches_up_files_missed_while_closed() {
    let r = Rig::new("catchup").await;
    put(&r.watch, "a.ncm", b"x");
    settle(&r.eng);
    r.wait("a.ncm", JobState::Done).await;
    r.eng.shutdown().await;
    // 关窗期间下载了两首
    put(&r.watch, "b.ncm", b"x");
    let sub = r.watch.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    put(&sub, "c.mflac", b"x");
    let e2 = engine(&r.t, &r.fake).await;
    let n = notices(&e2, NoticeKind::CaughtUp);
    assert_eq!(n.len(), 1);
    assert_eq!(n[0].count, 2);
    for _ in 0..500 {
        if r.fake.calls().len() == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut calls = r.fake.calls();
    calls.sort();
    assert_eq!(calls, vec!["a.ncm", "b.ncm", "c.mflac"]);
    e2.dismiss_notice(&n[0].id);
    assert!(e2.snapshot().notices.is_empty());
    // 设置也恢复了
    assert!(e2.snapshot().onboarded);
    assert_eq!(e2.snapshot().output_dir, r.out.to_string_lossy());
}

#[tokio::test]
async fn shutdown_cancels_running_job_without_recording_it() {
    let r = Rig::new("shutdown").await;
    r.fake.hold.store(true, Ordering::SeqCst);
    put(&r.watch, "a.ncm", b"x");
    settle(&r.eng);
    r.wait("a.ncm", JobState::Decrypt).await;
    let t0 = std::time::Instant::now();
    r.eng.shutdown().await;
    assert!(t0.elapsed() < Duration::from_secs(3));
    assert!(std::fs::read_dir(&r.out).unwrap().next().is_none());
    // 下次启动由补转接上
    r.fake.hold.store(false, Ordering::SeqCst);
    let e2 = engine(&r.t, &r.fake).await;
    assert_eq!(notices(&e2, NoticeKind::CaughtUp)[0].count, 1);
    for _ in 0..500 {
        if r.fake.calls().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(r.fake.calls(), vec!["a.ncm", "a.ncm"]);
}

#[tokio::test]
async fn needs_login_waits_and_resumes_when_qq_logs_in() {
    let r = Rig::new("login").await;
    assert_eq!(r.eng.snapshot().qq.state, QqState::LoggedOut);
    r.fake.fail("q.mflac", ConvertError::NeedsLogin);
    put(&r.watch, "q.mflac", b"x");
    settle(&r.eng);
    let j = r.wait("q.mflac", JobState::WaitLogin).await;
    assert_eq!(j.error, None);
    assert_eq!(r.eng.snapshot().waiting_login, 1);
    *r.fake.status.lock().unwrap() = Some(QqStatus::LoggedIn {
        masked_uin: "12****89".into(),
    });
    // 每 10s（5 次轮询）刷新一次
    for _ in 0..5 {
        r.eng.tick();
    }
    let s = r.eng.snapshot();
    assert_eq!(s.qq.state, QqState::LoggedIn);
    assert_eq!(s.qq.account.as_deref(), Some("12****89"));
    r.wait("q.mflac", JobState::Done).await;
    assert_eq!(r.fake.calls().len(), 2);
}

#[tokio::test]
async fn logged_in_but_rejected_shows_expired() {
    let r = Rig::new("expired").await;
    *r.fake.status.lock().unwrap() = Some(QqStatus::LoggedIn {
        masked_uin: "12****89".into(),
    });
    for _ in 0..5 {
        r.eng.tick();
    }
    assert_eq!(r.eng.snapshot().qq.state, QqState::LoggedIn);
    r.fake.fail("q.mflac", ConvertError::NeedsLogin);
    put(&r.watch, "q.mflac", b"x");
    settle(&r.eng);
    r.wait("q.mflac", JobState::WaitLogin).await;
    assert_eq!(r.eng.snapshot().qq.state, QqState::Expired);
    // 重新登录：plist 的 mtime 变了，下一次轮询就重新入队
    *r.fake.stamp.lock().unwrap() = Some(std::time::SystemTime::now());
    r.eng.tick();
    r.wait("q.mflac", JobState::Done).await;
    assert_eq!(r.eng.snapshot().qq.state, QqState::LoggedIn);
}

#[tokio::test]
async fn deleted_watch_folder_is_inaccessible_and_recovers() {
    let r = Rig::new("access").await;
    let other = r.t.dir("other");
    r.eng.add_watch_folder(&other.to_string_lossy()).unwrap();
    std::fs::remove_dir_all(&r.watch).unwrap();
    r.eng.tick();
    let s = r.eng.snapshot();
    let f = |p: &Path| {
        s.folders
            .iter()
            .find(|f| f.path == p.to_string_lossy())
            .unwrap()
            .clone()
    };
    assert!(!f(&r.watch).accessible);
    assert!(f(&r.watch).reason.is_some());
    assert!(f(&other).accessible);
    put(&other, "o.ncm", b"x");
    settle(&r.eng);
    r.wait("o.ncm", JobState::Done).await;
    std::fs::create_dir_all(&r.watch).unwrap();
    put(&r.watch, "w.ncm", b"x");
    settle(&r.eng);
    assert!(r.eng.snapshot().folders.iter().all(|f| f.accessible));
    r.wait("w.ncm", JobState::Done).await;
}

#[tokio::test]
async fn output_dir_inside_watch_folder_is_not_rescanned() {
    let r = Rig::new("inside").await;
    let out = r.watch.join("converted");
    r.eng.set_output_dir(&out.to_string_lossy()).unwrap();
    put(&r.watch, "a.ncm", b"x");
    settle(&r.eng);
    r.wait("a.ncm", JobState::Done).await;
    // 假转换器的输出与源同扩展名，若被扫描到会变成新任务
    assert!(out.join("a.ncm").exists());
    for _ in 0..4 {
        r.eng.tick();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(r.eng.snapshot().jobs.len(), 1);
    assert_eq!(r.fake.calls().len(), 1);
}

/// 输出文件夹永不当输入：否则普通音频转完又被扫到，无限复制出「(2)」。
#[tokio::test]
async fn watch_folder_equal_to_or_inside_output_dir_yields_nothing() {
    let r = Rig::new("loop").await;
    let inner = r.out.join("sub");
    std::fs::create_dir_all(&inner).unwrap();
    put(&r.out, "a.mp3", b"x");
    put(&inner, "b.mp3", b"x");
    for f in [&r.out, &inner] {
        assert_eq!(
            r.eng
                .add_watch_folder(&f.to_string_lossy())
                .unwrap()
                .existing,
            0
        );
    }
    for _ in 0..4 {
        r.eng.tick();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let s = r.eng.snapshot();
    assert!(s.jobs.is_empty(), "{:?}", s.jobs);
    assert!(s.notices.is_empty());
    assert!(s.folders.iter().all(|f| f.accessible), "{:?}", s.folders);
    assert!(r.fake.calls().is_empty());
}

/// 拖入或「添加文件…」：输出文件夹里（含子目录）的文件不当输入，否则会转出「(2)」副本。
#[tokio::test]
async fn added_files_inside_output_dir_are_ignored() {
    let r = Rig::new("addout").await;
    let sub = r.out.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let a = put(&r.out, "a.mp3", b"x");
    let b = put(&sub, "b.ncm", b"x");
    r.eng
        .add_files(vec![a.to_string_lossy().into(), b.to_string_lossy().into()]);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        r.eng.snapshot().jobs.is_empty(),
        "{:?}",
        r.eng.snapshot().jobs
    );
    assert!(r.fake.calls().is_empty());
}

/// 输出文件夹的别名：符号链、大小写不同（APFS 默认不分大小写）、末尾斜杠。
fn out_aliases(r: &Rig) -> Vec<PathBuf> {
    let link = r.t.0.join("alias");
    std::os::unix::fs::symlink(&r.out, &link).unwrap();
    let mut v = vec![link, PathBuf::from(format!("{}/", r.out.display()))];
    let upper = r.t.0.join("OUT");
    if upper.exists() {
        v.push(upper);
    } else {
        eprintln!("文件系统区分大小写，跳过大小写别名");
    }
    v
}

#[tokio::test]
async fn output_dir_aliases_are_never_input() {
    let r = Rig::new("alias").await;
    let sub = r.out.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    put(&r.out, "a.mp3", b"x");
    put(&sub, "b.ncm", b"x");
    for alias in out_aliases(&r) {
        // 单文件添加、拖入文件夹、监听文件夹，都经别名指向输出文件夹
        r.eng.add_files(vec![
            alias.join("a.mp3").to_string_lossy().into(),
            alias.join("sub/b.ncm").to_string_lossy().into(),
            alias.join("sub").to_string_lossy().into(),
        ]);
        for f in [alias.clone(), alias.join("sub")] {
            r.eng.add_watch_folder(&f.to_string_lossy()).unwrap();
        }
        settle(&r.eng);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let s = r.eng.snapshot();
        assert!(
            s.jobs.is_empty() && s.notices.is_empty(),
            "{alias:?}: {:?}",
            s.jobs
        );
        for f in &s.folders {
            r.eng.remove_watch_folder(&f.path);
        }
    }
    assert!(r.fake.calls().is_empty());
}

/// 输出文件夹外的符号链指向输出文件夹里的子文件夹：经它监听或添加都不算输入。
#[tokio::test]
async fn link_into_a_subfolder_of_output_dir_is_never_input() {
    let r = Rig::new("linkin").await;
    let album = r.out.join("album");
    std::fs::create_dir_all(&album).unwrap();
    put(&album, "existing.mp3", b"x");
    let link = r.t.0.join("album-link");
    std::os::unix::fs::symlink(&album, &link).unwrap();
    r.eng
        .add_files(vec![link.join("existing.mp3").to_string_lossy().into()]);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        r.eng.snapshot().jobs.is_empty(),
        "添加文件：{:?}",
        r.eng.snapshot().jobs
    );
    assert_eq!(
        r.eng
            .add_watch_folder(&link.to_string_lossy())
            .unwrap()
            .existing,
        0
    );
    settle(&r.eng);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let s = r.eng.snapshot();
    assert!(s.jobs.is_empty() && s.notices.is_empty(), "{:?}", s.jobs);
    assert!(r.fake.calls().is_empty());
}

/// 输出文件夹里的符号链指向外面的文件：链接本身在输出文件夹内，经它添加也不算输入。
#[tokio::test]
async fn link_inside_output_dir_to_outside_file_is_never_input() {
    let r = Rig::new("linkout").await;
    let ext = r.t.dir("external");
    let a = put(&ext, "a.ogg", b"x");
    let link = r.out.join("link.ogg");
    std::os::unix::fs::symlink(&a, &link).unwrap();
    r.eng.add_files(vec![link.to_string_lossy().into()]);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        r.eng.snapshot().jobs.is_empty(),
        "{:?}",
        r.eng.snapshot().jobs
    );
    // 外面的原文件本身照常可添加
    r.eng.add_files(vec![a.to_string_lossy().into()]);
    r.wait("a.ogg", JobState::Done).await;
}

/// 输出文件夹设成监听文件夹里某个子文件夹的别名拼写：递归扫描照样跳过它。
#[tokio::test]
async fn output_dir_spelled_differently_inside_watch_folder_is_skipped() {
    let r = Rig::new("aliasin").await;
    let conv = r.watch.join("conv");
    std::fs::create_dir_all(&conv).unwrap();
    put(&conv, "a.mp3", b"x");
    let link = r.t.0.join("conv-link");
    std::os::unix::fs::symlink(&conv, &link).unwrap();
    let mut spellings = vec![link];
    if r.watch.join("CONV").exists() {
        spellings.push(r.watch.join("CONV"));
    }
    for out in spellings {
        r.eng.set_output_dir(&out.to_string_lossy()).unwrap();
        settle(&r.eng);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(r.eng.snapshot().jobs.is_empty(), "{out:?}");
    }
    assert!(r.fake.calls().is_empty());
}

/// 输出文件夹还不存在：按字面判定，监听它（或它里面）的文件夹照样是空的、可访问。
#[tokio::test]
async fn missing_output_dir_falls_back_to_literal_paths() {
    let r = Rig::new("aliasmissing").await;
    let out = r.t.0.join("later");
    r.eng.set_output_dir(&out.to_string_lossy()).unwrap();
    for f in [out.clone(), out.join("sub")] {
        assert_eq!(
            r.eng
                .add_watch_folder(&f.to_string_lossy())
                .unwrap()
                .existing,
            0
        );
    }
    settle(&r.eng);
    let s = r.eng.snapshot();
    assert!(s.jobs.is_empty());
    assert!(s.folders.iter().all(|f| f.accessible), "{:?}", s.folders);
}

#[tokio::test]
async fn corrupt_settings_and_history_do_not_crash() {
    let t = Tmp::new("corrupt");
    let fake = Arc::new(Fake::default());
    let data = t.dir("data");
    std::fs::write(data.join("settings.json"), b"{not json").unwrap();
    std::fs::write(data.join("history.json"), b"\0\0garbage").unwrap();
    let e = engine(&t, &fake).await;
    let s = e.snapshot();
    assert!(!s.onboarded);
    assert!(s.output_dir.ends_with("Music/musicReveal"));
    assert_eq!(s.format, Fmt::Original);
    assert!(data.join("settings.json.bak").exists());
    assert!(data.join("history.json.bak").exists());
    // 之后照常保存
    let out = t.dir("out");
    e.complete_onboarding(vec![], out.to_string_lossy().into(), "apple")
        .unwrap();
    let text = std::fs::read_to_string(data.join("settings.json")).unwrap();
    assert!(text.contains("\"apple\""));
}

#[tokio::test]
async fn too_many_entries_marks_folder_inaccessible() {
    let r = Rig::new("many").await;
    for i in 0..5001 {
        std::fs::write(r.watch.join(format!("{i}.txt")), b"").unwrap();
    }
    r.eng.tick();
    let f = &r.eng.snapshot().folders[0];
    assert!(!f.accessible);
    assert!(f.reason.as_deref().unwrap().contains("5000"));
}

#[tokio::test]
async fn cover_becomes_small_thumbnail_url() {
    let r = Rig::new("cover").await;
    let png = r.t.0.join("big.png");
    let ok = std::process::Command::new("/opt/homebrew/bin/ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=red:s=800x600",
        ])
        .args(["-frames:v", "1"])
        .arg(&png)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("跳过：没有 ffmpeg 生成测试图");
        return;
    }
    *r.fake.cover.lock().unwrap() = Some(std::fs::read(&png).unwrap());
    let src = put(&r.t.0, "c.ncm", b"x");
    r.eng.add_files(vec![src.to_string_lossy().into()]);
    r.wait("c.ncm", JobState::Done).await;
    let mut url = None;
    for _ in 0..300 {
        url = r.job("c.ncm").unwrap().cover_url;
        if url.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let url = url.expect("应有 coverUrl");
    let enc = url.strip_prefix("asset://localhost/").expect(&url);
    assert!(!enc.contains('/'), "路径整体编码：{url}");
    let thumb = std::fs::read_dir(r.t.0.join("data/covers"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let out = std::process::Command::new("sips")
        .args(["-g", "pixelWidth", "-g", "pixelHeight"])
        .arg(&thumb)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let dims: Vec<u32> = text
        .lines()
        .filter_map(|l| l.split(':').nth(1)?.trim().parse().ok())
        .collect();
    assert_eq!(dims, vec![256, 192], "{text}");
}

#[tokio::test]
async fn on_change_receives_snapshots() {
    let t = Tmp::new("emit");
    let fake = Arc::new(Fake::default());
    let got = Arc::new(Mutex::new(vec![]));
    let g = got.clone();
    let e = Engine::new(
        t.0.join("data"),
        fake.clone(),
        Box::new(move |s: Snapshot| g.lock().unwrap().push(s)),
    )
    .with_poll_interval(None);
    e.start().await;
    e.set_format("apple").unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let got = got.lock().unwrap();
    assert!(!got.is_empty());
    assert_eq!(got.last().unwrap().format, Fmt::Apple);
    let json = serde_json::to_value(got.last().unwrap()).unwrap();
    assert!(json.get("outputDir").is_some() && json.get("waitingLogin").is_some());
}

#[tokio::test]
async fn unanswered_existing_songs_question_survives_restart() {
    let r = Rig::new("pending").await;
    let lib = r.t.dir("lib");
    put(&lib, "old.ncm", b"x");
    r.eng.add_watch_folder(&lib.to_string_lossy()).unwrap();
    r.eng.shutdown().await;
    let e2 = engine(&r.t, &r.fake).await;
    assert!(notices(&e2, NoticeKind::CaughtUp).is_empty());
    let n = notices(&e2, NoticeKind::ExistingSongs);
    assert_eq!(n.len(), 1);
    assert_eq!(n[0].count, 1);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(r.fake.calls().is_empty());
    e2.convert_existing(&lib.to_string_lossy());
    for _ in 0..500 {
        if r.fake.calls().len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(r.fake.calls(), vec!["old.ncm"]);
}

#[tokio::test]
async fn vanished_download_is_dropped_from_the_list() {
    let r = Rig::new("vanish").await;
    let p = put(&r.watch, "tmp.ncm", b"x");
    r.eng.tick();
    assert_eq!(r.state("tmp.ncm"), Some(JobState::Downloading));
    std::fs::remove_file(&p).unwrap();
    r.eng.tick();
    assert!(r.job("tmp.ncm").is_none());
    assert!(r.fake.calls().is_empty());
}

fn history_text(t: &Tmp) -> String {
    std::fs::read_to_string(t.0.join("data/history.json")).unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_saves_never_leave_broken_or_stale_files() {
    let r = Rig::new("concurrent").await;
    let data = r.t.0.join("data");
    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let (data, stop) = (data.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut bad = vec![];
            while !stop.load(Ordering::SeqCst) {
                for f in ["settings.json", "history.json"] {
                    if let Ok(b) = std::fs::read(data.join(f)) {
                        if serde_json::from_slice::<serde_json::Value>(&b).is_err() {
                            bad.push(f);
                        }
                    }
                }
            }
            bad
        })
    };
    let mut hs = vec![];
    for i in 0..8 {
        let e = r.eng.clone();
        let lib = r.t.dir(&format!("lib{i}"));
        put(&lib, "old.ncm", b"x");
        hs.push(tokio::task::spawn_blocking(move || {
            let lib = lib.to_string_lossy().to_string();
            for k in 0..40 {
                e.set_format(if (i + k) % 2 == 0 {
                    "apple"
                } else {
                    "original"
                })
                .unwrap();
                e.add_watch_folder(&lib).unwrap();
                e.remove_watch_folder(&lib);
            }
        }));
    }
    for h in hs {
        h.await.unwrap();
    }
    stop.store(true, Ordering::SeqCst);
    assert_eq!(reader.join().unwrap(), Vec::<&str>::new(), "读到坏 JSON");
    let leftovers: Vec<_> = std::fs::read_dir(&data)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(data.join("settings.json")).unwrap()).unwrap();
    let fmt = serde_json::to_value(r.eng.snapshot().format).unwrap();
    assert_eq!(saved["format"], fmt, "落盘的不是最新设置");
}

#[tokio::test]
async fn queued_file_that_changes_goes_back_to_downloading() {
    let r = Rig::new("requeue").await;
    r.fake.hold.store(true, Ordering::SeqCst);
    let a = put(&r.t.0, "a.ncm", b"x");
    r.eng.add_files(vec![a.to_string_lossy().into()]);
    r.wait("a.ncm", JobState::Decrypt).await;
    let b = put(&r.watch, "b.ncm", b"x");
    settle(&r.eng);
    assert_eq!(r.state("b.ncm"), Some(JobState::Queued));
    // 排队期间又在变：退回下载中，重新判稳定
    append(&b, b"2");
    r.eng.tick();
    assert_eq!(r.state("b.ncm"), Some(JobState::Downloading));
    r.eng.tick();
    assert_eq!(r.state("b.ncm"), Some(JobState::Queued));
    // 轮到它之前又变了一次（没等到下一次轮询）：开转前再核一次
    append(&b, b"3");
    r.fake.release.store(true, Ordering::SeqCst);
    r.wait("b.ncm", JobState::Downloading).await;
    assert_eq!(r.fake.calls(), vec!["a.ncm"]);
    settle(&r.eng);
    r.wait("b.ncm", JobState::Done).await;
    assert_eq!(r.fake.calls(), vec!["a.ncm", "b.ncm"]);
}

#[tokio::test]
async fn existing_song_waiting_for_an_answer_is_untouched_even_if_it_changes() {
    let r = Rig::new("held").await;
    let lib = r.t.dir("lib");
    let old = put(&lib, "old.ncm", b"x");
    assert_eq!(
        r.eng
            .add_watch_folder(&lib.to_string_lossy())
            .unwrap()
            .existing,
        1
    );
    append(&old, b"2");
    for _ in 0..3 {
        r.eng.tick();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(r.job("old.ncm").is_none());
    assert!(r.fake.calls().is_empty());
    r.eng.convert_existing(&lib.to_string_lossy());
    settle(&r.eng);
    r.wait("old.ncm", JobState::Done).await;
    assert_eq!(r.fake.calls(), vec!["old.ncm"]);
}

#[tokio::test]
async fn incomplete_baseline_is_the_stat_when_conversion_started() {
    let r = Rig::new("baseline").await;
    r.fake.grow.store(true, Ordering::SeqCst);
    r.fake.fail("g.mflac", ConvertError::Incomplete);
    put(&r.watch, "g.mflac", b"1");
    settle(&r.eng);
    r.wait("g.mflac", JobState::Downloading).await;
    assert_eq!(r.fake.calls().len(), 1);
    // 转换期间文件长了：这算「再变化」，稳定后应重试
    settle(&r.eng);
    r.wait("g.mflac", JobState::Done).await;
    assert_eq!(r.fake.calls().len(), 2);
}

#[tokio::test]
async fn restart_asks_only_about_the_original_existing_songs() {
    let r = Rig::new("pendingset").await;
    let lib = r.t.dir("lib");
    put(&lib, "old.ncm", b"x");
    r.eng.add_watch_folder(&lib.to_string_lossy()).unwrap();
    r.eng.shutdown().await;
    // 关窗期间新下载的
    put(&lib, "new.ncm", b"x");
    let e2 = engine(&r.t, &r.fake).await;
    assert_eq!(notices(&e2, NoticeKind::ExistingSongs)[0].count, 1);
    assert_eq!(notices(&e2, NoticeKind::CaughtUp)[0].count, 1);
    for _ in 0..500 {
        if !r.fake.calls().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(r.fake.calls(), vec!["new.ncm"]);
}

#[tokio::test]
async fn same_source_is_not_listed_twice_while_unfinished() {
    let r = Rig::new("dedupe").await;
    r.fake.hold.store(true, Ordering::SeqCst);
    let a = put(&r.t.0, "a.ncm", b"x");
    let b = put(&r.t.0, "b.ncm", b"x");
    let w = put(&r.watch, "w.ncm", b"x");
    let add = |p: &Path| r.eng.add_files(vec![p.to_string_lossy().into()]);
    add(&a);
    r.wait("a.ncm", JobState::Decrypt).await;
    add(&a);
    add(&b);
    add(&b);
    add(&w);
    settle(&r.eng);
    let names: Vec<String> = r
        .eng
        .snapshot()
        .jobs
        .into_iter()
        .map(|j| j.file_name)
        .collect();
    assert_eq!(names, vec!["w.ncm", "b.ncm", "a.ncm"]);
    r.fake.release.store(true, Ordering::SeqCst);
    r.wait("w.ncm", JobState::Done).await;
    // 失败的再拖进来：重新入队，不另建
    r.fake.fail("c.ncm", ConvertError::Failed("坏了".into()));
    let c = put(&r.t.0, "c.ncm", b"x");
    add(&c);
    r.wait("c.ncm", JobState::Failed).await;
    add(&c);
    r.wait("c.ncm", JobState::Done).await;
    assert_eq!(r.eng.snapshot().jobs.len(), 4);
}

#[tokio::test(start_paused = true)]
async fn shutdown_drops_a_conversion_that_ignores_cancel() {
    let r = Rig::new("stubborn").await;
    r.fake.stubborn.store(true, Ordering::SeqCst);
    put(&r.watch, "s.ncm", b"x");
    settle(&r.eng);
    r.wait("s.ncm", JobState::Decrypt).await;
    assert!(r.out.join(".s.ncm.part").exists());
    r.eng.shutdown().await;
    assert!(
        !r.out.join(".s.ncm.part").exists(),
        "shutdown 返回时半成品还在"
    );
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert!(!r.out.join("s.ncm").exists());
    assert!(
        !history_text(&r.t).contains("s.ncm"),
        "shutdown 之后写了记录"
    );
}

#[tokio::test]
async fn dropped_folder_follows_the_watch_scan_rules() {
    let r = Rig::new("dropdir").await;
    r.fake.hold.store(true, Ordering::SeqCst);
    let big = r.t.dir("big");
    for i in 0..5001 {
        std::fs::write(big.join(format!("{i}.ncm")), b"x").unwrap();
    }
    r.eng.add_files(vec![big.to_string_lossy().into()]);
    assert_eq!(
        r.eng.snapshot().jobs.len(),
        5000,
        "超过 5000 个条目只取前 5000"
    );
    // 输出目录本身拖进来不处理
    put(&r.out, "o.ncm", b"x");
    r.eng.add_files(vec![r.out.to_string_lossy().into()]);
    assert_eq!(r.eng.snapshot().jobs.len(), 5000);
    r.eng.shutdown().await;
}

#[tokio::test]
async fn history_keeps_one_entry_per_source_and_format() {
    let r = Rig::new("histdedupe").await;
    let a = put(&r.t.0, "a.ncm", b"x");
    let add = || r.eng.add_files(vec![a.to_string_lossy().into()]);
    add();
    r.wait("a.ncm", JobState::Done).await;
    append(&a, b"2");
    add();
    for _ in 0..500 {
        if r.fake.calls().len() == 2 && r.eng.snapshot().jobs[0].state == JobState::Done {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(r.fake.calls().len(), 2);
    let h: serde_json::Value = serde_json::from_str(&history_text(&r.t)).unwrap();
    assert_eq!(h["done"].as_array().unwrap().len(), 1, "{h}");
}

#[tokio::test]
async fn start_is_fast_with_5000_candidates_and_50000_history() {
    let t = Tmp::new("scale");
    let fake = Arc::new(Fake::default());
    let (watch, out, data) = (t.dir("watch"), t.dir("out"), t.dir("data"));
    let mut done = vec![];
    for i in 0..5000 {
        let p = put(&watch, &format!("{i}.ncm"), b"x");
        let m = std::fs::metadata(&p).unwrap();
        let mtime = m
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        // 一半转过，一半是关窗期间的新歌
        if i % 2 == 0 {
            done.push(
                serde_json::json!({"path": p, "size": m.len(), "mtime": mtime,
                "format": "original", "output": out.join(format!("{i}.ncm"))}),
            );
        }
    }
    for i in 0..47500 {
        done.push(
            serde_json::json!({"path": format!("/gone/{i}.ncm"), "size": 1, "mtime": 1,
            "format": "original", "output": format!("/gone/{i}.flac")}),
        );
    }
    std::fs::write(
        data.join("history.json"),
        serde_json::to_vec(&serde_json::json!({ "done": done })).unwrap(),
    )
    .unwrap();
    std::fs::write(
        data.join("settings.json"),
        serde_json::to_vec(&serde_json::json!({"onboarded": true, "folders": [watch],
            "outputDir": out, "format": "original"}))
        .unwrap(),
    )
    .unwrap();
    fake.hold.store(true, Ordering::SeqCst);
    let e = Engine::new(data, fake.clone(), Box::new(|_| {})).with_poll_interval(None);
    let t0 = std::time::Instant::now();
    e.start().await;
    let took = t0.elapsed();
    eprintln!("start 用时 {took:?}");
    let _ = e.snapshot();
    assert_eq!(notices(&e, NoticeKind::CaughtUp)[0].count, 2500);
    assert!(took < Duration::from_secs(1), "start 用了 {took:?}");
    e.shutdown().await;
}

#[tokio::test]
async fn song_stays_held_while_any_overlapping_question_is_open() {
    let r = Rig::new("overlap").await;
    let lib = r.t.dir("lib");
    let sub = lib.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let old = put(&sub, "old.ncm", b"x");
    let (lib_s, sub_s) = (lib.to_string_lossy(), sub.to_string_lossy());
    assert_eq!(r.eng.add_watch_folder(&lib_s).unwrap().existing, 1);
    assert_eq!(r.eng.add_watch_folder(&sub_s).unwrap().existing, 1);
    r.eng.ignore_existing(&lib_s);
    append(&old, b"2");
    for _ in 0..3 {
        r.eng.tick();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(r.job("old.ncm").is_none());
    assert!(r.fake.calls().is_empty());
    // 最后一次明确选择为准：子目录那张选「全部转换」就转
    r.eng.convert_existing(&sub_s);
    settle(&r.eng);
    r.wait("old.ncm", JobState::Done).await;
    assert_eq!(r.fake.calls(), vec!["old.ncm"]);
}

#[tokio::test]
async fn ignored_song_that_changed_while_asked_is_not_caught_up_after_restart() {
    let r = Rig::new("ignorechanged").await;
    let lib = r.t.dir("lib");
    let old = put(&lib, "old.ncm", b"x");
    let lib_s = lib.to_string_lossy();
    assert_eq!(r.eng.add_watch_folder(&lib_s).unwrap().existing, 1);
    append(&old, b"2");
    r.eng.tick();
    r.eng.ignore_existing(&lib_s);
    r.eng.shutdown().await;
    let e2 = engine(&r.t, &r.fake).await;
    assert!(notices(&e2, NoticeKind::CaughtUp).is_empty());
    assert!(notices(&e2, NoticeKind::ExistingSongs).is_empty());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(r.fake.calls().is_empty());
}

#[tokio::test]
async fn open_overlapping_question_survives_restart() {
    let r = Rig::new("overlaprestart").await;
    let lib = r.t.dir("lib");
    let sub = lib.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    put(&sub, "old.ncm", b"x");
    r.eng.add_watch_folder(&lib.to_string_lossy()).unwrap();
    r.eng.add_watch_folder(&sub.to_string_lossy()).unwrap();
    r.eng.ignore_existing(&lib.to_string_lossy());
    r.eng.shutdown().await;
    let e2 = engine(&r.t, &r.fake).await;
    let asked = notices(&e2, NoticeKind::ExistingSongs);
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].count, 1);
}

#[tokio::test]
async fn same_content_under_another_name_is_skipped() {
    let r = Rig::new("dupe").await;
    std::fs::write(r.watch.join("a.ncm"), b"same bytes").unwrap();
    settle(&r.eng);
    let a = r.wait("a.ncm", JobState::Done).await;
    std::fs::write(r.watch.join("b.ncm"), b"same bytes").unwrap();
    settle(&r.eng);
    let b = r.wait("b.ncm", JobState::Skipped).await;
    assert_eq!(b.output, a.output);
    assert_eq!(r.fake.calls(), vec!["a.ncm"]);
    assert_eq!(std::fs::read_dir(&r.out).unwrap().count(), 1);
    // 不同内容照常转换
    put(&r.watch, "c.ncm", b"other bytes");
    settle(&r.eng);
    r.wait("c.ncm", JobState::Done).await;
    // b 已记入记录：重启不补转
    r.eng.shutdown().await;
    let e2 = engine(&r.t, &r.fake).await;
    assert!(notices(&e2, NoticeKind::CaughtUp).is_empty());
    assert_eq!(r.fake.calls(), vec!["a.ncm", "c.ncm"]);
}

#[tokio::test]
async fn failed_file_is_not_caught_up_again_until_it_changes() {
    let r = Rig::new("failrec").await;
    r.fake.fail("bad.ncm", ConvertError::Corrupt("x".into()));
    r.fake.fail("inc.ncm", ConvertError::Incomplete);
    r.fake.fail(
        "net.mflac",
        ConvertError::Failed("网络错误：timeout".into()),
    );
    let bad = put(&r.watch, "bad.ncm", b"x");
    put(&r.watch, "inc.ncm", b"x");
    put(&r.watch, "net.mflac", b"x");
    settle(&r.eng);
    let j = r.wait("bad.ncm", JobState::Failed).await;
    assert_eq!(j.error.as_deref(), Some("文件已损坏"));
    r.wait("inc.ncm", JobState::Downloading).await;
    r.wait("net.mflac", JobState::Failed).await;
    r.eng.shutdown().await;
    let e2 = engine(&r.t, &r.fake).await;
    // 不完整、网络错误都不是文件本身的问题：照常补转；损坏且没变的不再自动入队
    let n = notices(&e2, NoticeKind::CaughtUp);
    assert_eq!(n.len(), 1);
    assert_eq!(n[0].count, 2);
    for _ in 0..500 {
        if r.fake.calls().len() == 5 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut calls = r.fake.calls();
    calls.sort();
    assert_eq!(
        calls,
        vec!["bad.ncm", "inc.ncm", "inc.ncm", "net.mflac", "net.mflac"]
    );
    assert!(e2.snapshot().jobs.iter().all(|j| j.file_name != "bad.ncm"));
    // 文件变了就当新文件
    append(&bad, b"2");
    settle(&e2);
    for _ in 0..500 {
        if e2
            .snapshot()
            .jobs
            .iter()
            .any(|j| j.file_name == "bad.ncm" && j.state == JobState::Done)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(r.fake.calls().len(), 6);
}

#[tokio::test]
async fn start_removes_leftover_part_files_only() {
    let r = Rig::new("parts").await;
    r.eng.shutdown().await;
    let keep = [
        ".notes.part",
        "a.part",
        ".1a2b.part.bak",
        ".XYZ.part",
        ".bad.part",
        ".face.part",
        ".1a2b3c4d5e6f.part",
        ".musicreveal-zz.part",
        ".musicreveal-.part",
        ".musicreveal-1a2b.part.bak",
        "song.flac",
    ];
    for n in keep {
        put(&r.out, n, b"x");
    }
    put(&r.out, ".musicreveal-1a2b3c4d5e6f.part", b"half");
    let _e2 = engine(&r.t, &r.fake).await;
    let mut left: Vec<String> = std::fs::read_dir(&r.out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    let mut want: Vec<String> = keep.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(left, want);
}

/// 普通音频入库：监听文件夹里的 mp3 经真实转换器进输出文件夹；快照回调交给资料库观察，
/// 它据此换到输出文件夹，并在任务完成时立即重扫（资料库不自动轮询）。
#[tokio::test(flavor = "multi_thread")]
async fn plain_audio_is_ingested_and_shows_up_in_library() {
    use musicreveal_app::library::Library;
    // 生成素材：`REVEAL_GEN_FFMPEG` 优先（随包精简版没有 lavfi）
    let ff = std::env::var_os("REVEAL_GEN_FFMPEG")
        .map(std::path::PathBuf::from)
        .or_else(reveal_core::find_ffmpeg)
        .expect("需要 ffmpeg");
    let t = Tmp::new("plain");
    let (watch, out) = (t.dir("watch"), t.dir("out"));
    let (tx, rx) = std::sync::mpsc::channel();
    let lib = Library::new(
        t.0.join("data"),
        Box::new(move |v| {
            let _ = tx.send(v);
        }),
    )
    .with_poll_interval(None);
    let l = lib.clone();
    let eng = Engine::new(
        t.0.join("data"),
        Arc::new(RealConverter::new(t.0.clone())),
        Box::new(move |s| l.observe(&s)),
    )
    .with_poll_interval(None);
    eng.start().await;
    eng.complete_onboarding(
        vec![watch.to_string_lossy().into()],
        out.to_string_lossy().into(),
        "original",
    )
    .unwrap();
    // 先等资料库在空的输出文件夹上扫完：之后能看到新歌只能靠任务完成时的重扫
    loop {
        let v = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("资料库没换到输出文件夹");
        if v.dir == out.to_string_lossy() && !v.scanning {
            break;
        }
    }
    let tmp = t.0.join("歌.mp3");
    let st = std::process::Command::new(ff)
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=f=440:d=1"])
        .args(["-c:a", "libmp3lame", "-metadata", "title=入库", "-metadata"])
        .arg("artist=某人")
        .arg(&tmp)
        .status()
        .unwrap();
    assert!(st.success());
    std::fs::rename(&tmp, watch.join("歌.mp3")).unwrap();
    settle(&eng);

    let mut job = None;
    for _ in 0..3000 {
        job = eng
            .snapshot()
            .jobs
            .into_iter()
            .find(|j| j.state == JobState::Done);
        if job.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let job = job.unwrap_or_else(|| panic!("没完成：{:?}", eng.snapshot().jobs));
    assert_eq!(job.platform, Platform::Other);
    let output = PathBuf::from(job.output.unwrap());
    assert!(output.starts_with(&out) && output.exists(), "{output:?}");

    let id = output.to_string_lossy().into_owned();
    let v = loop {
        let v = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("资料库没看到新歌");
        if v.tracks.iter().any(|t| t.id == id) {
            break v;
        }
    };
    assert_eq!(v.dir, out.to_string_lossy());
    assert_eq!(lib.track(&id).unwrap().title, "入库");
    eng.shutdown().await;
}
