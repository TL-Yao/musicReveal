//! 原生播放器：播放队列规则（`PlayQueue`）与 rodio 输出（`Player`）。不依赖 tauri。

use serde::{Deserialize, Serialize};

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
