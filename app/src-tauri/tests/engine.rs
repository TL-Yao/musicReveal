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
