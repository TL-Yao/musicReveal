//! 原生播放器：播放队列规则（`PlayQueue`）与 rodio 输出（`Player`）。不依赖 tauri。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

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
        Player(Arc::new(Inner {
            data_dir,
            on_change,
            st: Mutex::new(St {
                prefs,
                queue: None,
                track: None,
                playing: false,
                duration_ms: 0,
                error: None,
            }),
        }))
    }

    pub fn state(&self) -> PlayerState {
        self.lock().view()
    }

    pub fn set_volume(&self, v: f32) {
        self.edit_prefs(|st| st.prefs.volume = v.clamp(0.0, 1.0));
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

    fn lock(&self) -> MutexGuard<'_, St> {
        self.0.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 改状态，再在锁外通知。
    fn edit<T>(&self, f: impl FnOnce(&mut St) -> T) -> T {
        let (r, view) = {
            let mut st = self.lock();
            let r = f(&mut st);
            (r, st.view())
        };
        (self.0.on_change)(view);
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
            position_ms: 0,
            duration_ms: self.duration_ms,
            volume: self.prefs.volume,
            shuffle: self.prefs.shuffle,
            repeat: self.prefs.repeat,
            error: self.error.clone(),
        }
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
