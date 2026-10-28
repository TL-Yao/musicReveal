//! 资料库：输出文件夹（含子文件夹）里实际存在的可播放文件。后台线程在被叫醒或定时轮询时扫描，
//! 内容变了才整份推送。

use crate::engine::scan::{self, Stat};
use crate::engine::store;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// 自动轮询的间隔。
pub const POLL: Duration = Duration::from_secs(2);
/// 扫描期间部分结果的推送间隔。
const PARTIAL: Duration = Duration::from_millis(250);
const AUDIO: [&str; 5] = ["mp3", "flac", "ogg", "m4a", "wav"];

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    /// 绝对路径。
    pub id: String,
    pub title: String,
    pub artists: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disc: Option<u32>,
    pub duration_ms: u64,
    pub quality: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    pub has_lyrics: bool,
    /// 文件创建时间（ms），取不到用 mtime。
    pub added_at: u64,
    /// 封面缩略图文件。
    #[serde(skip)]
    pub cover: Option<PathBuf>,
    /// 同名 .lrc（不论能否解析出歌词）。
    #[serde(skip)]
    pub lrc: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LibraryView {
    pub dir: String,
    pub tracks: Vec<Track>,
    pub scanning: bool,
}

/// 一首歌读出来的标签，按路径 + 大小 + mtime 缓存。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Rec {
    path: PathBuf,
    #[serde(flatten)]
    stat: Stat,
    added_at: u64,
    title: Option<String>,
    artists: Vec<String>,
    album: Option<String>,
    album_artist: Option<String>,
    year: Option<u32>,
    track: Option<u32>,
    disc: Option<u32>,
    duration_ms: u64,
    quality: String,
}

struct State {
    dir: PathBuf,
    tracks: BTreeMap<PathBuf, Track>,
    scanning: bool,
}

#[derive(Default)]
struct Sig {
    kick: bool,
    closed: bool,
    poll: Option<Duration>,
}

#[derive(Default)]
struct Signal {
    m: Mutex<Sig>,
    cv: Condvar,
}

struct Inner {
    emit: Box<dyn Fn(LibraryView) + Send + Sync>,
    st: Mutex<State>,
    sig: Arc<Signal>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        lock(&self.sig.m).closed = true;
        self.sig.cv.notify_all();
    }
}

#[derive(Clone)]
pub struct Library(Arc<Inner>);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn ext(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn keep(p: &Path) -> bool {
    let e = ext(p);
    e == "lrc" || AUDIO.contains(&e.as_str())
}

fn ms(t: std::io::Result<std::time::SystemTime>) -> Option<u64> {
    Some(t.ok()?.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64)
}

fn read(path: &Path, stat: Stat) -> Option<Rec> {
    let i = reveal_core::read_track(path).ok()?;
    let m = std::fs::metadata(path).ok()?;
    Some(Rec {
        path: path.to_path_buf(),
        stat,
        added_at: ms(m.created()).or(ms(m.modified())).unwrap_or(0),
        title: i.title,
        artists: i.artists,
        album: i.album,
        album_artist: i.album_artist,
        year: i.year,
        track: i.track,
        disc: i.disc,
        duration_ms: i.duration_ms,
        quality: i.quality,
    })
}

fn track_of(r: &Rec) -> Track {
    let name = r
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Track {
        id: r.path.to_string_lossy().into_owned(),
        title: r
            .title
            .clone()
            .unwrap_or_else(|| reveal_core::title_from_file_name(&name)),
        artists: r.artists.clone(),
        album: r.album.clone(),
        album_artist: r.album_artist.clone(),
        year: r.year,
        track: r.track,
        disc: r.disc,
        duration_ms: r.duration_ms,
        quality: r.quality.clone(),
        cover_url: None,
        has_lyrics: false,
        added_at: r.added_at,
        cover: None,
        lrc: None,
    }
}

/// 后台扫描线程自己的状态。
struct Worker {
    /// library.json：标签缓存。
    file: PathBuf,
    cache: HashMap<PathBuf, Rec>,
    /// 缓存有没写盘的变化。
    dirty: bool,
    /// 读不了的文件：没变就不再试。
    bad: HashMap<PathBuf, Stat>,
    last: Option<LibraryView>,
}

impl Worker {
    fn new(data_dir: &Path) -> Self {
        let file = data_dir.join("library.json");
        let recs: Vec<Rec> = store::load(&file);
        Worker {
            file,
            cache: recs.into_iter().map(|r| (r.path.clone(), r)).collect(),
            dirty: false,
            bad: HashMap::new(),
            last: None,
        }
    }

    fn save(&mut self) {
        if std::mem::take(&mut self.dirty) {
            let mut recs: Vec<&Rec> = self.cache.values().collect();
            recs.sort_by(|a, b| a.path.cmp(&b.path));
            store::save(&self.file, &recs);
        }
    }

    fn emit(&mut self, lib: &Inner) {
        let v = view_of(&lock(&lib.st));
        if self.last.as_ref() != Some(&v) {
            (lib.emit)(v.clone());
            self.last = Some(v);
        }
    }

    fn scan(&mut self, lib: &Inner) {
        let dir = lock(&lib.st).dir.clone();
        let found = if dir.as_os_str().is_empty() {
            vec![]
        } else {
            scan::files(&dir, &keep)
        };
        let audio: Vec<(PathBuf, Stat)> =
            found.into_iter().filter(|(p, _)| ext(p) != "lrc").collect();
        let now: HashMap<&PathBuf, Stat> = audio.iter().map(|(p, s)| (p, *s)).collect();
        let before = self.cache.len();
        self.cache.retain(|p, _| now.contains_key(p));
        self.dirty |= self.cache.len() != before;
        self.bad.retain(|p, s| now.get(p) == Some(s));
        let mut known = BTreeMap::new();
        let mut misses = vec![];
        for (p, s) in &audio {
            match self.cache.get(p) {
                Some(r) if r.stat == *s => {
                    known.insert(p.clone(), track_of(r));
                }
                _ if self.bad.get(p) == Some(s) => {}
                _ => misses.push((p.clone(), *s)),
            }
        }
        {
            let mut st = lock(&lib.st);
            if st.dir != dir {
                return;
            }
            // 正在重读的歌先保留旧样子
            for (p, _) in &misses {
                if let Some(t) = st.tracks.get(p) {
                    known.insert(p.clone(), t.clone());
                }
            }
            st.tracks = known;
            st.scanning = !misses.is_empty();
        }
        // 推送前落盘：收到事件时缓存已是新的
        self.save();
        self.emit(lib);
        let mut shown = Instant::now();
        for (p, s) in misses {
            let rec = read(&p, s);
            let mut st = lock(&lib.st);
            if st.dir != dir {
                return;
            }
            match rec {
                Some(r) => {
                    st.tracks.insert(p.clone(), track_of(&r));
                    self.cache.insert(p, r);
                    self.dirty = true;
                }
                None => {
                    st.tracks.remove(&p);
                    self.bad.insert(p, s);
                }
            }
            drop(st);
            if shown.elapsed() >= PARTIAL {
                self.emit(lib);
                shown = Instant::now();
            }
        }
        self.save();
        lock(&lib.st).scanning = false;
        self.emit(lib);
    }
}

fn view_of(st: &State) -> LibraryView {
    LibraryView {
        dir: st.dir.to_string_lossy().into_owned(),
        tracks: st.tracks.values().cloned().collect(),
        scanning: st.scanning,
    }
}

fn run(data_dir: PathBuf, lib: Weak<Inner>, sig: Arc<Signal>) {
    let mut w = Worker::new(&data_dir);
    loop {
        {
            let mut s = lock(&sig.m);
            loop {
                if s.closed {
                    return;
                }
                if std::mem::take(&mut s.kick) {
                    break;
                }
                match s.poll {
                    Some(d) => {
                        let (g, t) = sig.cv.wait_timeout(s, d).unwrap_or_else(|e| e.into_inner());
                        s = g;
                        if t.timed_out() {
                            break;
                        }
                    }
                    None => s = sig.cv.wait(s).unwrap_or_else(|e| e.into_inner()),
                }
            }
        }
        let Some(lib) = lib.upgrade() else { return };
        w.scan(&lib);
    }
}

impl Library {
    /// 还没有文件夹：先 `set_dir`。`emit` 只在后台扫描线程上调用。
    pub fn new(data_dir: PathBuf, emit: Box<dyn Fn(LibraryView) + Send + Sync>) -> Self {
        let sig = Arc::new(Signal::default());
        lock(&sig.m).poll = Some(POLL);
        let inner = Arc::new(Inner {
            emit,
            st: Mutex::new(State {
                dir: PathBuf::new(),
                tracks: BTreeMap::new(),
                scanning: true,
            }),
            sig: sig.clone(),
        });
        let weak = Arc::downgrade(&inner);
        std::thread::Builder::new()
            .name("library".into())
            .spawn(move || run(data_dir, weak, sig))
            .expect("无法启动资料库线程");
        Library(inner)
    }

    /// `None`：不自动轮询，只在 `refresh` / `set_dir` 时扫描（测试用）。
    pub fn with_poll_interval(self, every: Option<Duration>) -> Self {
        lock(&self.0.sig.m).poll = every;
        self.0.sig.cv.notify_all();
        self
    }

    /// 立即重扫（在后台进行）。
    pub fn refresh(&self) {
        lock(&self.0.sig.m).kick = true;
        self.0.sig.cv.notify_all();
    }

    /// 换成另一个输出文件夹；同一个则不动。
    pub fn set_dir(&self, dir: &Path) {
        {
            let mut st = lock(&self.0.st);
            if st.dir == dir {
                return;
            }
            st.dir = dir.to_path_buf();
            st.tracks.clear();
            st.scanning = true;
        }
        self.refresh();
    }

    pub fn view(&self) -> LibraryView {
        view_of(&lock(&self.0.st))
    }

    pub fn track(&self, id: &str) -> Option<Track> {
        lock(&self.0.st).tracks.get(Path::new(id)).cloned()
    }
}
