//! 应用后台：监听、串行队列、处理记录。不依赖 tauri。

mod model;
mod real;
pub(crate) mod scan;
pub(crate) mod store;

pub use model::*;
pub use real::RealConverter;
use reveal_core::{
    CancelToken, ConvertError, ConvertOptions, ConvertOutcome, OutputFormat, QqStatus, Stage,
};
use scan::Stat;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};
pub use store::home;
use store::{History, HistoryFile, Key, Pending, Settings};
use tokio::sync::{watch, Notify};

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
/// QQ 登录态每 10s 刷新。
const QQ_POLLS: u64 = 5;
/// 不完整的文件 60s 不变就判失败。
const INCOMPLETE_POLLS: u32 = 30;
/// shutdown 先礼后兵：取消后等这么久，还没停就直接丢弃转换。
const GRACE: Duration = Duration::from_secs(3);

/// 给用户看的简短中文原因。
fn reason(e: &ConvertError) -> String {
    match e {
        ConvertError::Unsupported(_) => "不支持的格式".into(),
        ConvertError::Incomplete => "文件不完整".into(),
        ConvertError::Corrupt(_) => "文件已损坏".into(),
        ConvertError::Failed(s) => s.split(['：', '（']).next().unwrap_or(s).trim().into(),
        e => e.to_string(),
    }
}

fn fmt_of(f: OutputFormat) -> Fmt {
    match f {
        OutputFormat::Original => Fmt::Original,
        OutputFormat::Mp3 => Fmt::Mp3,
    }
}

/// 还没有结果的状态：同一源路径此时不再另建任务。
fn unfinished(s: JobState) -> bool {
    !matches!(s, JobState::Done | JobState::Skipped | JobState::Failed)
}

struct Task {
    id: u64,
    view: Job,
    src: PathBuf,
    /// 来自哪个监听文件夹；拖入的为 None。
    folder: Option<PathBuf>,
    /// 最近一次确认的大小与 mtime；downloading 时之后连续不变的轮询次数。
    last: Option<Stat>,
    same: u32,
    /// 转换报不完整，等文件再变化。
    incomplete: bool,
    opts: Option<ConvertOptions>,
}

struct State {
    settings: Settings,
    history: History,
    /// 按 id 升序（即创建顺序）。
    tasks: Vec<Task>,
    queue: VecDeque<u64>,
    running: Option<(u64, CancelToken)>,
    /// 已经纳入考虑的文件及其最近的大小与 mtime。
    seen: HashMap<PathBuf, Stat>,
    /// 源路径 → 最近一条任务。
    live: HashMap<PathBuf, u64>,
    /// 「已有旧歌」还没回答的文件 → 含它的未回答询问张数：只要还有一张没回答就不处理。
    held: HashMap<PathBuf, u32>,
    /// 通知及其涉及的文件。
    notices: Vec<(Notice, Vec<Key>)>,
    /// 不可访问的监听文件夹及原因。
    folder_err: HashMap<PathBuf, String>,
    qq: QqStatus,
    qq_stamp: Option<SystemTime>,
    /// 最近一次 QQ 新格式转换被拒（登录过期）。
    qq_rejected: bool,
    ticks: u64,
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
    /// shutdown 宽限期过后丢弃当前转换。
    kill: Notify,
    /// 串行化落盘；true 表示已关闭，不再写。
    saving: Mutex<bool>,
    ready: watch::Sender<bool>,
}

#[derive(Clone)]
pub struct Engine(Arc<Inner>);

fn id_of(s: &str) -> u64 {
    s.trim_start_matches('j').parse().unwrap_or(0)
}

/// 与前端 `convertFileSrc` 相同的 asset 协议地址（macOS）。
pub(crate) fn asset_url(p: &Path) -> String {
    let mut s = String::from("asset://localhost/");
    for b in p.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            s.push(b as char);
        } else {
            s += &format!("%{b:02X}");
        }
    }
    s
}

/// 用 macOS 自带的 sips 把封面缩成 ≤256px 的 JPEG。
async fn thumbnail(cover: &[u8], dir: &Path, out: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    out.hash(&mut h);
    let base = dir.join(format!("{:016x}", h.finish()));
    let (src, jpg) = (base.with_extension("src"), base.with_extension("jpg"));
    std::fs::create_dir_all(dir).ok()?;
    std::fs::write(&src, cover).ok()?;
    let ok = tokio::process::Command::new("sips")
        .args(["-Z", "256", "-s", "format", "jpeg"])
        .arg(&src)
        .arg("--out")
        .arg(&jpg)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success());
    let _ = std::fs::remove_file(&src);
    ok.then_some(jpg)
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

type ScanResult = Result<Vec<(PathBuf, Stat)>, String>;

/// 源文件内容的 SHA-256（十六进制）；读不了为 None。
async fn content_hash(p: PathBuf) -> Option<String> {
    tokio::task::spawn_blocking(move || {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        std::io::copy(&mut std::fs::File::open(p).ok()?, &mut h).ok()?;
        Some(format!("{:x}", h.finalize()))
    })
    .await
    .ok()
    .flatten()
}

/// reveal-core 的半成品命名：`.musicreveal-<小写十六进制>.part`。
fn is_leftover_part(name: &str) -> bool {
    name.strip_prefix(reveal_core::PART_PREFIX)
        .and_then(|n| n.strip_suffix(".part"))
        .is_some_and(|h| {
            !h.is_empty()
                && h.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

/// 崩溃或被强杀后留在输出目录里的半成品；其他文件不碰。
fn remove_leftover_parts(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        if is_leftover_part(&e.file_name().to_string_lossy())
            && e.file_type().is_ok_and(|t| t.is_file())
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

impl State {
    fn task(&mut self, id: u64) -> Option<&mut Task> {
        let i = self.tasks.binary_search_by_key(&id, |t| t.id).ok()?;
        Some(&mut self.tasks[i])
    }

    fn remove(&mut self, id: u64) {
        self.tasks.retain(|t| t.id != id);
        self.queue.retain(|q| *q != id);
    }

    /// 同一源路径最近一条任务的 id 与状态。
    fn live_of(&mut self, p: &Path) -> Option<(u64, JobState)> {
        let id = *self.live.get(p)?;
        self.task(id).map(|t| (id, t.view.state))
    }

    fn new_task(
        &mut self,
        src: PathBuf,
        folder: Option<PathBuf>,
        state: JobState,
        last: Stat,
    ) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        let ext = src
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());
        let view = Job {
            id: format!("j{id}"),
            source: src.to_string_lossy().into_owned(),
            file_name: name_of(&src),
            platform: match ext.as_deref() {
                Some("ncm") => Platform::Netease,
                Some("mp3" | "flac" | "ogg" | "m4a" | "wav") => Platform::Other,
                _ => Platform::Qq,
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
        };
        self.live.insert(src.clone(), id);
        self.tasks.push(Task {
            id,
            view,
            src,
            folder,
            last: Some(last),
            same: 0,
            incomplete: false,
            opts: None,
        });
        id
    }

    /// 要转换的文件：同一源路径已有未结束的任务就不动，失败的重新入队，其余新建。
    fn submit(&mut self, path: PathBuf, folder: Option<PathBuf>, stat: Stat) {
        self.seen.insert(path.clone(), stat);
        match self.live_of(&path) {
            Some((id, JobState::Failed)) => {
                self.task(id).expect("刚查到").last = Some(stat);
                self.enqueue(id);
            }
            Some((_, s)) if unfinished(s) => {}
            _ => {
                let id = self.new_task(path, folder, JobState::Queued, stat);
                self.enqueue(id);
            }
        }
    }

    fn notice(&mut self, kind: NoticeKind, folder: Option<&Path>, files: Vec<Key>) {
        self.next_id += 1;
        let n = Notice {
            id: format!("n{}", self.next_id),
            kind,
            folder: folder.map(|f| f.to_string_lossy().into_owned()),
            count: files.len() as u32,
        };
        self.notices.push((n, files));
    }

    /// 记下扫描结果对应的可访问状态，返回扫到的文件。
    fn apply_scan(&mut self, folder: &Path, r: ScanResult) -> Vec<(PathBuf, Stat)> {
        match r {
            Ok(f) => {
                self.folder_err.remove(folder);
                f
            }
            Err(e) => {
                self.folder_err.insert(folder.to_path_buf(), e);
                vec![]
            }
        }
    }

    /// 问用户要不要转换这批旧歌；回答前它们一律不处理。
    fn ask(&mut self, folder: &Path, files: Vec<Key>) {
        for k in &files {
            *self.held.entry(k.path.clone()).or_default() += 1;
        }
        self.history.pending.push(Pending {
            folder: folder.to_path_buf(),
            files: files.iter().map(|k| k.path.clone()).collect(),
        });
        self.notice(NoticeKind::ExistingSongs, Some(folder), files);
    }

    /// 加入监听；已有的旧歌先发通知等用户选择。
    fn add_folder(&mut self, folder: PathBuf, r: ScanResult) -> u32 {
        if self.settings.folders.contains(&folder) {
            return 0;
        }
        self.settings.folders.push(folder.clone());
        let mut old = vec![];
        for (path, stat) in self.apply_scan(&folder, r) {
            self.seen.insert(path.clone(), stat);
            if !self.history.known(&path, stat) {
                old.push(Key { path, stat });
            }
        }
        let n = old.len() as u32;
        if n > 0 {
            self.ask(&folder, old);
        }
        n
    }

    fn take_notice(&mut self, folder: &Path) -> Vec<Key> {
        self.history.pending.retain(|p| p.folder != folder);
        let f = Some(folder.to_string_lossy().into_owned());
        let mut out = vec![];
        self.notices.retain_mut(|(n, files)| {
            let hit = n.kind == NoticeKind::ExistingSongs && n.folder == f;
            if hit {
                out.append(files);
            }
            !hit
        });
        for k in &out {
            if let Some(n) = self.held.get_mut(&k.path) {
                *n -= 1;
                if *n == 0 {
                    self.held.remove(&k.path);
                }
            }
        }
        out
    }

    /// 刷新 QQ 登录态；刚登录上或 plist 变了，就把等登录的任务重新入队。
    fn refresh_qq(&mut self, s: QqStatus, stamp: Option<SystemTime>) {
        let logged = |q: &QqStatus| matches!(q, QqStatus::LoggedIn { .. });
        if (logged(&s) && !logged(&self.qq)) || stamp != self.qq_stamp {
            self.qq_rejected = false;
            let waiting: Vec<u64> = self
                .tasks
                .iter()
                .filter(|t| t.view.state == JobState::WaitLogin)
                .map(|t| t.id)
                .collect();
            for id in waiting {
                self.requeue(id);
            }
        }
        self.qq = s;
        self.qq_stamp = stamp;
    }

    /// 重新入队，以文件当前的大小与 mtime 为准。
    fn requeue(&mut self, id: u64) {
        if let Some(t) = self.task(id) {
            t.last = scan::stat(&t.src);
            self.enqueue(id);
        }
    }

    /// 以当前输出设置入队。
    fn enqueue(&mut self, id: u64) {
        let opts = ConvertOptions {
            output_dir: self.settings.output_dir.clone(),
            format: match self.settings.format {
                Fmt::Original => OutputFormat::Original,
                Fmt::Mp3 => OutputFormat::Mp3,
            },
            ffmpeg: None,
        };
        let Some(t) = self.task(id) else { return };
        let was_queued = t.view.state == JobState::Queued;
        t.view.state = JobState::Queued;
        t.view.error = None;
        t.opts = Some(opts);
        if !was_queued || !self.queue.contains(&id) {
            self.queue.push_back(id);
        }
    }

    /// 一次轮询里，对一个下载中或排队中的任务看到的大小与 mtime。
    fn track(&mut self, id: u64, now: Option<Stat>) {
        let Some(t) = self.task(id) else { return };
        if !matches!(t.view.state, JobState::Downloading | JobState::Queued) {
            return;
        }
        if now.is_none() {
            // 下载器的临时文件被改名或删掉了
            self.remove(id);
            return;
        }
        if t.view.state == JobState::Queued {
            if now != t.last {
                t.view.state = JobState::Downloading;
                (t.last, t.same, t.incomplete) = (now, 0, false);
                self.queue.retain(|q| *q != id);
            }
            return;
        }
        if now == t.last {
            t.same = t.same.saturating_add(1);
        } else {
            (t.last, t.same, t.incomplete) = (now, 0, false);
        }
        if t.incomplete {
            if t.same >= INCOMPLETE_POLLS {
                t.view.state = JobState::Failed;
                t.view.error = Some(reason(&ConvertError::Incomplete));
            }
        } else if t.same >= 1 {
            self.enqueue(id);
        }
    }

    /// 监听扫描看到的文件。
    fn discover(&mut self, p: PathBuf, s: Stat, folder: &Path) {
        if self.seen.get(&p) == Some(&s) {
            return;
        }
        self.seen.insert(p.clone(), s);
        if self.held.contains_key(&p) {
            return;
        }
        match self.live_of(&p) {
            None if self.history.failed(&p, s) => {}
            Some((id, JobState::Failed)) => {
                let t = self.task(id).expect("刚查到");
                t.view.state = JobState::Downloading;
                t.view.error = None;
                (t.last, t.same, t.incomplete) = (Some(s), 0, false);
            }
            Some((_, st)) if unfinished(st) => {}
            _ => {
                self.new_task(p, Some(folder.to_path_buf()), JobState::Downloading, s);
            }
        }
    }

    /// 取下一首：开转前再核一次文件，变了就退回下载中。
    fn next_job(&mut self) -> Option<(u64, PathBuf, ConvertOptions, CancelToken, Stat)> {
        if self.stopping {
            return None;
        }
        while let Some(id) = self.queue.pop_front() {
            let Some(t) = self.task(id) else { continue };
            if t.view.state != JobState::Queued {
                continue;
            }
            let Some(opts) = t.opts.clone() else { continue };
            let now = scan::stat(&t.src);
            match now {
                None => self.remove(id),
                Some(stat) if Some(stat) == t.last => {
                    t.view.state = JobState::Decrypt;
                    let (src, tok) = (t.src.clone(), CancelToken::default());
                    self.running = Some((id, tok.clone()));
                    return Some((id, src, opts, tok, stat));
                }
                _ => {
                    t.view.state = JobState::Downloading;
                    (t.last, t.same, t.incomplete) = (now, 0, false);
                }
            }
        }
        None
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
                live: HashMap::new(),
                held: HashMap::new(),
                notices: vec![],
                folder_err: HashMap::new(),
                qq: QqStatus::LoggedOut,
                qq_stamp: None,
                qq_rejected: false,
                ticks: 0,
                next_id: 0,
                stopping: false,
            }),
            wake: Notify::new(),
            dirty: Notify::new(),
            kill: Notify::new(),
            saving: Mutex::new(false),
            ready: watch::Sender::new(false),
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

    fn covers_dir(&self) -> PathBuf {
        self.0.data_dir.join("covers")
    }

    /// 落盘当前设置与记录。串行；在锁内取快照，所以后写的一定是更新的版本。
    // ponytail: 每次整份重写 history.json，记录到几十万条再改增量
    fn persist(&self) {
        let closed = self.0.saving.lock().unwrap_or_else(|e| e.into_inner());
        if *closed {
            return;
        }
        let (s, h) = {
            let st = self.lock();
            (st.settings.clone(), HistoryFile::from(&st.history))
        };
        store::save(&self.settings_path(), &s);
        store::save(&self.history_path(), &h);
    }

    /// `start()` 完成后返回。
    pub async fn ready(&self) {
        let _ = self.0.ready.subscribe().wait_for(|r| *r).await;
    }

    pub async fn start(&self) {
        // 任务只属于本次运行，上次的缩略图没用了
        let _ = std::fs::remove_dir_all(self.covers_dir());
        let settings: Settings = store::load(&self.settings_path());
        remove_leftover_parts(&settings.output_dir);
        let history: History = store::load::<HistoryFile>(&self.history_path()).into();
        let (qq, stamp) = (self.0.conv.qq_status(), self.0.conv.qq_stamp());
        let scans: Vec<(PathBuf, ScanResult)> = settings
            .folders
            .iter()
            .map(|f| (f.clone(), scan::scan(f, &settings.output_dir)))
            .collect();
        self.edit(|st| {
            st.settings = settings;
            st.history = history;
            st.qq = qq;
            st.qq_stamp = stamp;
            // 没回答的旧歌提问照旧挂着；其余没记录的是关窗期间下载的、或上次被中断的：补转
            let pending = std::mem::take(&mut st.history.pending);
            let mut caught = vec![];
            for (folder, r) in scans {
                let asked: HashSet<&PathBuf> = pending
                    .iter()
                    .filter(|p| p.folder == folder)
                    .flat_map(|p| &p.files)
                    .collect();
                let mut held = vec![];
                for (path, stat) in st.apply_scan(&folder, r) {
                    st.seen.insert(path.clone(), stat);
                    let k = Key { path, stat };
                    if asked.contains(&k.path) {
                        held.push(k);
                    } else if !st.history.known(&k.path, stat) && !st.history.failed(&k.path, stat)
                    {
                        caught.push((k, folder.clone()));
                    }
                }
                if !held.is_empty() {
                    st.ask(&folder, held);
                }
            }
            if !caught.is_empty() {
                let keys = caught.iter().map(|(k, _)| k.clone()).collect();
                for (k, folder) in caught {
                    st.submit(k.path, Some(folder), k.stat);
                }
                st.notice(NoticeKind::CaughtUp, None, keys);
            }
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
        self.0.ready.send_replace(true);
    }

    async fn emitter(self) {
        let mut last = None;
        loop {
            self.0.dirty.notified().await;
            // 合并 50ms 内的连续变化
            tokio::time::sleep(Duration::from_millis(50)).await;
            if self.lock().stopping {
                return;
            }
            let s = self.snapshot();
            if last.as_ref() != Some(&s) {
                (self.0.on_change)(s.clone());
                last = Some(s);
            }
        }
    }

    /// 一次轮询：跟踪下载中与排队中的文件，发现新文件，按时刷新 QQ 状态。读盘都在锁外。
    pub fn tick(&self) {
        let (folders, skip, tracked, due, old_stamp) = {
            let mut st = self.lock();
            st.ticks += 1;
            let tracked: Vec<(u64, PathBuf)> = st
                .tasks
                .iter()
                .filter(|t| matches!(t.view.state, JobState::Downloading | JobState::Queued))
                .map(|t| (t.id, t.src.clone()))
                .collect();
            (
                st.settings.folders.clone(),
                st.settings.output_dir.clone(),
                tracked,
                st.ticks.is_multiple_of(QQ_POLLS),
                st.qq_stamp,
            )
        };
        let stamp = self.0.conv.qq_stamp();
        let qq = (due || stamp != old_stamp).then(|| self.0.conv.qq_status());
        let stats: Vec<(u64, Option<Stat>)> = tracked
            .into_iter()
            .map(|(id, p)| (id, scan::stat(&p)))
            .collect();
        let scans: Vec<(PathBuf, ScanResult)> = folders
            .into_iter()
            .map(|f| {
                let r = scan::scan(&f, &skip);
                (f, r)
            })
            .collect();
        self.edit(|st| {
            if let Some(q) = qq {
                st.refresh_qq(q, stamp);
            }
            for (id, now) in stats {
                st.track(id, now);
            }
            for (folder, r) in scans {
                if !st.settings.folders.contains(&folder) {
                    continue;
                }
                for (p, s) in st.apply_scan(&folder, r) {
                    st.discover(p, s, &folder);
                }
            }
        });
    }

    async fn worker(self) {
        loop {
            let Some((id, src, opts, tok, stat)) = self.edit(|st| st.next_job()) else {
                if self.lock().stopping {
                    return;
                }
                self.0.wake.notified().await;
                continue;
            };
            let key = Key {
                path: src.clone(),
                stat,
            };
            let fmt = fmt_of(opts.format);
            let reuse = self.lock().history.reusable(&key, fmt);
            // 不同路径的同一份内容：按哈希找已有输出；命中后记到这条路径上，下次不必再算
            let (reuse, hash) = match reuse {
                Some(o) => (Some(o), None),
                None => {
                    let h = content_hash(src.clone()).await;
                    let o = h
                        .as_deref()
                        .and_then(|h| self.lock().history.same_content(h, fmt));
                    (o, h)
                }
            };
            if let Some(out) = reuse {
                let fresh = hash.is_some();
                self.edit(|st| {
                    st.running = None;
                    if let Some(t) = st.task(id) {
                        t.view.state = JobState::Skipped;
                        t.view.output = Some(out.to_string_lossy().into_owned());
                    }
                    if fresh {
                        st.history.record(key, fmt, out, hash);
                    }
                });
                if fresh {
                    self.persist();
                }
                continue;
            }
            let r = match std::fs::create_dir_all(&opts.output_dir) {
                Err(e) => Err(ConvertError::Failed(format!("无法创建输出目录：{e}"))),
                Ok(()) => {
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
                    let conv = self.0.conv.clone();
                    // 被 kill 时丢弃 future：reveal-core 借此清理 .part
                    tokio::select! {
                        r = conv.convert(&src, &opts, &tok, &mut progress) => r,
                        _ = self.0.kill.notified() => Err(ConvertError::Cancelled),
                    }
                }
            };
            let save = self.edit(|st| {
                st.running = None;
                let Some(t) = st.task(id) else { return false };
                match r {
                    Ok(o) => {
                        if let Some(c) = o.meta.cover.clone() {
                            let (me, out) = (self.clone(), o.output.clone());
                            tokio::spawn(async move {
                                let Some(jpg) = thumbnail(&c, &me.covers_dir(), &out).await else {
                                    return;
                                };
                                me.edit(|st| {
                                    if let Some(t) = st.task(id) {
                                        t.view.cover_url = Some(asset_url(&jpg));
                                    }
                                });
                            });
                        }
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
                        st.history.record(key, fmt, o.output, hash);
                        return true;
                    }
                    Err(ConvertError::Cancelled) => {}
                    Err(ConvertError::NeedsLogin) => {
                        t.view.state = JobState::WaitLogin;
                        st.qq_rejected = true;
                    }
                    Err(ConvertError::Incomplete) => {
                        // 以开转时的样子为基线：转换期间长了也算「再变化」
                        t.view.state = JobState::Downloading;
                        (t.last, t.same, t.incomplete) = (Some(stat), 0, true);
                    }
                    Err(e) => {
                        t.view.state = JobState::Failed;
                        t.view.error = Some(reason(&e));
                        // 只记源文件本身的问题；网络、磁盘等暂时性失败下次启动照常补转
                        if matches!(e, ConvertError::Unsupported(_) | ConvertError::Corrupt(_)) {
                            st.history.fail(key);
                            return true;
                        }
                    }
                }
                false
            });
            if save {
                self.persist();
            }
        }
    }

    pub fn set_output_dir(&self, path: &str) -> Result<(), String> {
        if path.trim().is_empty() {
            return Err("输出目录不能为空".into());
        }
        self.edit(|st| st.settings.output_dir = path.into());
        self.persist();
        Ok(())
    }

    pub fn set_format(&self, format: &str) -> Result<(), String> {
        let f = Fmt::parse(format)?;
        self.edit(|st| st.settings.format = f);
        self.persist();
        Ok(())
    }

    /// 拖入的文件或文件夹（只看一层，规则同监听扫描），直接入队；输出文件夹里的不算。
    pub fn add_files(&self, paths: Vec<String>) {
        let skip = self.lock().settings.output_dir.clone();
        let mut files = vec![];
        for p in paths.into_iter().map(PathBuf::from) {
            if p.is_dir() {
                files.extend(scan::shallow(&p, &skip));
            } else if scan::candidate(&p) && !scan::inside(&p, &skip) {
                files.extend(scan::stat(&p).map(|s| (p, s)));
            }
        }
        self.edit(|st| {
            for (f, s) in files {
                st.submit(f, None, s);
            }
        });
    }

    pub fn add_watch_folder(&self, path: &str) -> Result<AddFolderResult, String> {
        if path.trim().is_empty() {
            return Err("文件夹路径不能为空".into());
        }
        let folder = PathBuf::from(path);
        let r = scan::scan(&folder, &self.lock().settings.output_dir.clone());
        let existing = self.edit(|st| st.add_folder(folder, r));
        self.persist();
        Ok(AddFolderResult { existing })
    }

    /// 移除监听；该文件夹里还没开始的任务一并移出列表。
    pub fn remove_watch_folder(&self, path: &str) {
        let folder = PathBuf::from(path);
        self.edit(|st| {
            st.settings.folders.retain(|f| *f != folder);
            st.folder_err.remove(&folder);
            st.take_notice(&folder);
            st.seen.retain(|p, _| !p.starts_with(&folder));
            let gone: HashSet<u64> = st
                .tasks
                .iter()
                .filter(|t| {
                    t.folder.as_ref() == Some(&folder)
                        && matches!(
                            t.view.state,
                            JobState::Downloading | JobState::Queued | JobState::WaitLogin
                        )
                })
                .map(|t| t.id)
                .collect();
            st.tasks.retain(|t| !gone.contains(&t.id));
            st.queue.retain(|q| !gone.contains(q));
        });
        self.persist();
    }

    pub fn convert_existing(&self, path: &str) {
        let folder = PathBuf::from(path);
        self.edit(|st| {
            for k in st.take_notice(&folder) {
                st.submit(k.path, Some(folder.clone()), k.stat);
            }
        });
        self.persist();
    }

    /// 按忽略这一刻的磁盘样子记下：询问期间变过的也不再补转；之后再变（重新下载）照常转换。
    pub fn ignore_existing(&self, path: &str) {
        self.edit(|st| {
            let files = st
                .take_notice(Path::new(path))
                .into_iter()
                .filter_map(|k| scan::stat(&k.path).map(|stat| Key { stat, ..k }))
                .collect();
            st.history.ignore(files);
        });
        self.persist();
    }

    /// 关掉通知；关掉「已有旧歌」的提问等于忽略。
    pub fn dismiss_notice(&self, id: &str) {
        let folder = self.lock().notices.iter().find_map(|(n, _)| {
            (n.id == id && n.kind == NoticeKind::ExistingSongs)
                .then(|| n.folder.clone())
                .flatten()
        });
        match folder {
            Some(f) => self.ignore_existing(&f),
            None => self.edit(|st| st.notices.retain(|(n, _)| n.id != id)),
        }
    }

    /// 取消当前任务并等它清理完；宽限期过了还没停就丢弃它。返回后不再写盘。
    /// 被中断的任务不写记录。
    pub async fn shutdown(&self) {
        let tok = self.edit(|st| {
            st.stopping = true;
            st.running.as_ref().map(|(_, t)| t.clone())
        });
        self.0.wake.notify_one();
        if let Some(t) = tok {
            t.cancel();
            let idle = || async {
                while self.lock().running.is_some() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            };
            if tokio::time::timeout(GRACE, idle()).await.is_err() {
                self.0.kill.notify_one();
                let _ = tokio::time::timeout(Duration::from_secs(1), idle()).await;
            }
        }
        *self.0.saving.lock().unwrap_or_else(|e| e.into_inner()) = true;
    }

    fn output_of(&self, id: &str) -> Result<PathBuf, String> {
        self.lock()
            .task(id_of(id))
            .and_then(|t| t.view.output.clone())
            .map(PathBuf::from)
            .ok_or_else(|| "还没有输出文件".into())
    }

    pub fn open_qq_music(&self) -> Result<(), String> {
        std::process::Command::new("open")
            .args(["-b", "com.tencent.QQMusicMac"])
            .spawn()
            .map(drop)
            .map_err(|e| format!("无法打开 QQ 音乐：{e}"))
    }

    pub fn reveal_in_finder(&self, id: &str) -> Result<(), String> {
        let out = self.output_of(id)?;
        std::process::Command::new("open")
            .arg("-R")
            .arg(out)
            .spawn()
            .map(drop)
            .map_err(|e| format!("无法打开 Finder：{e}"))
    }

    pub fn open_auth_settings(&self) -> Result<(), String> {
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
            .spawn()
            .map(drop)
            .map_err(|e| format!("无法打开系统设置：{e}"))
    }

    /// 失败或等登录的任务重新入队。
    pub fn retry_job(&self, id: &str) {
        let id = id_of(id);
        self.edit(|st| {
            if st
                .task(id)
                .is_some_and(|t| matches!(t.view.state, JobState::Failed | JobState::WaitLogin))
            {
                st.requeue(id);
            }
        });
    }

    pub fn remove_job(&self, id: &str) {
        let id = id_of(id);
        self.edit(|st| {
            st.remove(id);
            if let Some((_, tok)) = st.running.as_ref().filter(|(r, _)| *r == id) {
                tok.cancel();
            }
        });
    }

    pub fn snapshot(&self) -> Snapshot {
        let st = self.lock();
        let s = &st.settings;
        let pos: HashMap<u64, u32> = st
            .queue
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, i as u32 + 1))
            .collect();
        let jobs: Vec<Job> = st
            .tasks
            .iter()
            .rev()
            .map(|t| Job {
                queue_pos: pos.get(&t.id).copied(),
                ..t.view.clone()
            })
            .collect();
        let count =
            |f: &dyn Fn(JobState) -> bool| jobs.iter().filter(|j| f(j.state)).count() as u32;
        let busy = count(&|s| unfinished(s) && s != JobState::WaitLogin);
        let waiting_login = count(&|s| s == JobState::WaitLogin);
        Snapshot {
            onboarded: s.onboarded,
            folders: s
                .folders
                .iter()
                .map(|p| Folder {
                    path: p.to_string_lossy().into_owned(),
                    name: name_of(p),
                    accessible: !st.folder_err.contains_key(p),
                    reason: st.folder_err.get(p).cloned(),
                })
                .collect(),
            output_dir: s.output_dir.to_string_lossy().into_owned(),
            format: s.format,
            qq: match &st.qq {
                QqStatus::LoggedIn { masked_uin } => Qq {
                    state: if st.qq_rejected {
                        QqState::Expired
                    } else {
                        QqState::LoggedIn
                    },
                    account: Some(masked_uin.clone()),
                },
                s => Qq {
                    state: match s {
                        QqStatus::NotInstalled => QqState::NotInstalled,
                        QqStatus::NeedsAuth => QqState::NeedsAuth,
                        QqStatus::Expired => QqState::Expired,
                        _ => QqState::LoggedOut,
                    },
                    account: None,
                },
            },
            busy,
            waiting_login,
            jobs,
            notices: st.notices.iter().map(|(n, _)| n.clone()).collect(),
        }
    }

    pub fn complete_onboarding(
        &self,
        folders: Vec<String>,
        output_dir: String,
        format: &str,
    ) -> Result<(), String> {
        let format = Fmt::parse(format)?;
        let out = PathBuf::from(output_dir);
        let scans: Vec<(PathBuf, ScanResult)> = folders
            .into_iter()
            .map(PathBuf::from)
            .map(|f| {
                let r = scan::scan(&f, &out);
                (f, r)
            })
            .collect();
        self.edit(|st| {
            st.settings.onboarded = true;
            st.settings.output_dir = out;
            st.settings.format = format;
            st.settings.folders.clear();
            for (f, r) in scans {
                st.add_folder(f, r);
            }
        });
        self.persist();
        Ok(())
    }
}
