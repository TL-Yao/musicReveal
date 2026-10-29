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
fn renamed_track_keeps_its_place_in_plain_and_shuffled_queues() {
    for shuffle in [false, true] {
        let mut p = PlayQueue::new(abc(), 1, shuffle, Repeat::Off);
        p.rename("b", "b2");
        p.rename("c", "c2");
        assert_eq!(p.current(), Some("b2"));
        let mut rest: Vec<String> =
            std::iter::from_fn(|| p.next(false).map(String::from)).collect();
        if !shuffle {
            assert_eq!(rest, ["c2"]);
        } else {
            rest.sort();
            assert_eq!(rest, ["a", "c2"]);
        }
    }
}

#[test]
fn shuffle_keeps_current_first_and_plays_every_other_track_once() {
    let mut p = PlayQueue::new(ids(20), 5, true, Repeat::Off);
    assert_eq!(p.current(), Some("5"));
    let mut rest: Vec<String> = (0..19).map(|_| p.next(false).unwrap().to_owned()).collect();
    assert_eq!(p.next(false), None, "随机播放一轮后到队尾");
    let want: Vec<String> = ids(20).into_iter().filter(|s| s != "5").collect();
    assert_ne!(rest, want, "其余曲目被打乱（19! 分之一的概率恰好原序）");
    rest.sort_by_key(|s| s.parse::<u32>().unwrap());
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
    let want = if cur == 19 {
        None
    } else {
        Some((cur + 1).to_string())
    };
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
fn order_and_pos_expose_the_play_order() {
    let p = q(1, Repeat::Off);
    assert_eq!(p.order(), abc().as_slice());
    assert_eq!(p.pos(), 1);
}

#[test]
fn shuffled_order_is_the_shuffled_play_order_with_every_track() {
    let mut p = PlayQueue::new(ids(20), 5, true, Repeat::Off);
    let order = p.order().to_vec();
    assert_eq!((order[0].as_str(), p.pos()), ("5", 0));
    let played: Vec<String> = std::iter::once("5".to_owned())
        .chain(std::iter::from_fn(|| p.next(false).map(String::from)))
        .collect();
    assert_eq!(played, order, "order() 就是实际播放顺序");
    assert_ne!(order, ids(20), "被打乱（20! 分之一的概率恰好原序）");
    let mut sorted = order;
    sorted.sort_by_key(|s| s.parse::<u32>().unwrap());
    assert_eq!(sorted, ids(20));
}

#[test]
fn jump_moves_to_index_without_reordering() {
    let mut p = PlayQueue::new(ids(10), 0, true, Repeat::Off);
    let order = p.order().to_vec();
    assert_eq!(p.jump(4), Some(order[4].as_str()));
    assert_eq!((p.pos(), p.order()), (4, order.as_slice()));
    assert_eq!(p.next(false), Some(order[5].as_str()));
}

#[test]
fn jump_out_of_range_changes_nothing() {
    let mut p = q(1, Repeat::Off);
    assert_eq!(p.jump(3), None);
    assert_eq!((p.current(), p.pos()), (Some("b"), 1));
}

#[test]
fn remove_current_then_current_is_the_next_in_order() {
    let mut p = PlayQueue::new(ids(10), 0, true, Repeat::Off);
    let order = p.order().to_vec();
    p.jump(3);
    p.remove(&order[3]);
    assert_eq!(p.current(), Some(order[4].as_str()));
    assert_eq!(p.pos(), 3);
    assert_eq!(p.order().len(), 9);
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
            "volume": 0.8, "shuffle": false, "repeat": "off", "queueRev": 0
        })
    );
}

#[test]
fn empty_player_has_an_empty_queue() {
    let t = Tmp::new("queue");
    let p = Player::new(t.0.clone(), noop());
    let v = serde_json::to_value(p.queue()).unwrap();
    assert_eq!(v, serde_json::json!({ "ids": [], "pos": 0 }));
    p.jump(0);
    p.remove("x");
    assert_eq!(p.state().track_id, None);
}

// ---------- 真实解码（需要音频设备；音量 0，音频本身也是静音） ----------

/// 同一时刻只开一个播放器，计时断言不受彼此拖累。
static AUDIO: Mutex<()> = Mutex::new(());

fn audio() -> std::sync::MutexGuard<'static, ()> {
    AUDIO.lock().unwrap_or_else(|e| e.into_inner())
}

/// 2 秒静音 44.1kHz 双声道 16 位 wav。
fn wav(path: &Path) {
    let data_len: u32 = 44_100 * 2 * 2 * 2;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&2u16.to_le_bytes()); // 声道
    b.extend_from_slice(&44_100u32.to_le_bytes());
    b.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
    b.extend_from_slice(&4u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    b.resize(b.len() + data_len as usize, 0);
    std::fs::write(path, b).unwrap();
}

/// 生成 2 秒静音文件；非 wav 用 ffmpeg，没有 ffmpeg 返回 None。
fn make(dir: &Path, name: &str) -> Option<String> {
    let p = dir.join(name);
    let ext = p.extension().unwrap().to_str().unwrap();
    if ext == "wav" {
        wav(&p);
        return Some(p.to_string_lossy().into_owned());
    }
    let codec: &[&str] = match ext {
        "ogg" => &["-c:a", "vorbis", "-strict", "-2"],
        "m4a" => &["-c:a", "aac"],
        _ => &[],
    };
    encode(dir, name, codec, 2.0)
}

/// 用 ffmpeg 按 `codec` 生成 `secs` 秒静音文件；没有 ffmpeg 返回 None。
fn encode(dir: &Path, name: &str, codec: &[&str], secs: f32) -> Option<String> {
    let p = dir.join(name);
    let ok = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("anullsrc=r=44100:cl=stereo")
        .args(["-t", &secs.to_string()])
        .args(codec)
        .arg(&p)
        .status()
        .ok()?
        .success();
    assert!(ok, "ffmpeg 生成 {name} 失败");
    Some(p.to_string_lossy().into_owned())
}

const OPUS: &[&str] = &["-c:a", "libopus", "-ar", "48000"];
const ALAC: &[&str] = &["-c:a", "alac"];

/// 资料库收的每种编码都能得到可播音源（rodio 解不了的经 ffmpeg 兜底），时长对得上。不需要音频设备。
#[test]
fn every_library_codec_decodes_to_a_playable_source() {
    let t = Tmp::new("codecs");
    let cache = t.0.join("cache");
    let cases: [(&str, &[&str]); 7] = [
        ("a.wav", &[]),
        ("a.flac", &[]),
        ("a.mp3", &[]),
        ("aac.m4a", &["-c:a", "aac"]),
        ("alac.m4a", ALAC),
        ("vorbis.ogg", &["-c:a", "vorbis", "-strict", "-2"]),
        ("opus.ogg", OPUS),
    ];
    let mut bad = vec![];
    for (name, codec) in cases {
        let Some(p) = encode(&t.0, name, codec, 0.5) else {
            eprintln!("没有 ffmpeg，跳过");
            return;
        };
        match open_source(Path::new(&p), &cache) {
            Ok(src) => {
                let per_sec = src.sample_rate().get() as f64 * src.channels().get() as f64;
                let secs = src.count() as f64 / per_sec;
                if !(0.4..0.65).contains(&secs) {
                    bad.push(format!("{name}：时长 {secs:.3}s"));
                }
            }
            Err(e) => bad.push(format!("{name}：{e}")),
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

fn quiet(t: &Tmp, on_change: Box<dyn Fn(PlayerState) + Send + Sync>) -> Player {
    let p = Player::new(t.0.join("data"), on_change);
    p.set_volume(0.0);
    assert_eq!(p.state().volume, 0.0);
    p
}

fn wait(p: &Player, secs: u64, f: impl Fn(&PlayerState) -> bool) -> PlayerState {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        let s = p.state();
        if f(&s) {
            return s;
        }
        assert!(Instant::now() < end, "等待超时：{s:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn plays_seeks_and_advances(ext: &str) {
    let make = |dir: &Path, name: &str| make(dir, name);
    plays_seeks_and_advances_with(ext, &make);
}

fn plays_seeks_and_advances_with(ext: &str, make: &dyn Fn(&Path, &str) -> Option<String>) {
    let _g = audio();
    let t = Tmp::new(ext);
    let (Some(a), Some(b)) = (
        make(&t.0, &format!("a.{ext}")),
        make(&t.0, &format!("b.{ext}")),
    ) else {
        eprintln!("没有 ffmpeg，跳过 {ext}");
        return;
    };
    let pushes = Arc::new(AtomicUsize::new(0));
    let n = pushes.clone();
    let p = quiet(
        &t,
        Box::new(move |_| {
            n.fetch_add(1, Ordering::Relaxed);
        }),
    );
    p.play(vec![a.clone(), b.clone()], 0).unwrap();
    let before = pushes.load(Ordering::Relaxed);
    // ffmpeg 兜底解码时开头会晚一点
    let s = wait(&p, 3, |s| s.position_ms >= 1000);
    assert!(
        s.playing && s.track_id.as_deref() == Some(a.as_str()),
        "{s:?}"
    );
    assert!(s.position_ms >= 1000, "{s:?}");
    assert!((1900..=2200).contains(&s.duration_ms), "{s:?}");
    assert!(
        pushes.load(Ordering::Relaxed) > before,
        "播放中每秒推送一次"
    );

    p.seek(1500);
    let s = p.state();
    assert!(s.position_ms >= 1500, "{s:?}");

    let s = wait(&p, 3, |s| s.track_id.as_deref() == Some(b.as_str()));
    assert!(s.playing, "{s:?}");
    let s = wait(&p, 4, |s| !s.playing);
    assert_eq!(
        s.track_id.as_deref(),
        Some(b.as_str()),
        "循环关：停在最后一首"
    );
    assert_eq!((s.position_ms, s.error), (0, None));
}

#[test]
#[ignore = "需要音频设备"]
fn wav_plays_seeks_and_advances() {
    plays_seeks_and_advances("wav");
}

#[test]
#[ignore = "需要音频设备"]
fn flac_plays_seeks_and_advances() {
    plays_seeks_and_advances("flac");
}

#[test]
#[ignore = "需要音频设备"]
fn mp3_plays_seeks_and_advances() {
    plays_seeks_and_advances("mp3");
}

#[test]
#[ignore = "需要音频设备"]
fn ogg_plays_seeks_and_advances() {
    plays_seeks_and_advances("ogg");
}

#[test]
#[ignore = "需要音频设备"]
fn m4a_plays_seeks_and_advances() {
    plays_seeks_and_advances("m4a");
}

#[test]
#[ignore = "需要音频设备"]
fn alac_m4a_plays_seeks_and_advances() {
    plays_seeks_and_advances_with("m4a", &|d, n| encode(d, n, ALAC, 2.0));
}

#[test]
#[ignore = "需要音频设备"]
fn opus_ogg_plays_seeks_and_advances() {
    plays_seeks_and_advances_with("ogg", &|d, n| encode(d, n, OPUS, 2.0));
}

/// 让兜底解码慢下来：`REVEAL_FFMPEG` 指向以 10 倍实时速度读入的包装，丢掉时恢复。
/// 改的是进程环境变量，调用方须持有 `audio()`。
struct SlowFfmpeg;

impl SlowFfmpeg {
    fn new(t: &Tmp) -> Option<Self> {
        use std::os::unix::fs::PermissionsExt;
        let real = reveal_core::find_ffmpeg()?;
        let sh = t.0.join("slow-ffmpeg");
        std::fs::write(
            &sh,
            format!("#!/bin/sh\nexec '{}' -readrate 10 \"$@\"\n", real.display()),
        )
        .unwrap();
        std::fs::set_permissions(&sh, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("REVEAL_FFMPEG", &sh);
        Some(SlowFfmpeg)
    }
}

impl Drop for SlowFfmpeg {
    fn drop(&mut self) {
        std::env::remove_var("REVEAL_FFMPEG");
    }
}

/// 命令行里带 `dir` 的 ffmpeg 进程 pid（输入文件在 `dir` 里）。
fn ffmpeg_pids(dir: &Path) -> Vec<String> {
    let o = std::process::Command::new("pgrep")
        .arg("-f")
        .arg(dir)
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// 兜底解码缓存目录里的文件（含半成品）。
fn decoded_files(t: &Tmp) -> Vec<String> {
    std::fs::read_dir(t.0.join("data/decoded"))
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
#[ignore = "需要音频设备"]
fn stop_during_fallback_decode_kills_ffmpeg_and_cleans_up() {
    let _g = audio();
    let t = Tmp::new("decodestop");
    let Some(_slow) = SlowFfmpeg::new(&t) else {
        return;
    };
    let o = encode(&t.0, "o.ogg", OPUS, 30.0).unwrap();
    let p = quiet(&t, noop());
    p.play(vec![o], 0).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(!ffmpeg_pids(&t.0).is_empty(), "此时应在解码");
    p.stop();
    assert_eq!(
        ffmpeg_pids(&t.0),
        Vec::<String>::new(),
        "stop 返回时 ffmpeg 已结束"
    );
    assert_eq!(decoded_files(&t), Vec::<String>::new(), "不留半成品");
}

#[test]
#[ignore = "需要音频设备"]
fn seek_while_fallback_decoding_applies_once_ready() {
    let _g = audio();
    let t = Tmp::new("decodeseek");
    let Some(_slow) = SlowFfmpeg::new(&t) else {
        return;
    };
    let o = encode(&t.0, "o.ogg", OPUS, 30.0).unwrap();
    let p = quiet(&t, noop());
    p.play(vec![o.clone()], 0).unwrap();
    p.seek(15_000);
    assert_eq!(p.state().position_ms, 15_000, "等待期间显示待定位的位置");
    let s = wait(&p, 20, |s| s.position_ms > 15_200);
    assert!(
        s.playing && s.track_id.as_deref() == Some(o.as_str()) && s.position_ms < 20_000,
        "{s:?}"
    );
    p.stop();
}

#[test]
#[ignore = "需要音频设备"]
fn rapid_switches_between_fallback_tracks_leave_one_cache_file() {
    let _g = audio();
    let t = Tmp::new("decoderapid");
    let Some(_slow) = SlowFfmpeg::new(&t) else {
        return;
    };
    let ids: Vec<String> = ["a.ogg", "b.ogg", "c.ogg"]
        .iter()
        .map(|n| encode(&t.0, n, OPUS, 30.0).unwrap())
        .collect();
    let p = quiet(&t, noop());
    p.play(ids.clone(), 0).unwrap();
    p.next();
    p.next();
    let s = wait(&p, 20, |s| s.position_ms > 300);
    assert_eq!(s.track_id.as_deref(), Some(ids[2].as_str()), "{s:?}");
    assert_eq!(decoded_files(&t).len(), 1, "{:?}", decoded_files(&t));
    assert_eq!(ffmpeg_pids(&t.0), Vec::<String>::new());
    p.stop();
}

/// 兜底解码还没完就切走：解码结果作废，不把播放拉回那首。
#[test]
#[ignore = "需要音频设备"]
fn switching_away_during_fallback_decode_discards_it() {
    let _g = audio();
    let t = Tmp::new("decodeskip");
    let Some(o) = encode(&t.0, "o.ogg", OPUS, 30.0) else {
        return;
    };
    let w = make(&t.0, "w.wav").unwrap();
    let p = quiet(&t, noop());
    p.play(vec![o.clone(), w.clone()], 0).unwrap();
    let s = p.state();
    assert_eq!(s.track_id.as_deref(), Some(o.as_str()), "{s:?}");
    assert!(
        (29_000..=31_000).contains(&s.duration_ms),
        "时长按原文件：{s:?}"
    );
    p.next();
    std::thread::sleep(Duration::from_millis(1500));
    let s = p.state();
    assert!(
        s.playing && s.track_id.as_deref() == Some(w.as_str()),
        "{s:?}"
    );
    // 放的确实是 2 秒的 w：到点就停（作废的 30 秒解码若被接上会一直放）
    let s = wait(&p, 3, |s| !s.playing);
    assert_eq!((s.track_id.as_deref(), s.error), (Some(w.as_str()), None));
}

#[test]
#[ignore = "需要音频设备"]
fn repeat_one_replays_the_track_when_it_ends() {
    let _g = audio();
    let t = Tmp::new("one");
    let a = make(&t.0, "a.wav").unwrap();
    let b = make(&t.0, "b.wav").unwrap();
    let p = quiet(&t, noop());
    p.set_repeat(Repeat::One);
    p.play(vec![a.clone(), b], 0).unwrap();
    p.seek(1800);
    let s = wait(&p, 3, |s| s.position_ms < 1000);
    assert!(
        s.playing && s.track_id.as_deref() == Some(a.as_str()),
        "{s:?}"
    );
}

#[test]
#[ignore = "需要音频设备"]
fn repeat_all_wraps_to_first_track_when_queue_ends() {
    let _g = audio();
    let t = Tmp::new("all");
    let a = make(&t.0, "a.wav").unwrap();
    let b = make(&t.0, "b.wav").unwrap();
    let p = quiet(&t, noop());
    p.set_repeat(Repeat::All);
    p.play(vec![a.clone(), b], 1).unwrap();
    p.seek(1800);
    let s = wait(&p, 3, |s| s.track_id.as_deref() == Some(a.as_str()));
    assert!(s.playing, "{s:?}");
}

#[test]
#[ignore = "需要音频设备"]
fn toggle_next_and_prev_drive_playback() {
    let _g = audio();
    let t = Tmp::new("ctl");
    let a = make(&t.0, "a.wav").unwrap();
    let b = make(&t.0, "b.wav").unwrap();
    let p = quiet(&t, noop());
    p.play(vec![a.clone(), b.clone()], 0).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    p.toggle();
    assert!(!p.state().playing);
    // 暂停在音频线程上 5ms 内生效
    std::thread::sleep(Duration::from_millis(50));
    let paused = p.state();
    assert!(!paused.playing && paused.position_ms > 0, "{paused:?}");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(p.state().position_ms, paused.position_ms, "暂停时位置不动");
    p.toggle();
    wait(&p, 2, |s| s.playing && s.position_ms > paused.position_ms);

    p.prev();
    let s = p.state();
    assert!(
        s.track_id.as_deref() == Some(a.as_str()) && s.position_ms < 300,
        "第一首上一首 = 从头：{s:?}"
    );
    p.next();
    assert_eq!(p.state().track_id.as_deref(), Some(b.as_str()));
    p.prev();
    assert_eq!(p.state().track_id.as_deref(), Some(a.as_str()));
    p.next();
    p.next();
    let s = p.state();
    assert!(
        !s.playing && s.position_ms == 0 && s.track_id.as_deref() == Some(b.as_str()),
        "{s:?}"
    );
    p.toggle();
    let s = p.state();
    assert!(
        s.playing && s.track_id.as_deref() == Some(b.as_str()),
        "停止后再按播放从当前曲开始：{s:?}"
    );
}

/// 曲终消息已发出、还没被处理时拖动进度：留在本曲的新位置继续播，不跳到下一首。
#[test]
#[ignore = "需要音频设备"]
fn seek_after_track_ended_but_before_advance_stays_on_track() {
    let _g = audio();
    let t = Tmp::new("seekend");
    let a = make(&t.0, "a.wav").unwrap();
    let b = make(&t.0, "b.wav").unwrap();
    let p = quiet(&t, noop());
    p.play(vec![a.clone(), b], 0).unwrap();
    p.seek(1900);
    let hold = p.hold_events();
    // 剩 100ms：等足 600ms，曲终消息必已排队
    std::thread::sleep(Duration::from_millis(600));
    p.seek(500);
    drop(hold);
    std::thread::sleep(Duration::from_millis(300));
    let s = p.state();
    assert!(
        s.playing && s.track_id.as_deref() == Some(a.as_str()),
        "{s:?}"
    );
    assert!((500..1500).contains(&s.position_ms), "{s:?}");
}

fn bad(dir: &Path) -> String {
    let p = dir.join("bad.mp3");
    std::fs::write(&p, "not audio\n".repeat(500)).unwrap();
    p.to_string_lossy().into_owned()
}

#[test]
#[ignore = "需要音频设备"]
fn missing_or_corrupt_tracks_are_skipped_with_a_reason() {
    let _g = audio();
    let t = Tmp::new("skip");
    let good = make(&t.0, "good.wav").unwrap();
    let gone = t.0.join("gone.wav").to_string_lossy().into_owned();
    let bad = bad(&t.0);
    let p = quiet(&t, noop());

    p.play(vec![gone.clone(), good.clone()], 0).unwrap();
    let s = p.state();
    assert!(
        s.playing && s.track_id.as_deref() == Some(good.as_str()),
        "{s:?}"
    );
    assert!(s.error.as_deref().unwrap().contains("不存在"), "{s:?}");

    // 损坏文件先交给 ffmpeg 兜底，它也解不了才跳过
    p.play(vec![bad.clone(), good.clone()], 0).unwrap();
    let s = wait(&p, 5, |s| s.track_id.as_deref() != Some(bad.as_str()));
    assert!(
        s.playing && s.track_id.as_deref() == Some(good.as_str()),
        "{s:?}"
    );
    assert!(s.error.as_deref().unwrap().contains("损坏"), "{s:?}");

    // 曲终自动前进到失效的曲目
    p.play(vec![good.clone(), gone], 0).unwrap();
    assert_eq!(p.state().error, None, "新的播放清掉旧错误");
    p.seek(1800);
    let s = wait(&p, 3, |s| !s.playing);
    assert!(s.error.as_deref().unwrap().contains("不存在"), "{s:?}");
}

#[test]
#[ignore = "需要音频设备"]
fn all_tracks_failing_stops_playback() {
    let _g = audio();
    let t = Tmp::new("allfail");
    let gone = t.0.join("gone.flac").to_string_lossy().into_owned();
    let p = quiet(&t, noop());
    p.play(vec![gone, bad(&t.0)], 0).unwrap();
    let s = wait(&p, 5, |s| s.track_id.is_none());
    assert!(
        !s.playing && s.track_id.is_none() && s.error.is_some(),
        "{s:?}"
    );
}

#[test]
fn play_rejects_empty_queue_or_bad_index() {
    let t = Tmp::new("badplay");
    let p = Player::new(t.0.clone(), noop());
    assert!(p.play(vec![], 0).is_err());
    assert!(p.play(vec!["x".into()], 1).is_err());
}

/// 正在播放的 FLAC 被转成 ALAC：播放不断，当前曲与队列指向新路径，上一首 / 下一首照常。
#[test]
#[ignore = "需要音频设备"]
fn track_recoded_while_playing_keeps_playing_and_the_queue_follows() {
    let _g = audio();
    let t = Tmp::new("recode");
    let (Some(a), Some(b)) = (
        encode(&t.0, "a.flac", &[], 4.0),
        encode(&t.0, "b.flac", &[], 4.0),
    ) else {
        eprintln!("没有 ffmpeg，跳过");
        return;
    };
    let p = quiet(&t, Box::new(|_| {}));
    p.play(vec![a.clone(), b.clone()], 0).unwrap();
    wait(&p, 3, |s| s.playing && s.position_ms >= 300);
    let new = reveal_core::recode_lossless(
        Path::new(&a),
        reveal_core::Lossless::Alac,
        &Default::default(),
    )
    .unwrap();
    let new = new.to_string_lossy().into_owned();
    assert!(!Path::new(&a).exists());
    p.rename(&a, &new);
    let s = p.state();
    assert_eq!(s.track_id.as_deref(), Some(new.as_str()));
    assert!(s.playing && s.error.is_none(), "{s:?}");
    let before = s.position_ms;
    let s = wait(&p, 3, |s| s.position_ms >= before + 500);
    assert!(s.playing && s.error.is_none(), "播放没被打断：{s:?}");

    p.next();
    let s = wait(&p, 3, |s| s.track_id.as_deref() == Some(b.as_str()));
    assert!(s.error.is_none(), "{s:?}");
    p.prev();
    let s = wait(&p, 3, |s| {
        s.track_id.as_deref() == Some(new.as_str()) && s.playing
    });
    assert!(s.error.is_none(), "上一首是新路径：{s:?}");
    p.stop();
}
