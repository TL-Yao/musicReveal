//! 播放器：`PlayQueue` 纯逻辑、偏好持久化；真实解码的用例需要音频设备，标 `#[ignore]`，
//! 本机用 `cargo test -p musicreveal-app --test player -- --ignored` 跑。测试全程音量 0、音频为静音。

use musicreveal_app::player::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "mr-player-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ids(n: usize) -> Vec<String> {
    (0..n).map(|i| i.to_string()).collect()
}

fn abc() -> Vec<String> {
    ["a", "b", "c"].map(String::from).to_vec()
}

fn q(index: usize, repeat: Repeat) -> PlayQueue {
    PlayQueue::new(abc(), index, false, repeat)
}

// ---------- PlayQueue ----------

#[test]
fn starts_at_index_in_original_order() {
    let mut p = q(1, Repeat::Off);
    assert_eq!(p.current(), Some("b"));
    assert_eq!(p.next(false), Some("c"));
    assert_eq!(p.current(), Some("c"));
}

#[test]
fn shuffle_keeps_current_first_and_plays_every_other_track_once() {
    let mut p = PlayQueue::new(ids(20), 5, true, Repeat::Off);
    assert_eq!(p.current(), Some("5"));
    let mut rest: Vec<String> = (0..19).map(|_| p.next(false).unwrap().to_owned()).collect();
    assert_eq!(p.next(false), None, "随机播放一轮后到队尾");
    rest.sort_by_key(|s| s.parse::<u32>().unwrap());
    let want: Vec<String> = ids(20).into_iter().filter(|s| s != "5").collect();
    assert_eq!(rest, want);
}

#[test]
fn shuffle_turned_on_mid_queue_keeps_current_and_covers_the_rest() {
    let mut p = PlayQueue::new(ids(10), 3, false, Repeat::Off);
    p.set_shuffle(true);
    assert_eq!(p.current(), Some("3"));
    let mut rest: Vec<String> = (0..9).map(|_| p.next(false).unwrap().to_owned()).collect();
    rest.sort_by_key(|s| s.parse::<u32>().unwrap());
    let want: Vec<String> = ids(10).into_iter().filter(|s| s != "3").collect();
    assert_eq!(rest, want);
}

#[test]
fn shuffle_turned_off_continues_after_current_in_original_order() {
    let mut p = PlayQueue::new(ids(20), 0, true, Repeat::Off);
    let cur: usize = p.next(false).unwrap().parse().unwrap();
    p.set_shuffle(false);
    assert_eq!(p.current(), Some(cur.to_string().as_str()));
    let want = if cur == 19 { None } else { Some((cur + 1).to_string()) };
    assert_eq!(p.next(false).map(str::to_owned), want);
}

#[test]
fn repeat_off_stops_at_end_and_keeps_last_track() {
    let mut p = q(2, Repeat::Off);
    assert_eq!(p.next(true), None);
    assert_eq!(p.next(false), None);
    assert_eq!(p.current(), Some("c"));
}

#[test]
fn repeat_all_wraps_for_auto_and_manual() {
    let mut p = q(2, Repeat::All);
    assert_eq!(p.next(true), Some("a"));
    let mut p = q(2, Repeat::All);
    assert_eq!(p.next(false), Some("a"));
}

#[test]
fn repeat_one_repeats_on_auto_but_manual_next_advances_and_wraps() {
    let mut p = q(1, Repeat::One);
    assert_eq!(p.next(true), Some("b"));
    assert_eq!(p.next(false), Some("c"));
    assert_eq!(p.next(false), Some("a"));
}

#[test]
fn set_repeat_changes_end_of_queue_behaviour() {
    let mut p = q(2, Repeat::Off);
    p.set_repeat(Repeat::All);
    assert_eq!(p.next(true), Some("a"));
}

#[test]
fn prev_restarts_after_three_seconds() {
    let mut p = q(1, Repeat::Off);
    assert_eq!(p.prev(3001), PrevAction::Restart);
    assert_eq!(p.current(), Some("b"));
    assert_eq!(p.prev(3000), PrevAction::Go("a"));
    assert_eq!(p.current(), Some("a"));
}

#[test]
fn prev_on_first_track_restarts_unless_repeat_all() {
    assert_eq!(q(0, Repeat::Off).prev(0), PrevAction::Restart);
    assert_eq!(q(0, Repeat::One).prev(0), PrevAction::Restart);
    let mut p = q(0, Repeat::All);
    assert_eq!(p.prev(0), PrevAction::Go("c"));
    assert_eq!(p.current(), Some("c"));
}

#[test]
fn remove_track_before_current_keeps_current() {
    let mut p = q(1, Repeat::Off);
    p.remove("a");
    assert_eq!(p.current(), Some("b"));
    assert_eq!(p.prev(0), PrevAction::Restart, "b 现在是第一首");
    assert_eq!(p.next(false), Some("c"));
}

#[test]
fn remove_track_after_current_skips_it() {
    let mut p = q(0, Repeat::Off);
    p.remove("b");
    assert_eq!(p.current(), Some("a"));
    assert_eq!(p.next(false), Some("c"));
    assert_eq!(p.next(false), None);
}

#[test]
fn remove_current_moves_to_the_following_track() {
    let mut p = q(1, Repeat::Off);
    p.remove("b");
    assert_eq!(p.current(), Some("c"));
}

#[test]
fn remove_current_last_track_ends_queue_unless_repeat_all() {
    let mut p = q(2, Repeat::Off);
    p.remove("c");
    assert_eq!(p.current(), None);
    let mut p = q(2, Repeat::All);
    p.remove("c");
    assert_eq!(p.current(), Some("a"));
    let mut p = PlayQueue::new(vec!["a".into()], 0, false, Repeat::All);
    p.remove("a");
    assert_eq!(p.current(), None);
}

#[test]
fn repeat_parses_only_known_modes() {
    assert_eq!("off".parse::<Repeat>(), Ok(Repeat::Off));
    assert_eq!("all".parse::<Repeat>(), Ok(Repeat::All));
    assert_eq!("one".parse::<Repeat>(), Ok(Repeat::One));
    assert!("loop".parse::<Repeat>().is_err());
}

// ---------- Player：偏好与状态 ----------

fn noop() -> Box<dyn Fn(PlayerState) + Send + Sync> {
    Box::new(|_| {})
}

#[test]
fn defaults_without_prefs_file() {
    let t = Tmp::new("defaults");
    let s = Player::new(t.0.clone(), noop()).state();
    assert_eq!((s.volume, s.shuffle, s.repeat), (0.8, false, Repeat::Off));
    assert_eq!((s.track_id, s.playing, s.position_ms), (None, false, 0));
}

#[test]
fn prefs_survive_a_new_player_on_the_same_data_dir() {
    let t = Tmp::new("prefs");
    let p = Player::new(t.0.clone(), noop());
    p.set_volume(0.3);
    p.set_shuffle(true);
    p.set_repeat(Repeat::One);
    drop(p);
    assert!(t.0.join("player.json").is_file());
    let s = Player::new(t.0.clone(), noop()).state();
    assert_eq!((s.volume, s.shuffle, s.repeat), (0.3, true, Repeat::One));
}

#[test]
fn volume_is_clamped_to_unit_range() {
    let t = Tmp::new("clamp");
    let p = Player::new(t.0.clone(), noop());
    p.set_volume(1.7);
    assert_eq!(p.state().volume, 1.0);
    p.set_volume(-1.0);
    assert_eq!(p.state().volume, 0.0);
}

#[test]
fn changes_are_pushed_to_listener() {
    let t = Tmp::new("notify");
    let got = Arc::new(Mutex::new(vec![]));
    let g = got.clone();
    let p = Player::new(t.0.clone(), Box::new(move |s| g.lock().unwrap().push(s)));
    p.set_repeat(Repeat::All);
    assert_eq!(got.lock().unwrap().last().unwrap().repeat, Repeat::All);
}

#[test]
fn state_serializes_to_the_ipc_contract() {
    let t = Tmp::new("json");
    let text = serde_json::to_string(&Player::new(t.0.clone(), noop()).state()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        v,
        serde_json::json!({
            "playing": false, "positionMs": 0, "durationMs": 0,
            "volume": 0.8, "shuffle": false, "repeat": "off"
        })
    );
}
