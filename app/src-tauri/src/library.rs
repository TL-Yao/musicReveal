//! 资料库：输出文件夹（含子文件夹）里实际存在的可播放文件。后台线程在被叫醒或定时轮询时扫描，
//! 内容变了才整份推送。

use crate::engine::scan::{self, Stat};
use crate::engine::store;
use crate::engine::{JobState, Snapshot};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// 自动轮询的间隔。
pub const POLL: Duration = Duration::from_secs(2);
/// 扫描期间部分结果的推送间隔。
const PARTIAL: Duration = Duration::from_millis(250);
/// 封面缩略图长边上限：歌词视图与「正在播放」用。
const COVER_PX: u32 = 600;
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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Lyric {
    pub ms: u64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trans: Option<String>,
}

/// 读 .lrc 并解析；读不了或没有带时间的行为 None。
fn parse_lrc(p: &Path) -> Option<Vec<Lyric>> {
    let text = std::fs::read(p).ok()?;
    let v: Vec<Lyric> = reveal_core::parse_lrc(&String::from_utf8_lossy(&text))
        .into_iter()
        .map(|l| Lyric {
            ms: l.ms,
            text: l.text,
            trans: l.trans,
        })
        .collect();
    (!v.is_empty()).then_some(v)
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
    /// 封面缩略图在 covers 目录里的文件名。
    cover: Option<String>,
}

struct State {
    dir: PathBuf,
    tracks: BTreeMap<PathBuf, Track>,
    scanning: bool,
    /// 上一份快照里已完成的任务 id。
    done: HashSet<String>,
    /// 换目录、删歌时加一：之前开始的扫描结果过时，不再写进 `tracks`。
    epoch: u64,
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

/// 资料库封面缩略图目录（asset 协议要放行它）。
pub fn covers_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("library-covers")
}

/// 用 macOS 自带的 sips 把封面转成 JPEG，长边超过 `COVER_PX` 才缩小。
fn thumbnail(cover: &[u8], jpg: &Path) -> bool {
    use std::process::{Command, Stdio};
    let src = jpg.with_extension("src");
    let ok = (|| {
        std::fs::create_dir_all(jpg.parent()?).ok()?;
        std::fs::write(&src, cover).ok()?;
        let o = Command::new("sips")
            .args(["-g", "pixelWidth", "-g", "pixelHeight"])
            .arg(&src)
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let long = String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| l.split_once(':'))
            .filter(|(k, _)| k.trim().starts_with("pixel"))
            .filter_map(|(_, v)| v.trim().parse::<u32>().ok())
            .max()?;
        let mut c = Command::new("sips");
        if long > COVER_PX {
            c.args(["-Z", &COVER_PX.to_string()]);
        }
        c.args(["-s", "format", "jpeg"])
            .arg(&src)
            .arg("--out")
            .arg(jpg)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?
            .success()
            .then_some(())
    })()
    .is_some();
    let _ = std::fs::remove_file(&src);
    ok
}

/// 缩略图文件名随路径、大小、mtime 变：文件变了 URL 也变，webview 不会拿旧图。
fn cover_name(path: &Path, stat: Stat) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (path, stat).hash(&mut h);
    format!("{:016x}.jpg", h.finish())
}

fn read(path: &Path, stat: Stat, covers: &Path) -> Option<Rec> {
    let i = reveal_core::read_track(path).ok()?;
    let m = std::fs::metadata(path).ok()?;
    let cover = i
        .has_cover
        .then(|| reveal_core::read_cover(path))
        .flatten()
        .map(|c| (c, cover_name(path, stat)))
        .filter(|(c, n)| thumbnail(c, &covers.join(n)))
        .map(|(_, n)| n);
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
        cover,
    })
}

fn track_of(r: &Rec, covers: &Path) -> Track {
    let cover = r.cover.as_ref().map(|n| covers.join(n));
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
        cover_url: cover.as_deref().map(crate::engine::asset_url),
        has_lyrics: false,
        added_at: r.added_at,
        cover,
        lrc: None,
    }
}

/// 后台扫描线程自己的状态。
struct Worker {
    /// library.json：标签缓存。
    file: PathBuf,
    covers: PathBuf,
    cache: HashMap<PathBuf, Rec>,
    /// 缓存有没写盘的变化。
    dirty: bool,
    /// 读不了的文件：没变就不再试。
    bad: HashMap<PathBuf, Stat>,
    /// .lrc 能否解析出歌词，按大小 + mtime 记。
    lrc: HashMap<PathBuf, (Stat, bool)>,
    last: Option<LibraryView>,
}

impl Worker {
    fn new(data_dir: &Path) -> Self {
        let file = data_dir.join("library.json");
        let covers = covers_dir(data_dir);
        let mut recs: Vec<Rec> = store::load(&file);
        // 缩略图没了的条目重读；没人用的缩略图删掉
        recs.retain(|r| r.cover.as_ref().is_none_or(|n| covers.join(n).exists()));
        let used: HashSet<&str> = recs.iter().filter_map(|r| r.cover.as_deref()).collect();
        for e in std::fs::read_dir(&covers).into_iter().flatten().flatten() {
            if !used.contains(&*e.file_name().to_string_lossy()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
        Worker {
            file,
            covers,
            cache: recs.into_iter().map(|r| (r.path.clone(), r)).collect(),
            dirty: false,
            bad: HashMap::new(),
            lrc: HashMap::new(),
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
        let (dir, epoch) = {
            let st = lock(&lib.st);
            (st.dir.clone(), st.epoch)
        };
        let found = if dir.as_os_str().is_empty() {
            vec![]
        } else {
            scan::files(&dir, &keep)
        };
        // 去掉扩展名的路径 → 同名 .lrc
        let mut lrcs: HashMap<PathBuf, (PathBuf, Stat)> = HashMap::new();
        let mut audio = vec![];
        for (p, s) in found {
            if ext(&p) == "lrc" {
                lrcs.insert(p.with_extension(""), (p, s));
            } else {
                audio.push((p, s));
            }
        }
        self.lrc.retain(|p, (s, _)| {
            lrcs.get(&p.with_extension(""))
                .is_some_and(|l| &l.0 == p && l.1 == *s)
        });
        // 锁外解析：之后持锁时 `lyrics` 只查表
        for (p, s) in lrcs.values() {
            self.lrc
                .entry(p.clone())
                .or_insert_with(|| (*s, parse_lrc(p).is_some()));
        }
        let now: HashMap<&PathBuf, Stat> = audio.iter().map(|(p, s)| (p, *s)).collect();
        let covers = &self.covers;
        let before = self.cache.len();
        self.cache.retain(|p, r| {
            now.contains_key(p) || {
                drop_cover(covers, r);
                false
            }
        });
        self.dirty |= self.cache.len() != before;
        self.bad.retain(|p, s| now.get(p) == Some(s));
        let mut known = BTreeMap::new();
        let mut misses = vec![];
        for (p, s) in &audio {
            match self.cache.get(p) {
                Some(r) if r.stat == *s => {
                    known.insert(p.clone(), track_of(r, &self.covers));
                }
                _ if self.bad.get(p) == Some(s) => {}
                _ => misses.push((p.clone(), *s)),
            }
        }
        {
            let mut st = lock(&lib.st);
            if st.epoch != epoch {
                return;
            }
            // 正在重读的歌先保留旧样子
            for (p, _) in &misses {
                if let Some(t) = st.tracks.get(p) {
                    known.insert(p.clone(), t.clone());
                }
            }
            for t in known.values_mut() {
                lyrics(&mut self.lrc, &lrcs, t);
            }
            st.tracks = known;
            st.scanning = !misses.is_empty();
        }
        // 推送前落盘：收到事件时缓存已是新的
        self.save();
        self.emit(lib);
        let mut shown = Instant::now();
        for (p, s) in misses {
            let rec = read(&p, s, &self.covers);
            let mut st = lock(&lib.st);
            if st.epoch != epoch {
                return;
            }
            match rec {
                Some(r) => {
                    let mut t = track_of(&r, &self.covers);
                    lyrics(&mut self.lrc, &lrcs, &mut t);
                    st.tracks.insert(p.clone(), t);
                    if let Some(old) = self.cache.insert(p, r) {
                        drop_cover(&self.covers, &old);
                    }
                    self.dirty = true;
                }
                None => {
                    st.tracks.remove(&p);
                    if let Some(old) = self.cache.remove(&p) {
                        drop_cover(&self.covers, &old);
                    }
                    self.bad.insert(p, s);
                    self.dirty = true;
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

/// 填上同名 .lrc；能否解析出歌词按 .lrc 的大小 + mtime 记在 `seen` 里。
fn lyrics(
    seen: &mut HashMap<PathBuf, (Stat, bool)>,
    lrcs: &HashMap<PathBuf, (PathBuf, Stat)>,
    t: &mut Track,
) {
    let found = lrcs.get(&Path::new(&t.id).with_extension(""));
    t.lrc = found.map(|l| l.0.clone());
    t.has_lyrics = found.is_some_and(|(p, s)| {
        seen.entry(p.clone())
            .or_insert_with(|| (*s, parse_lrc(p).is_some()))
            .1
    });
}

/// NSFileManager 移到废纸篓：不经 Finder，不会弹「想要控制 Finder」的授权。
fn trash(p: &Path) -> Result<(), String> {
    use objc2_foundation::{NSFileManager, NSURL};
    let url = NSURL::from_file_path(p).ok_or("路径无效")?;
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|e| format!("无法移到废纸篓：{}", e.localizedDescription()))
}

fn drop_cover(covers: &Path, r: &Rec) {
    if let Some(n) = &r.cover {
        let _ = std::fs::remove_file(covers.join(n));
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
                done: HashSet::new(),
                epoch: 0,
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
            st.epoch += 1;
            st.tracks.clear();
            st.scanning = true;
        }
        self.refresh();
    }

    /// 跟着引擎快照走：输出文件夹变了就换过去；有任务新完成就立即重扫，好让新歌尽快出现。
    pub fn observe(&self, s: &Snapshot) {
        self.set_dir(Path::new(&s.output_dir));
        let done: HashSet<String> = s
            .jobs
            .iter()
            .filter(|j| j.state == JobState::Done)
            .map(|j| j.id.clone())
            .collect();
        let fresh = {
            let mut st = lock(&self.0.st);
            let fresh = !done.is_subset(&st.done);
            st.done = done;
            fresh
        };
        if fresh {
            self.refresh();
        }
    }

    pub fn view(&self) -> LibraryView {
        view_of(&lock(&self.0.st))
    }

    pub fn track(&self, id: &str) -> Option<Track> {
        lock(&self.0.st).tracks.get(Path::new(id)).cloned()
    }

    /// 歌与同名 .lrc 移到废纸篓，立即从资料库去掉。只认资料库里的歌。
    pub fn trash(&self, id: &str) -> Result<(), String> {
        let t = self.track(id).ok_or("不在资料库里")?;
        trash(Path::new(&t.id))?;
        if let Some(l) = &t.lrc {
            let _ = trash(l);
        }
        {
            let mut st = lock(&self.0.st);
            st.tracks.remove(Path::new(id));
            st.epoch += 1;
        }
        self.refresh();
        Ok(())
    }

    /// 在 Finder 里显示。只认资料库里的歌。
    pub fn reveal(&self, id: &str) -> Result<(), String> {
        self.track(id).ok_or("不在资料库里")?;
        std::process::Command::new("open")
            .arg("-R")
            .arg(id)
            .spawn()
            .map(drop)
            .map_err(|e| format!("无法打开 Finder：{e}"))
    }

    /// 资料库里这首歌的同名 .lrc 解析出的歌词；没有或解析为空为 None。
    pub fn lyrics(&self, id: &str) -> Option<Vec<Lyric>> {
        parse_lrc(&self.track(id)?.lrc?)
    }
}
