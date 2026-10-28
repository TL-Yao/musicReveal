//! 原生播放器：播放队列规则（`PlayQueue`）与 rodio 输出（`Player`）。不依赖 tauri。

use rodio::source::EmptyCallback;
pub use rodio::Source;
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl std::str::FromStr for Repeat {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "off" => Ok(Repeat::Off),
            "all" => Ok(Repeat::All),
            "one" => Ok(Repeat::One),
            _ => Err(format!("未知的循环模式：{s}")),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PrevAction<'a> {
    Restart,
    Go(&'a str),
}

/// 播放顺序与循环规则，不碰音频。曲目 id 即文件绝对路径。
pub struct PlayQueue {
    /// 原顺序。
    ids: Vec<String>,
    /// 实际播放顺序（随机时打乱）。
    order: Vec<String>,
    /// `order` 下标；等于 `order.len()` 表示没有当前曲。
    pos: usize,
    repeat: Repeat,
}

impl PlayQueue {
    pub fn new(ids: Vec<String>, index: usize, shuffle: bool, repeat: Repeat) -> Self {
        let pos = index.min(ids.len());
        let mut q = PlayQueue {
            order: ids.clone(),
            ids,
            pos,
            repeat,
        };
        if shuffle {
            q.set_shuffle(true);
        }
        q
    }

    pub fn current(&self) -> Option<&str> {
        self.order.get(self.pos).map(String::as_str)
    }

    /// 前进一首；`auto` 为曲终自动前进。到队尾且不循环时返回 None，当前曲不变。
    pub fn next(&mut self, auto: bool) -> Option<&str> {
        if self.pos >= self.order.len() {
            return None;
        }
        if auto && self.repeat == Repeat::One {
            return self.current();
        }
        if self.pos + 1 < self.order.len() {
            self.pos += 1;
        } else if self.repeat == Repeat::Off {
            return None;
        } else {
            self.pos = 0;
        }
        self.current()
    }

    pub fn prev(&mut self, position_ms: u64) -> PrevAction<'_> {
        if position_ms > 3000 || self.pos >= self.order.len() {
            return PrevAction::Restart;
        }
        if self.pos > 0 {
            self.pos -= 1;
        } else if self.repeat == Repeat::All {
            self.pos = self.order.len() - 1;
        } else {
            return PrevAction::Restart;
        }
        PrevAction::Go(&self.order[self.pos])
    }

    /// 开：当前曲排第一，其余打乱；关：回到原顺序，从当前曲之后继续。
    pub fn set_shuffle(&mut self, on: bool) {
        let cur = self.current().map(str::to_owned);
        if on {
            let mut rest: Vec<String> = self
                .ids
                .iter()
                .filter(|i| Some(*i) != cur.as_ref())
                .cloned()
                .collect();
            shuffle(&mut rest);
            self.pos = if cur.is_some() { 0 } else { rest.len() };
            self.order = cur.into_iter().chain(rest).collect();
        } else {
            self.order = self.ids.clone();
            self.pos = cur
                .and_then(|c| self.order.iter().position(|i| *i == c))
                .unwrap_or(self.order.len());
        }
    }

    pub fn set_repeat(&mut self, r: Repeat) {
        self.repeat = r;
    }

    /// 曲目失效：移出队列。移走的是当前曲时，当前曲变成它之后的那首。
    pub fn remove(&mut self, id: &str) {
        self.ids.retain(|i| i != id);
        let Some(i) = self.order.iter().position(|x| x == id) else {
            return;
        };
        self.order.remove(i);
        if i < self.pos {
            self.pos -= 1;
        } else if i == self.pos && self.pos == self.order.len() && self.repeat == Repeat::All {
            self.pos = 0;
        }
    }
}

/// Fisher–Yates；随机数取自每次新建的 `RandomState`（SipHash 随机密钥）。
fn shuffle(v: &mut [String]) {
    use std::hash::BuildHasher;
    let s = std::hash::RandomState::new();
    for i in (1..v.len()).rev() {
        v.swap(i, (s.hash_one(i) % (i as u64 + 1)) as usize);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_id: Option<String>,
    pub playing: bool,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub volume: f32,
    pub shuffle: bool,
    pub repeat: Repeat,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Prefs {
    volume: f32,
    shuffle: bool,
    repeat: Repeat,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            volume: 0.8,
            shuffle: false,
            repeat: Repeat::Off,
        }
    }
}

struct St {
    prefs: Prefs,
    queue: Option<PlayQueue>,
    track: Option<String>,
    playing: bool,
    duration_ms: u64,
    error: Option<String>,
    /// 音频设备，第一次播放时才打开。
    out: Option<MixerDeviceSink>,
    /// 当前曲目的输出；每首新建一个，换曲时丢掉旧的即停。
    sink: Option<rodio::Player>,
    /// 曲目编号：曲终回调带着它，过期的回调据此忽略。
    gen: u64,
    /// 已放完的曲目编号：曲终回调在音频线程上先记下它，再发消息。
    finished: Arc<AtomicU64>,
    ended: mpsc::Sender<Msg>,
    /// ffmpeg 兜底解码的缓存目录。
    cache: PathBuf,
}

/// 发给后台线程的消息，都带曲目编号。
enum Msg {
    Ended(u64),
    /// ffmpeg 兜底解码完成；None 为解不了。
    Decoded(u64, Option<PathBuf>),
}

struct Inner {
    data_dir: PathBuf,
    on_change: Box<dyn Fn(PlayerState) + Send + Sync>,
    st: Mutex<St>,
    /// 系统媒体键与「正在播放」；只有 App 里开（测试不碰系统）。
    media: Mutex<Option<Media>>,
    /// 后台线程处理每条消息前先拿它；测试借此把消息压在队列里。
    hold: Mutex<()>,
}

pub struct Player(Arc<Inner>);

impl Player {
    /// 偏好从 `<data_dir>/player.json` 读，读不到用默认值。
    pub fn new(data_dir: PathBuf, on_change: Box<dyn Fn(PlayerState) + Send + Sync>) -> Self {
        let prefs = std::fs::read(data_dir.join(PREFS))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let (ended, rx) = mpsc::channel();
        let cache = data_dir.join("decoded");
        let inner = Arc::new(Inner {
            data_dir,
            on_change,
            st: Mutex::new(St {
                prefs,
                queue: None,
                track: None,
                playing: false,
                duration_ms: 0,
                error: None,
                out: None,
                sink: None,
                gen: 0,
                finished: Arc::new(AtomicU64::new(0)),
                ended,
                cache,
            }),
            media: Mutex::new(None),
            hold: Mutex::new(()),
        });
        // 曲终前进与播放中每秒推送都在这条线程上；Player 丢掉后它随之退出。
        let weak = Arc::downgrade(&inner);
        std::thread::spawn(move || loop {
            let msg = rx.recv_timeout(Duration::from_secs(1));
            let Some(inner) = weak.upgrade() else { return };
            drop(inner.hold.lock().unwrap_or_else(|e| e.into_inner()));
            let p = Player(inner);
            match msg {
                Ok(Msg::Ended(gen)) => p.ended(gen),
                Ok(Msg::Decoded(gen, r)) => p.decoded(gen, r),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if p.lock().playing {
                        p.notify();
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        });
        Player(inner)
    }

    /// 从 `ids[index]` 开始播放这个队列。失效的曲目跳过，原因写进 `error`。
    pub fn play(&self, ids: Vec<String>, index: usize) -> Result<(), String> {
        if index >= ids.len() {
            return Err("没有可播放的曲目".into());
        }
        self.edit(|st| {
            st.error = None;
            let q = PlayQueue::new(ids, index, st.prefs.shuffle, st.prefs.repeat);
            let id = q.current().map(str::to_owned);
            st.queue = Some(q);
            if let Some(id) = id {
                st.load(id);
            }
        });
        Ok(())
    }

    /// 播放/暂停；停止状态下从当前曲开头播。
    pub fn toggle(&self) {
        self.edit(|st| {
            st.error = None;
            match &st.sink {
                Some(s) if st.playing => {
                    s.pause();
                    st.playing = false;
                }
                Some(s) => {
                    s.play();
                    st.playing = true;
                }
                // 兜底解码中按暂停：取消；解出的文件留在缓存里，再按播放可复用
                None if st.playing => st.halt(),
                None => {
                    if let Some(id) = st.current() {
                        st.load(id);
                    }
                }
            }
        });
    }

    pub fn next(&self) {
        self.edit(|st| {
            st.error = None;
            let Some(q) = st.queue.as_mut() else { return };
            match q.next(false).map(str::to_owned) {
                Some(id) => st.load(id),
                None => st.halt(),
            }
        });
    }

    pub fn prev(&self) {
        self.edit(|st| {
            st.error = None;
            let pos = st.position();
            let Some(q) = st.queue.as_mut() else { return };
            match q.prev(pos) {
                PrevAction::Go(id) => {
                    let id = id.to_owned();
                    st.load(id);
                }
                PrevAction::Restart => st.restart(),
            }
        });
    }

    pub fn seek(&self, ms: u64) {
        self.edit(|st| {
            let ms = if st.duration_ms > 0 {
                ms.min(st.duration_ms)
            } else {
                ms
            };
            let pos = Duration::from_millis(ms);
            if let Some(s) = st.sink.as_ref().filter(|_| !st.ended()) {
                let _ = s.try_seek(pos);
            }
            // 本曲已放完（曲终消息可能已在路上）：重开再定位；新编号让那条消息作废
            if st.ended() {
                if let Some(id) = st.track.clone() {
                    st.load(id);
                    if let Some(s) = &st.sink {
                        let _ = s.try_seek(pos);
                    }
                }
            }
        });
    }

    /// 测试用：持有期间后台线程不处理曲终等消息。
    #[doc(hidden)]
    pub fn hold_events(&self) -> MutexGuard<'_, ()> {
        self.0.hold.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 退出前调用：停播并关掉音频设备。
    pub fn stop(&self) {
        self.edit(|st| {
            st.halt();
            st.out = None;
        });
    }

    pub fn state(&self) -> PlayerState {
        self.lock().view()
    }

    pub fn set_volume(&self, v: f32) {
        self.edit_prefs(|st| {
            st.prefs.volume = v.clamp(0.0, 1.0);
            if let Some(s) = &st.sink {
                s.set_volume(st.prefs.volume);
            }
        });
    }

    pub fn set_shuffle(&self, on: bool) {
        self.edit_prefs(|st| {
            st.prefs.shuffle = on;
            if let Some(q) = st.queue.as_mut() {
                q.set_shuffle(on);
            }
        });
    }

    pub fn set_repeat(&self, r: Repeat) {
        self.edit_prefs(|st| {
            st.prefs.repeat = r;
            if let Some(q) = st.queue.as_mut() {
                q.set_repeat(r);
            }
        });
    }

    /// 兜底解码完成：还是这首就开播，解不了就跳过。
    fn decoded(&self, gen: u64, r: Option<PathBuf>) {
        self.edit(|st| {
            let Some(id) = st.track.clone().filter(|_| st.gen == gen) else {
                return;
            };
            match r.ok_or(CORRUPT).and_then(|p| decoder(&p)) {
                Ok(dec) => {
                    let ms = st.duration_ms;
                    st.start(&id, dec, ms);
                }
                Err(e) => {
                    if let Some(next) = st.skip(&id, e) {
                        st.load(next);
                    }
                }
            }
        });
    }

    fn ended(&self, gen: u64) {
        self.edit(|st| {
            if st.gen != gen || st.sink.is_none() {
                return;
            }
            match st
                .queue
                .as_mut()
                .and_then(|q| q.next(true))
                .map(str::to_owned)
            {
                Some(id) => st.load(id),
                None => st.halt(),
            }
        });
    }

    fn lock(&self) -> MutexGuard<'_, St> {
        self.0.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn notify(&self) {
        let s = self.state();
        if let Some(m) = self
            .0
            .media
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            m.update(&s);
        }
        (self.0.on_change)(s);
    }

    /// 接入系统媒体键（播放/暂停/上一首/下一首/拖动进度）与控制中心「正在播放」。
    pub fn enable_media_controls(&self) -> Result<(), String> {
        use souvlaki::{MediaControlEvent as E, MediaControls, PlatformConfig};
        let mut controls = MediaControls::new(PlatformConfig {
            display_name: "musicReveal",
            dbus_name: "musicreveal",
            hwnd: None,
        })
        .map_err(|e| format!("无法接入媒体键：{e}"))?;
        let weak = Arc::downgrade(&self.0);
        controls
            .attach(move |ev| {
                let Some(inner) = weak.upgrade() else { return };
                let p = Player(inner);
                match ev {
                    E::Toggle => p.toggle(),
                    E::Play | E::Pause | E::Stop => {
                        if p.state().playing != (ev == E::Play) {
                            p.toggle();
                        }
                    }
                    E::Next => p.next(),
                    E::Previous => p.prev(),
                    E::SetPosition(souvlaki::MediaPosition(d)) => p.seek(d.as_millis() as u64),
                    _ => {}
                }
            })
            .map_err(|e| format!("无法接入媒体键：{e}"))?;
        *self.0.media.lock().unwrap_or_else(|e| e.into_inner()) = Some(Media {
            controls,
            track: None,
            cover_dir: self.0.data_dir.join("now-playing"),
        });
        self.notify();
        Ok(())
    }

    /// 改状态，再在锁外通知。
    fn edit<T>(&self, f: impl FnOnce(&mut St) -> T) -> T {
        let r = f(&mut self.lock());
        self.notify();
        r
    }

    /// 改偏好并落盘（在锁内写，天然串行）。
    fn edit_prefs(&self, f: impl FnOnce(&mut St)) {
        let dir = self.0.data_dir.clone();
        self.edit(|st| {
            f(st);
            save(&dir, &st.prefs);
        });
    }
}

impl St {
    fn view(&self) -> PlayerState {
        PlayerState {
            track_id: self.track.clone(),
            playing: self.playing,
            position_ms: self.position(),
            duration_ms: self.duration_ms,
            volume: self.prefs.volume,
            shuffle: self.prefs.shuffle,
            repeat: self.prefs.repeat,
            error: self.error.clone(),
        }
    }

    fn position(&self) -> u64 {
        let ms = self
            .sink
            .as_ref()
            .map_or(0, |s| s.get_pos().as_millis() as u64);
        if self.duration_ms > 0 {
            ms.min(self.duration_ms)
        } else {
            ms
        }
    }

    /// 当前曲的音源已放完。
    fn ended(&self) -> bool {
        self.sink.is_some() && self.finished.load(Ordering::SeqCst) == self.gen
    }

    fn current(&self) -> Option<String> {
        self.queue.as_ref()?.current().map(str::to_owned)
    }

    /// 停止：不再出声，位置归零，曲目保留。
    fn halt(&mut self) {
        self.sink = None;
        self.playing = false;
        self.gen += 1;
    }

    fn restart(&mut self) {
        let ok = self
            .sink
            .as_ref()
            .is_some_and(|s| s.try_seek(Duration::ZERO).is_ok());
        if !ok {
            if let Some(id) = self.current() {
                self.load(id);
            }
        }
    }

    /// 播放 `id`；打不开就移出队列试下一首，全部失败则停止。
    fn load(&mut self, mut id: String) {
        if self.out.is_none() {
            match DeviceSinkBuilder::open_default_sink() {
                Ok(mut o) => {
                    o.log_on_drop(false);
                    self.out = Some(o);
                }
                Err(_) => {
                    self.error = Some("无法打开音频设备".into());
                    return self.halt();
                }
            }
        }
        loop {
            let Err(e) = self.open(&id) else { return };
            match self.skip(&id, e) {
                Some(next) => id = next,
                None => return,
            }
        }
    }

    /// `id` 播不了：记下原因、移出队列，返回接着该播的；没有了就停。
    fn skip(&mut self, id: &str, e: &str) -> Option<String> {
        // 界面不显示文件名：用歌名
        let name = Path::new(id).file_name().map_or(id.to_owned(), |n| {
            reveal_core::title_from_file_name(&n.to_string_lossy())
        });
        self.error = Some(format!("「{name}」{e}，已跳过"));
        let q = self.queue.as_mut().expect("只在有队列时调用");
        q.remove(id);
        let next = q.current().map(str::to_owned);
        if next.is_none() {
            self.track = None;
            self.duration_ms = 0;
            self.halt();
        }
        next
    }

    /// 开播 `id`；rodio 解不了的交给后台 ffmpeg 解码（`Msg::Decoded` 回来再播）。
    fn open(&mut self, id: &str) -> Result<(), &'static str> {
        let path = Path::new(id);
        let from_tags = || reveal_core::read_track(path).ok().map(|t| t.duration_ms);
        let dec = match decoder(path) {
            Ok(d) => d,
            Err(CORRUPT) => {
                self.sink = None;
                self.gen += 1;
                let (gen, tx, src, cache) = (
                    self.gen,
                    self.ended.clone(),
                    path.to_owned(),
                    self.cache.clone(),
                );
                std::thread::spawn(move || {
                    let _ = tx.send(Msg::Decoded(gen, transcode(&src, &cache)));
                });
                self.track = Some(id.to_owned());
                self.playing = true;
                self.duration_ms = from_tags().unwrap_or(0);
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let duration_ms = dec
            .total_duration()
            .map(|d| d.as_millis() as u64)
            .filter(|&d| d > 0)
            .or_else(from_tags)
            .unwrap_or(0);
        self.start(id, dec, duration_ms);
        Ok(())
    }

    fn start(&mut self, id: &str, dec: Audio, duration_ms: u64) {
        let out = self.out.as_ref().expect("设备已打开");
        let sink = rodio::Player::connect_new(out.mixer());
        sink.set_volume(self.prefs.volume);
        self.gen += 1;
        let (gen, tx, done) = (self.gen, self.ended.clone(), self.finished.clone());
        sink.append(dec);
        sink.append(EmptyCallback::new(Box::new(move || {
            done.store(gen, Ordering::SeqCst);
            let _ = tx.send(Msg::Ended(gen));
        })));
        self.sink = Some(sink);
        self.track = Some(id.to_owned());
        self.playing = true;
        self.duration_ms = duration_ms;
    }
}
pub type Audio = Decoder<std::io::BufReader<std::fs::File>>;

/// rodio 直接解码。
fn decoder(path: &Path) -> Result<Audio, &'static str> {
    let file = std::fs::File::open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "文件不存在",
        _ => "无法读取文件",
    })?;
    let len = file.metadata().map_or(0, |m| m.len());
    let mut b = Decoder::builder()
        .with_data(std::io::BufReader::new(file))
        .with_byte_len(len)
        .with_seekable(true);
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        b = b.with_hint(ext);
    }
    b.build().map_err(|_| CORRUPT)
}

const CORRUPT: &str = "文件已损坏或格式不支持";

/// 能播的音源：先 rodio，解不了再 ffmpeg 兜底（同步版，播放器在后台线程里走同一条路）。
pub fn open_source(path: &Path, cache: &Path) -> Result<Audio, &'static str> {
    match decoder(path) {
        Err(CORRUPT) => decoder(&transcode(path, cache).ok_or(CORRUPT)?),
        r => r,
    }
}

/// rodio 解不了的编码（如 Opus）用 ffmpeg 解成 FLAC 放进 `cache`：按路径 + 大小 + mtime 命名可复用，
/// 只留最近解的一个。ffmpeg 也解不了为 None。
fn transcode(path: &Path, cache: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    use std::sync::atomic::AtomicU32;
    static N: AtomicU32 = AtomicU32::new(0);
    let m = std::fs::metadata(path).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (path, m.len(), m.modified().ok()).hash(&mut h);
    let out = cache.join(format!("{:016x}.flac", h.finish()));
    if out.exists() {
        return Some(out);
    }
    let ff = reveal_core::find_ffmpeg()?;
    std::fs::create_dir_all(cache).ok()?;
    for e in std::fs::read_dir(cache).ok()?.flatten() {
        let _ = std::fs::remove_file(e.path());
    }
    let tmp = cache.join(format!(".{}.tmp", N.fetch_add(1, Ordering::Relaxed)));
    let ok = std::process::Command::new(ff)
        .args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(path)
        .args([
            "-map",
            "0:a:0",
            "-c:a",
            "flac",
            "-compression_level",
            "0",
            "-f",
            "flac",
        ])
        .arg(&tmp)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok && std::fs::rename(&tmp, &out).is_ok() {
        return Some(out);
    }
    let _ = std::fs::remove_file(&tmp);
    None
}

struct Media {
    controls: souvlaki::MediaControls,
    /// 已经交给系统的曲目。
    track: Option<String>,
    cover_dir: PathBuf,
}

impl Media {
    fn update(&mut self, s: &PlayerState) {
        use souvlaki::{MediaMetadata, MediaPlayback, MediaPosition};
        if s.track_id != self.track {
            self.track = s.track_id.clone();
            let _ = std::fs::remove_dir_all(&self.cover_dir);
            let meta = match &s.track_id {
                Some(id) => {
                    let path = Path::new(id);
                    let t = reveal_core::read_track(path).ok();
                    let title = t.as_ref().and_then(|t| t.title.clone()).unwrap_or_else(|| {
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        reveal_core::title_from_file_name(&name)
                    });
                    let artist = t.as_ref().map(|t| t.artists.join("、"));
                    let album = t.as_ref().and_then(|t| t.album.clone());
                    let cover = reveal_core::read_cover(path).and_then(|b| {
                        // 每首一个文件名：系统按 URL 缓存图片
                        let f = self.cover_dir.join(cover_name(id));
                        std::fs::create_dir_all(&self.cover_dir).ok()?;
                        std::fs::write(&f, b).ok()?;
                        Some(file_url(&f))
                    });
                    Some((title, artist, album, cover))
                }
                None => None,
            };
            let m = meta.as_ref();
            let _ = self.controls.set_metadata(MediaMetadata {
                title: m.map(|m| m.0.as_str()),
                artist: m.and_then(|m| m.1.as_deref()).filter(|a| !a.is_empty()),
                album: m.and_then(|m| m.2.as_deref()),
                cover_url: m.and_then(|m| m.3.as_deref()),
                duration: m.map(|_| Duration::from_millis(s.duration_ms)),
            });
        }
        let progress = Some(MediaPosition(Duration::from_millis(s.position_ms)));
        let _ = self.controls.set_playback(match (&s.track_id, s.playing) {
            (None, _) => MediaPlayback::Stopped,
            (_, true) => MediaPlayback::Playing { progress },
            (_, false) => MediaPlayback::Paused { progress },
        });
    }
}

fn cover_name(id: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// 百分号编码的 file:// URL（数据目录里有空格）。
fn file_url(p: &Path) -> String {
    let mut s = String::from("file://");
    for b in p.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

const PREFS: &str = "player.json";

/// 写临时文件再 rename。
fn save(dir: &Path, prefs: &Prefs) {
    let tmp = dir.join(format!("{PREFS}.{}.tmp", std::process::id()));
    let r = (|| {
        std::fs::create_dir_all(dir)?;
        std::fs::write(&tmp, serde_json::to_vec(prefs)?)?;
        std::fs::rename(&tmp, dir.join(PREFS))
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}
