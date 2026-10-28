//! 资料库行为测试：ffmpeg 合成的样本放进临时输出目录；不自动轮询，`refresh()` / `set_dir()` 驱动，
//! `library` 事件经通道收取。

use musicreveal_app::library::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "mr-library-{tag}-{}-{}",
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

fn ffmpeg(args: &[&str]) {
    let ff = reveal_core::find_ffmpeg().expect("需要 ffmpeg（可设 REVEAL_FFMPEG）");
    let st = Command::new(ff)
        .args(["-v", "error", "-y"])
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "ffmpeg {args:?}");
}

/// 1 秒正弦波 mp3；`meta` 是 `-metadata` 键值；`cover` 是嵌入封面的宽高。
fn mp3(path: &Path, meta: &[(&str, &str)], cover: Option<(u32, u32)>) -> PathBuf {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let out = path.to_string_lossy().into_owned();
    let png = format!("{out}.png");
    let mut a: Vec<String> = ["-f", "lavfi", "-i", "sine=f=440:d=1"]
        .map(String::from)
        .to_vec();
    if let Some((w, h)) = cover {
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &format!("color=red:s={w}x{h}"),
            "-frames:v",
            "1",
            &png,
        ]);
        a.extend(
            [
                "-i",
                &png,
                "-map",
                "0:a",
                "-map",
                "1:v",
                "-c:v",
                "mjpeg",
                "-disposition:v",
                "attached_pic",
            ]
            .map(String::from),
        );
    }
    a.extend(["-c:a", "libmp3lame", "-id3v2_version", "3"].map(String::from));
    for (k, v) in meta {
        a.push("-metadata".into());
        a.push(format!("{k}={v}"));
    }
    a.push(out);
    ffmpeg(&a.iter().map(String::as_str).collect::<Vec<_>>());
    let _ = std::fs::remove_file(png);
    path.to_path_buf()
}

struct Rig {
    t: Tmp,
    out: PathBuf,
    lib: Library,
    rx: Receiver<LibraryView>,
}

fn open(data: &Path) -> (Library, Receiver<LibraryView>) {
    let (tx, rx) = channel();
    let lib = Library::new(
        data.to_path_buf(),
        Box::new(move |v| {
            let _ = tx.send(v);
        }),
    )
    .with_poll_interval(None);
    (lib, rx)
}

impl Rig {
    fn new(tag: &str) -> Self {
        let t = Tmp::new(tag);
        let out = t.0.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let (lib, rx) = open(&t.0.join("data"));
        Rig { t, out, lib, rx }
    }
    fn data(&self) -> PathBuf {
        self.t.0.join("data")
    }
}

/// 收事件直到一份 `scanning: false` 的；中间的部分结果一并返回。
fn settled(rx: &Receiver<LibraryView>) -> (Vec<LibraryView>, LibraryView) {
    let mut seen = vec![];
    loop {
        let v = rx
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| panic!("20s 内没等到扫描完成，已收到 {seen:#?}"));
        if !v.scanning {
            return (seen, v);
        }
        seen.push(v);
    }
}

fn ids(v: &LibraryView) -> Vec<String> {
    let mut ids: Vec<String> = v.tracks.iter().map(|t| t.id.clone()).collect();
    ids.sort();
    ids
}

fn id(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[test]
fn scan_collects_playable_files_recursively_and_skips_the_rest() {
    let r = Rig::new("scan");
    let o = &r.out;
    let a = mp3(
        &o.join("a.mp3"),
        &[
            ("title", "标题甲"),
            ("artist", "X/Y"),
            ("album", "专辑"),
            ("album_artist", "群星"),
            ("date", "2020"),
            ("track", "3"),
        ],
        None,
    );
    let b = mp3(&o.join("sub/歌手 - 歌名乙.mp3"), &[], None);
    let c = mp3(&o.join("sub/1/2/c.mp3"), &[("title", "丙")], None);
    mp3(&o.join("sub/1/2/3/太深.mp3"), &[], None);
    mp3(&o.join(".隐藏.mp3"), &[], None);
    mp3(&o.join(".hid/x.mp3"), &[], None);
    let part = format!("{}1a2b.part", reveal_core::PART_PREFIX);
    std::fs::copy(&a, o.join(&part)).unwrap();
    std::fs::copy(&a, o.join("a.mp3.txt")).unwrap();
    std::fs::write(o.join("坏的.mp3"), b"not audio at all").unwrap();
    std::fs::write(o.join("notes.txt"), b"x").unwrap();

    assert!(r.lib.view().tracks.is_empty());
    r.lib.set_dir(o);
    let (_, v) = settled(&r.rx);
    assert_eq!(v.dir, id(o));
    assert_eq!(ids(&v), [id(&a), id(&c), id(&b)].map(|s| s.to_string()));

    let ta = r.lib.track(&id(&a)).unwrap();
    assert_eq!(ta.title, "标题甲");
    assert_eq!(ta.artists, ["X", "Y"]);
    assert_eq!(ta.album.as_deref(), Some("专辑"));
    assert_eq!(ta.album_artist.as_deref(), Some("群星"));
    assert_eq!((ta.year, ta.track, ta.disc), (Some(2020), Some(3), None));
    assert!(ta.duration_ms.abs_diff(1000) <= 60, "{}", ta.duration_ms);
    assert!(ta.quality.starts_with("MP3"), "{}", ta.quality);
    assert_eq!(ta.cover_url, None);
    assert!(!ta.has_lyrics);
    assert!(ta.added_at > 1_600_000_000_000, "{}", ta.added_at);
    assert_eq!(r.lib.track(&id(&b)).unwrap().title, "歌名乙");
    assert_eq!(r.lib.view(), v);

    let j = serde_json::to_value(&v).unwrap();
    assert_eq!(j["scanning"], false);
    let t = j["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id(&a).as_str())
        .unwrap();
    for k in [
        "durationMs",
        "addedAt",
        "hasLyrics",
        "albumArtist",
        "quality",
    ] {
        assert!(t.get(k).is_some(), "缺 {k}：{t}");
    }
    assert!(
        t.get("coverUrl").is_none() && t.get("disc").is_none(),
        "{t}"
    );
}

#[test]
fn changes_show_up_and_unchanged_rescans_stay_silent() {
    let r = Rig::new("changes");
    let a = mp3(&r.out.join("a.mp3"), &[("title", "一")], None);
    r.lib.set_dir(&r.out);
    let (_, v) = settled(&r.rx);
    assert_eq!(ids(&v), [id(&a)]);

    // 没变化的重扫不发；若发了，下面先收到的会是只有 a 的那份
    r.lib.refresh();
    let b = mp3(&r.out.join("b.mp3"), &[], None);
    r.lib.refresh();
    let (_, v) = settled(&r.rx);
    assert_eq!(ids(&v), [id(&a), id(&b)], "新增");

    let b2 = r.out.join("b2.mp3");
    std::fs::rename(&b, &b2).unwrap();
    r.lib.refresh();
    let (_, v) = settled(&r.rx);
    assert_eq!(ids(&v), [id(&a), id(&b2)], "改名");

    mp3(&a, &[("title", "二")], None);
    r.lib.refresh();
    let (_, v) = settled(&r.rx);
    assert_eq!(v.tracks[0].title, "二", "改标签");

    std::fs::remove_file(&b2).unwrap();
    r.lib.refresh();
    let (_, v) = settled(&r.rx);
    assert_eq!(ids(&v), [id(&a)], "删除");

    let other = r.t.0.join("other");
    let c = mp3(&other.join("c.mp3"), &[], None);
    r.lib.set_dir(&other);
    assert!(r.lib.view().tracks.is_empty() && r.lib.view().scanning);
    let (_, v) = settled(&r.rx);
    assert_eq!(
        (v.dir.clone(), ids(&v)),
        (id(&other), vec![id(&c)]),
        "换目录"
    );
}

/// 默认轮询：不叫醒也能在 3 秒内看到变化。
#[test]
fn polling_picks_up_new_files_within_three_seconds() {
    let t = Tmp::new("poll");
    let (tx, rx) = channel();
    let lib = Library::new(
        t.0.join("data"),
        Box::new(move |v| {
            let _ = tx.send(v);
        }),
    );
    lib.set_dir(&t.0);
    let (_, v) = settled(&rx);
    assert!(v.tracks.is_empty());
    let a = mp3(&t.0.join("a.mp3"), &[], None);
    let t0 = std::time::Instant::now();
    let (_, v) = settled(&rx);
    assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
    assert_eq!(ids(&v), [id(&a)]);
}

/// 把内容换成同样大小的空字节、mtime 拨回原样：再读标签就会失败，只有缓存还记得原来的。
fn spoil_keeping_stat(p: &Path) {
    let m = std::fs::metadata(p).unwrap();
    std::fs::write(p, vec![0u8; m.len() as usize]).unwrap();
    std::fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(m.modified().unwrap())
        .unwrap();
}

#[test]
fn tags_are_cached_across_restarts_and_dropped_with_the_file() {
    let r = Rig::new("cache");
    let a = mp3(&r.out.join("a.mp3"), &[("title", "缓存")], None);
    let b = mp3(&r.out.join("b.mp3"), &[("title", "乙")], None);
    r.lib.set_dir(&r.out);
    settled(&r.rx);
    spoil_keeping_stat(&a);

    let (lib, rx) = open(&r.data());
    lib.set_dir(&r.out);
    let (partial, v) = settled(&rx);
    assert!(
        partial.is_empty(),
        "全部命中缓存，不必进入扫描中：{partial:?}"
    );
    assert_eq!(ids(&v), [id(&a), id(&b)]);
    assert_eq!(lib.track(&id(&a)).unwrap().title, "缓存");

    std::fs::remove_file(&a).unwrap();
    lib.refresh();
    let (_, v) = settled(&rx);
    assert_eq!(ids(&v), [id(&b)]);
    let json = std::fs::read_to_string(r.data().join("library.json")).unwrap();
    assert!(!json.contains("a.mp3") && json.contains("b.mp3"), "{json}");
}

#[test]
fn first_scan_reports_scanning_then_the_full_list() {
    let r = Rig::new("partial");
    let a = mp3(&r.out.join("000.mp3"), &[], None);
    for i in 1..40 {
        std::fs::copy(&a, r.out.join(format!("{i:03}.mp3"))).unwrap();
    }
    r.lib.set_dir(&r.out);
    assert!(r.lib.view().scanning, "get_library 立即返回当前已知");
    let (partial, v) = settled(&r.rx);
    assert!(!partial.is_empty() && partial.iter().all(|p| p.scanning));
    assert_eq!(v.tracks.len(), 40);
}

fn dims(jpg: &Path) -> (u32, u32) {
    let o = Command::new("sips")
        .args(["-g", "pixelWidth", "-g", "pixelHeight"])
        .arg(jpg)
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&o.stdout);
    let n = |k: &str| {
        s.lines()
            .find_map(|l| l.trim().strip_prefix(k))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    };
    (n("pixelWidth:"), n("pixelHeight:"))
}

#[test]
fn embedded_covers_become_jpeg_thumbnails_of_at_least_600px() {
    let r = Rig::new("cover");
    let big = mp3(&r.out.join("big.mp3"), &[], Some((1000, 800)));
    let small = mp3(&r.out.join("small.mp3"), &[], Some((300, 200)));
    let none = mp3(&r.out.join("none.mp3"), &[], None);
    r.lib.set_dir(&r.out);
    settled(&r.rx);

    let tb = r.lib.track(&id(&big)).unwrap();
    let jpg = tb.cover.clone().expect("有封面");
    assert!(jpg.starts_with(r.data()), "{jpg:?}");
    assert_eq!(&std::fs::read(&jpg).unwrap()[..2], [0xFF, 0xD8], "JPEG");
    assert_eq!(dims(&jpg), (600, 480));
    let url = tb.cover_url.clone().unwrap();
    assert!(url.starts_with("asset://localhost/%2F"), "{url}");
    assert!(
        url.ends_with(&*jpg.file_name().unwrap().to_string_lossy()),
        "{url}"
    );
    let ts = r.lib.track(&id(&small)).unwrap();
    assert_eq!(dims(&ts.cover.unwrap()), (300, 200), "小图不放大");
    let tn = r.lib.track(&id(&none)).unwrap();
    assert_eq!((tn.cover_url, tn.cover), (None, None));

    // 重启后缩略图沿用（内容已坏，只能来自缓存）
    spoil_keeping_stat(&big);
    let (lib, rx) = open(&r.data());
    lib.set_dir(&r.out);
    settled(&rx);
    assert_eq!(lib.track(&id(&big)).unwrap().cover_url, Some(url));
    assert!(jpg.exists());

    std::fs::remove_file(&big).unwrap();
    lib.refresh();
    settled(&rx);
    assert!(!jpg.exists(), "文件删了缩略图也删");
}

#[test]
fn same_name_lrc_gives_lyrics() {
    let r = Rig::new("lrc");
    let a = mp3(&r.out.join("a.mp3"), &[], None);
    let b = mp3(&r.out.join("b.mp3"), &[], None);
    let c = mp3(&r.out.join("c.mp3"), &[], None);
    std::fs::write(
        r.out.join("a.LRC"),
        "[ti:x]\n[00:01.00]第一句\n[00:01.00]first\n[00:02.50]第二句\n",
    )
    .unwrap();
    std::fs::write(r.out.join("b.lrc"), "[ti:只有标签]\n").unwrap();
    r.lib.set_dir(&r.out);
    settled(&r.rx);

    assert!(r.lib.track(&id(&a)).unwrap().has_lyrics);
    let l = r.lib.lyrics(&id(&a)).unwrap();
    assert_eq!(l.len(), 2);
    assert_eq!(
        (l[0].ms, l[0].text.as_str(), l[0].trans.as_deref()),
        (1000, "第一句", Some("first"))
    );
    assert_eq!((l[1].ms, l[1].trans.as_deref()), (2500, None));
    let j = serde_json::to_value(&l).unwrap();
    assert_eq!(j[0]["trans"], "first");
    assert!(j[1].get("trans").is_none(), "{j}");

    assert!(!r.lib.track(&id(&b)).unwrap().has_lyrics, "解析为空");
    assert_eq!(r.lib.lyrics(&id(&b)), None);
    assert!(!r.lib.track(&id(&c)).unwrap().has_lyrics);
    assert_eq!(r.lib.lyrics(&id(&c)), None);
    assert_eq!(
        r.lib.lyrics(&id(&r.out.join("a.LRC"))),
        None,
        "不是资料库里的歌"
    );

    // 后放进来的歌词也算
    std::fs::write(r.out.join("c.lrc"), "[00:00.10]来了\n").unwrap();
    r.lib.refresh();
    let (_, v) = settled(&r.rx);
    assert!(v.tracks.iter().find(|t| t.id == id(&c)).unwrap().has_lyrics);
}

#[test]
fn trash_moves_song_and_its_lrc_to_the_trash_and_refuses_strangers() {
    let r = Rig::new("trash");
    let tag = format!(
        "mr-trash-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let a = mp3(&r.out.join(format!("{tag}.mp3")), &[], None);
    let lrc = r.out.join(format!("{tag}.lrc"));
    std::fs::write(&lrc, "[00:01.00]词\n").unwrap();
    let txt = r.out.join(format!("{tag}.txt"));
    std::fs::write(&txt, "x").unwrap();
    r.lib.set_dir(&r.out);
    settled(&r.rx);

    for stranger in [&txt, &lrc, &r.t.0.join("nope.mp3")] {
        assert!(r.lib.trash(&id(stranger)).is_err(), "{stranger:?}");
        assert!(r.lib.reveal(&id(stranger)).is_err(), "{stranger:?}");
    }
    assert!(txt.exists() && lrc.exists() && a.exists());

    r.lib.trash(&id(&a)).unwrap();
    let bin = PathBuf::from(std::env::var_os("HOME").unwrap()).join(".Trash");
    let gone: Vec<PathBuf> = [&a, &lrc]
        .iter()
        .map(|p| bin.join(p.file_name().unwrap()))
        .collect();
    let in_bin: Vec<bool> = gone.iter().map(|p| p.exists()).collect();
    for p in &gone {
        let _ = std::fs::remove_file(p);
    }
    assert_eq!(in_bin, [true, true], "废纸篓里应有同名项");
    assert!(!a.exists() && !lrc.exists() && txt.exists());
    assert!(r.lib.track(&id(&a)).is_none(), "立即从资料库消失");
    let (_, v) = settled(&r.rx);
    assert!(v.tracks.is_empty());
}
