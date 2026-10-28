//! 原生播放器：播放队列规则（`PlayQueue`）与 rodio 输出（`Player`）。不依赖 tauri。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use rodio::source::EmptyCallback;
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Source};
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
        use rand::seq::SliceRandom;
        let cur = self.current().map(str::to_owned);
        if on {
            let mut rest: Vec<String> = self
                .ids
                .iter()
                .filter(|i| Some(*i) != cur.as_ref())
                .cloned()
                .collect();
            rest.shuffle(&mut rand::rng());
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
    ended: mpsc::Sender<u64>,
}

struct Inner {
    data_dir: PathBuf,
    on_change: Box<dyn Fn(PlayerState) + Send + Sync>,
    st: Mutex<St>,
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
                ended,
            }),
        });
        // 曲终前进与播放中每秒推送都在这条线程上；Player 丢掉后它随之退出。
        let weak = Arc::downgrade(&inner);
        std::thread::spawn(move || loop {
            let msg = rx.recv_timeout(Duration::from_secs(1));
            let Some(inner) = weak.upgrade() else { return };
            let p = Player(inner);
            match msg {
                Ok(gen) => p.ended(gen),
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
            let ms = if st.duration_ms > 0 { ms.min(st.duration_ms) } else { ms };
            if let Some(s) = &st.sink {
                let _ = s.try_seek(Duration::from_millis(ms));
            }
        });
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

    fn ended(&self, gen: u64) {
        self.edit(|st| {
            if st.gen != gen || st.sink.is_none() {
                return;
            }
            match st.queue.as_mut().and_then(|q| q.next(true)).map(str::to_owned) {
                Some(id) => st.load(id),
                None => st.halt(),
            }
        });
    }

    fn lock(&self) -> MutexGuard<'_, St> {
        self.0.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn notify(&self) {
        (self.0.on_change)(self.state());
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
            let name = Path::new(&id).file_name().map_or(id.clone(), |n| {
                n.to_string_lossy().into_owned()
            });
            self.error = Some(format!("{name}：{e}，已跳过"));
            let q = self.queue.as_mut().expect("load 只在有队列时调用");
            q.remove(&id);
            match q.current() {
                Some(c) => id = c.to_owned(),
                None => {
                    self.track = None;
                    self.duration_ms = 0;
                    return self.halt();
                }
            }
        }
    }

    fn open(&mut self, id: &str) -> Result<(), &'static str> {
        let path = Path::new(id);
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
        let dec = b.build().map_err(|_| "文件已损坏或格式不支持")?;
        let duration_ms = dec
            .total_duration()
            .map(|d| d.as_millis() as u64)
            .filter(|&d| d > 0)
            .or_else(|| reveal_core::read_track(path).ok().map(|t| t.duration_ms))
            .unwrap_or(0);
        let out = self.out.as_ref().expect("设备已打开");
        let sink = rodio::Player::connect_new(out.mixer());
        sink.set_volume(self.prefs.volume);
        self.gen += 1;
        let (gen, tx) = (self.gen, self.ended.clone());
        sink.append(dec);
        sink.append(EmptyCallback::new(Box::new(move || {
            let _ = tx.send(gen);
        })));
        self.sink = Some(sink);
        self.track = Some(id.to_owned());
        self.playing = true;
        self.duration_ms = duration_ms;
        Ok(())
    }
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
