//! 应用后台：监听、串行队列、处理记录。不依赖 tauri。

mod model;
mod scan;
mod store;

pub use model::*;
use reveal_core::{CancelToken, ConvertError, ConvertOptions, ConvertOutcome, QqStatus, Stage};
use scan::Stat;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};
use store::{History, Key, Settings};
use tokio::sync::Notify;

#[async_trait::async_trait]
pub trait Converter: Send + Sync {
    async fn convert(
        &self,
        input: &Path,
        opts: &ConvertOptions,
        cancel: &CancelToken,
        progress: &mut (dyn FnMut(Stage) + Send),
    ) -> Result<ConvertOutcome, ConvertError>;
    fn qq_status(&self) -> QqStatus;
    /// QQ 音乐登录 plist 的 mtime；变化即视为登录态可能变了。
    fn qq_stamp(&self) -> Option<SystemTime> {
        None
    }
}

/// 一次轮询的间隔；以下以轮询次数计时。
pub const POLL: Duration = Duration::from_secs(2);
/// 不完整的文件 60s 不变就判失败。
const INCOMPLETE_POLLS: u32 = 30;

/// 给用户看的简短中文原因。
fn reason(e: &ConvertError) -> String {
    match e {
        ConvertError::Unsupported(_) => "不支持的格式".into(),
        ConvertError::Incomplete => "文件不完整".into(),
        ConvertError::Failed(s) => s.split(['：', '（']).next().unwrap_or(s).trim().into(),
        e => e.to_string(),
    }
}

struct Task {
    view: Job,
    src: PathBuf,
    /// 来自哪个监听文件夹；拖入的为 None。
    folder: Option<PathBuf>,
    /// downloading 时最近一次看到的大小与 mtime，以及之后连续不变的轮询次数。
    last: Option<Stat>,
    same: u32,
    /// 转换报不完整，等文件再变化。
    incomplete: bool,
    opts: Option<ConvertOptions>,
}

struct State {
    settings: Settings,
    history: History,
    tasks: Vec<Task>,
    queue: VecDeque<u64>,
    running: Option<(u64, CancelToken)>,
    /// 已经纳入考虑的文件及其最近的大小与 mtime。
    seen: HashMap<PathBuf, Stat>,
    next_id: u64,
    stopping: bool,
}

struct Inner {
    data_dir: PathBuf,
    conv: Arc<dyn Converter>,
    on_change: Box<dyn Fn(Snapshot) + Send + Sync>,
    poll: Option<Duration>,
    st: Mutex<State>,
    wake: Notify,
    dirty: Notify,
}

#[derive(Clone)]
pub struct Engine(Arc<Inner>);

fn id_of(s: &str) -> u64 {
    s.trim_start_matches('j').parse().unwrap_or(0)
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

impl State {
    fn task(&mut self, id: u64) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| id_of(&t.view.id) == id)
    }

    fn new_task(&mut self, src: PathBuf, folder: Option<PathBuf>, state: JobState) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        let ext = src
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());
        let view = Job {
            id: format!("j{id}"),
            source: src.to_string_lossy().into_owned(),
            file_name: name_of(&src),
            platform: if ext.as_deref() == Some("ncm") {
                Platform::Netease
            } else {
                Platform::Qq
            },
            state,
            queue_pos: None,
            title: None,
            artists: vec![],
            album: None,
            cover_url: None,
            quality: None,
            has_cover: false,
            has_lyrics: false,
            renamed: None,
            error: None,
            output: None,
            playing: false,
        };
        self.tasks.insert(
            0,
            Task {
                view,
                src,
                folder,
                last: None,
                same: 0,
                incomplete: false,
                opts: None,
            },
        );
        id
    }

    /// 以当前输出设置入队。
    fn enqueue(&mut self, id: u64) {
        let opts = ConvertOptions {
            output_dir: self.settings.output_dir.clone(),
            format: match self.settings.format {
                Fmt::Original => reveal_core::OutputFormat::Original,
                Fmt::Mp3 => reveal_core::OutputFormat::Mp3,
            },
            ffmpeg: None,
        };
        if let Some(t) = self.task(id) {
            t.view.state = JobState::Queued;
            t.view.error = None;
            t.opts = Some(opts);
            self.queue.retain(|q| *q != id);
            self.queue.push_back(id);
        }
    }
}

impl Engine {
    pub fn new(
        data_dir: PathBuf,
        converter: Arc<dyn Converter>,
        on_change: Box<dyn Fn(Snapshot) + Send + Sync>,
    ) -> Self {
        Engine(Arc::new(Inner {
            data_dir,
            conv: converter,
            on_change,
            poll: Some(POLL),
            st: Mutex::new(State {
                settings: Settings::default(),
                history: History::default(),
                tasks: vec![],
                queue: VecDeque::new(),
                running: None,
                seen: HashMap::new(),
                next_id: 0,
                stopping: false,
            }),
            wake: Notify::new(),
            dirty: Notify::new(),
        }))
    }

    /// `None`：不自动轮询，只靠 `tick()` 推进（测试用）。须在 `start()` 之前调用。
    pub fn with_poll_interval(self, every: Option<Duration>) -> Self {
        let mut inner = Arc::try_unwrap(self.0).ok().expect("start 之前调用");
        inner.poll = every;
        Engine(Arc::new(inner))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 改状态并安排推送快照。
    fn edit<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let (r, kick) = {
            let mut st = self.lock();
            let r = f(&mut st);
            (r, !st.queue.is_empty() && st.running.is_none())
        };
        if kick {
            self.0.wake.notify_one();
        }
        self.0.dirty.notify_one();
        r
    }

    fn settings_path(&self) -> PathBuf {
        self.0.data_dir.join("settings.json")
    }

    fn history_path(&self) -> PathBuf {
        self.0.data_dir.join("history.json")
    }

    pub async fn start(&self) {
        let (settings, history) = (
            store::load::<Settings>(&self.settings_path()),
            store::load::<History>(&self.history_path()),
        );
        self.edit(|st| {
            st.settings = settings;
            st.history = history;
        });
        tokio::spawn(self.clone().worker());
        tokio::spawn(self.clone().emitter());
        if let Some(every) = self.0.poll {
            let me = self.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(every).await;
                    if me.lock().stopping {
                        return;
                    }
                    me.tick();
                }
            });
        }
    }

    async fn emitter(self) {
        loop {
            self.0.dirty.notified().await;
            // 合并 50ms 内的连续变化
            tokio::time::sleep(Duration::from_millis(50)).await;
            if self.lock().stopping {
                return;
            }
            (self.0.on_change)(self.snapshot());
        }
    }

    /// 一次轮询：跟踪下载中的文件，发现新文件。
    pub fn tick(&self) {
        self.edit(|st| {
            for t in st.tasks.iter_mut() {
                if t.view.state != JobState::Downloading {
                    continue;
                }
                let now = scan::stat(&t.src);
                if now == t.last {
                    t.same += 1;
                } else {
                    (t.last, t.same, t.incomplete) = (now, 0, false);
                }
                if t.incomplete && t.same >= INCOMPLETE_POLLS {
                    t.view.state = JobState::Failed;
                    t.view.error = Some(reason(&ConvertError::Incomplete));
                }
            }
            let ready: Vec<u64> = st
                .tasks
                .iter()
                .filter(|t| {
                    t.view.state == JobState::Downloading && !t.incomplete && t.same >= 1
                })
                .map(|t| id_of(&t.view.id))
                .collect();
            for id in ready {
                st.enqueue(id);
            }
            let skip = st.settings.output_dir.clone();
            for folder in st.settings.folders.clone() {
                let Ok(files) = scan::scan(&folder, &skip) else {
                    continue;
                };
                for (p, s) in files {
                    if st.seen.get(&p) == Some(&s) {
                        continue;
                    }
                    st.seen.insert(p.clone(), s);
                    let live = st.tasks.iter_mut().find(|t| {
                        t.src == p
                            && !matches!(t.view.state, JobState::Done | JobState::Skipped)
                    });
                    if let Some(t) = live {
                        if t.view.state == JobState::Failed {
                            t.view.state = JobState::Downloading;
                            t.view.error = None;
                            (t.last, t.same, t.incomplete) = (Some(s), 0, false);
                        }
                        continue;
                    }
                    let id = st.new_task(p, Some(folder.clone()), JobState::Downloading);
                    st.task(id).unwrap().last = Some(s);
                }
            }
        });
    }

    async fn worker(self) {
        loop {
            let next = self.edit(|st| {
                if st.stopping {
                    return None;
                }
                let id = st.queue.pop_front()?;
                let tok = CancelToken::default();
                st.running = Some((id, tok.clone()));
                let t = st.task(id)?;
                t.view.state = JobState::Decrypt;
                Some((id, t.src.clone(), t.opts.clone()?, tok))
            });
            let Some((id, src, opts, tok)) = next else {
                if self.lock().stopping {
                    return;
                }
                self.lock().running = None;
                self.0.wake.notified().await;
                continue;
            };
            let me = self.clone();
            let mut progress = move |s: Stage| {
                me.edit(|st| {
                    if let Some(t) = st.task(id) {
                        t.view.state = match s {
                            Stage::Decrypt => JobState::Decrypt,
                            Stage::Transcode => JobState::Transcode,
                            Stage::Metadata => JobState::Metadata,
                        };
                    }
                });
            };
            let _ = std::fs::create_dir_all(&opts.output_dir);
            let r = self.0.conv.convert(&src, &opts, &tok, &mut progress).await;
            self.edit(|st| {
                st.running = None;
                let Some(t) = st.task(id) else { return };
                match r {
                    Ok(o) => {
                        let v = &mut t.view;
                        v.state = JobState::Done;
                        v.title = o.meta.title;
                        v.artists = o.meta.artists;
                        v.album = o.meta.album;
                        v.quality = Some(o.quality);
                        v.has_cover = o.has_cover;
                        v.has_lyrics = o.has_lyrics;
                        v.renamed = o.renamed;
                        v.output = Some(o.output.to_string_lossy().into_owned());
                    }
                    Err(ConvertError::Incomplete) => {
                        t.view.state = JobState::Downloading;
                        (t.last, t.same, t.incomplete) = (scan::stat(&t.src), 0, true);
                    }
                    Err(e) => {
                        t.view.state = JobState::Failed;
                        t.view.error = Some(reason(&e));
                    }
                }
            });
        }
    }

    pub fn retry_job(&self, id: &str) {
        self.edit(|st| st.enqueue(id_of(id)));
    }

    pub fn remove_job(&self, id: &str) {
        let id = id_of(id);
        self.edit(|st| {
            st.tasks.retain(|t| id_of(&t.view.id) != id);
            st.queue.retain(|q| *q != id);
            if let Some((_, tok)) = st.running.as_ref().filter(|(r, _)| *r == id) {
                tok.cancel();
            }
        });
    }

    pub fn snapshot(&self) -> Snapshot {
        let st = self.lock();
        let s = &st.settings;
        let mut jobs: Vec<Job> = st.tasks.iter().map(|t| t.view.clone()).collect();
        for (i, id) in st.queue.iter().enumerate() {
            if let Some(j) = jobs.iter_mut().find(|j| id_of(&j.id) == *id) {
                j.queue_pos = Some(i as u32 + 1);
            }
        }
        let count = |f: &dyn Fn(JobState) -> bool| jobs.iter().filter(|j| f(j.state)).count() as u32;
        let busy = count(&|s| {
            !matches!(
                s,
                JobState::Done | JobState::Skipped | JobState::WaitLogin | JobState::Failed
            )
        });
        let waiting_login = count(&|s| s == JobState::WaitLogin);
        let home = store::home();
        Snapshot {
            onboarded: s.onboarded,
            suggestions: ["Music/QQ音乐", "Music/网易云音乐"]
                .iter()
                .map(|r| home.join(r))
                .filter(|p| p.is_dir())
                .map(|p| Suggestion {
                    name: name_of(&p),
                    path: p.to_string_lossy().into_owned(),
                })
                .collect(),
            folders: s
                .folders
                .iter()
                .map(|p| Folder {
                    path: p.to_string_lossy().into_owned(),
                    name: name_of(p),
                    accessible: true,
                    reason: None,
                })
                .collect(),
            output_dir: s.output_dir.to_string_lossy().into_owned(),
            format: s.format,
            qq: Qq {
                state: QqState::LoggedOut,
                account: None,
            },
            busy,
            waiting_login,
            jobs,
            notices: vec![],
        }
    }

    pub fn complete_onboarding(
        &self,
        folders: Vec<String>,
        output_dir: String,
        format: &str,
    ) -> Result<(), String> {
        let format = Fmt::parse(format)?;
        self.edit(|st| {
            st.settings.onboarded = true;
            st.settings.output_dir = output_dir.into();
            st.settings.format = format;
            st.settings.folders = folders.into_iter().map(PathBuf::from).collect();
        });
        Ok(())
    }
}

#[allow(dead_code)]
fn _unused(_: Key) {}
