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
    // 生成素材：`REVEAL_GEN_FFMPEG` 优先（随包精简版做 REVEAL_FFMPEG 时用完整版生成）
    let ff = std::env::var_os("REVEAL_GEN_FFMPEG")
        .map(PathBuf::from)
        .or_else(reveal_core::find_ffmpeg)
        .expect("需要 ffmpeg（可设 REVEAL_FFMPEG / REVEAL_GEN_FFMPEG）");
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
    // 资料库不受监听扫描的 3 层限制：第 5 层照收
    let deep = mp3(&o.join("sub/1/2/3/4/深.mp3"), &[], None);
    // 符号链目录不进（含指回自己的环）
    let elsewhere = r.t.0.join("elsewhere");
    mp3(&elsewhere.join("外.mp3"), &[], None);
    std::os::unix::fs::symlink(&elsewhere, o.join("link")).unwrap();
    std::os::unix::fs::symlink(o, o.join("sub/loop")).unwrap();
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
    assert_eq!(
        ids(&v),
        [id(&a), id(&deep), id(&c), id(&b)].map(|s| s.to_string())
    );

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
fn trash_moves_song_and_its_lrcs_to_the_trash_and_refuses_strangers() {
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
    let beside = r.out.join(format!("{tag}.lrc"));
    std::fs::write(&beside, "[00:01.00]词\n").unwrap();
    let txt = r.out.join(format!("{tag}.txt"));
    std::fs::write(&txt, "x").unwrap();
    r.lib.set_dir(&r.out);
    settled(&r.rx);
    let home = r.out.join(format!("lrc/{tag}.lrc"));
    assert!(home.exists() && !beside.exists(), "扫描时挪进 lrc 文件夹");
    // 旁边又放了一份（还没扫到）
    std::fs::write(&beside, "[00:02.00]又一份\n").unwrap();

    for stranger in [&txt, &home, &beside, &r.t.0.join("nope.mp3")] {
        assert!(r.lib.trash(&id(stranger)).is_err(), "{stranger:?}");
        assert!(r.lib.reveal(&id(stranger)).is_err(), "{stranger:?}");
    }
    assert!(txt.exists() && home.exists() && beside.exists() && a.exists());

    r.lib.trash(&id(&a)).unwrap();
    let bin = PathBuf::from(std::env::var_os("HOME").unwrap()).join(".Trash");
    let in_bin = bin.join(a.file_name().unwrap()).exists();
    for e in std::fs::read_dir(&bin).into_iter().flatten().flatten() {
        if e.file_name().to_string_lossy().starts_with(&tag) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    assert!(in_bin, "废纸篓里应有这首歌");
    assert!(!a.exists() && !home.exists() && !beside.exists() && txt.exists());
    assert!(r.lib.track(&id(&a)).is_none(), "立即从资料库消失");
    let (_, v) = settled(&r.rx);
    assert!(v.tracks.is_empty());
}

#[test]
fn trash_does_not_follow_an_lrc_folder_swapped_for_a_link_after_the_scan() {
    let r = Rig::new("trashlink");
    let tag = format!("mr-trashlink-{}", std::process::id());
    let a = mp3(&r.out.join(format!("{tag}.mp3")), &[], None);
    std::fs::write(r.out.join(format!("{tag}.lrc")), "[00:01.00]词\n").unwrap();
    r.lib.set_dir(&r.out);
    settled(&r.rx);
    let lrcs = r.out.join("lrc");
    assert!(lrcs.join(format!("{tag}.lrc")).exists());
    // 扫描之后：lrc 文件夹换成指向外部的链接，外部有同名文件
    std::fs::rename(&lrcs, r.out.join("lrc-old")).unwrap();
    let outside = r.t.0.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let victim = outside.join(format!("{tag}.lrc"));
    std::fs::write(&victim, "外部").unwrap();
    std::os::unix::fs::symlink(&outside, &lrcs).unwrap();

    r.lib.trash(&id(&a)).unwrap();
    let bin = PathBuf::from(std::env::var_os("HOME").unwrap()).join(".Trash");
    for e in std::fs::read_dir(&bin).into_iter().flatten().flatten() {
        if e.file_name().to_string_lossy().starts_with(&tag) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    assert!(!a.exists());
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "外部");
}

/// 大小、mtime、inode：文件被改写或替换都会变。
fn fingerprint(p: &Path) -> (u64, std::time::SystemTime, u64) {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(p).unwrap();
    (m.len(), m.modified().unwrap(), m.ino())
}

/// 目录下全部文件（递归）的指纹。
fn fingerprints(
    dir: &Path,
) -> std::collections::BTreeMap<PathBuf, (u64, std::time::SystemTime, u64)> {
    let mut out = std::collections::BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(fingerprints(&p));
        } else {
            out.insert(p.clone(), fingerprint(&p));
        }
    }
    out
}

/// 再扫一次并等它完成：没变化的重扫不推送，所以顺带放一首新歌 `zz-<n>.mp3`（不算进指纹）。
fn rescan(lib: &Library, rx: &Receiver<LibraryView>, dir: &Path, n: u32) {
    lib.refresh();
    mp3(&dir.join(format!("zz-{n}.mp3")), &[], None);
    lib.refresh();
    settled(rx);
}

fn without_markers(
    mut f: std::collections::BTreeMap<PathBuf, (u64, std::time::SystemTime, u64)>,
) -> std::collections::BTreeMap<PathBuf, (u64, std::time::SystemTime, u64)> {
    f.retain(|p, _| !p.file_name().unwrap().to_string_lossy().starts_with("zz-"));
    f
}

fn texts(l: &[Lyric]) -> Vec<&str> {
    l.iter().map(|l| l.text.as_str()).collect()
}

#[test]
fn scan_embeds_usable_lrc_and_files_it_under_the_lrc_folder() {
    let r = Rig::new("embed");
    let o = &r.out;
    // 已有带时间内嵌歌词：文件不动，旁边的 .lrc 挪进 lrc/，歌词以内嵌为准
    let timed = mp3(&o.join("timed.mp3"), &[], None);
    reveal_core::embed_lyrics(&timed, "[00:01.00]内嵌\n").unwrap();
    std::fs::write(o.join("timed.lrc"), "[00:05.00]旁边\n").unwrap();
    // 只有纯文本内嵌 + 子文件夹里旁边的 .lrc：换成 .lrc 原文；lrc/ 里已有的同名被覆盖
    let plain = mp3(&o.join("sub/plain.mp3"), &[], None);
    reveal_core::embed_lyrics(&plain, "纯文本").unwrap();
    let plain_lrc = "[ti:x]\n[00:01.00]换上\n[00:01.00]translated\n";
    std::fs::write(o.join("sub/plain.lrc"), plain_lrc).unwrap();
    std::fs::create_dir_all(o.join("lrc/sub")).unwrap();
    std::fs::write(o.join("lrc/sub/plain.lrc"), "旧的").unwrap();
    // 纯文本内嵌、没有 .lrc：不动
    let alone = mp3(&o.join("alone.mp3"), &[], None);
    reveal_core::embed_lyrics(&alone, "纯文本").unwrap();
    // 没有内嵌、旁边的 .lrc 解析不出带时间的行：不嵌不移
    let untimed = mp3(&o.join("untimed.mp3"), &[], None);
    std::fs::write(o.join("untimed.lrc"), "[ti:只有标签]\n").unwrap();
    // 没有内嵌、.lrc 只在 lrc/ 下：从那里嵌
    let homed = mp3(&o.join("homed.mp3"), &[], None);
    std::fs::write(o.join("lrc/homed.lrc"), "[00:02.00]在家\n").unwrap();
    // lrc/ 里没有对应歌的 .lrc：不是曲目
    std::fs::write(o.join("lrc/orphan.lrc"), "[00:01.00]孤儿\n").unwrap();

    let born = |p: &Path| std::fs::metadata(p).unwrap().created().unwrap();
    let plain_born = born(&plain);
    let before = fingerprints(o);
    r.lib.set_dir(o);
    let (_, v) = settled(&r.rx);
    assert_eq!(
        ids(&v),
        [&alone, &homed, &plain, &timed, &untimed].map(|p| id(p))
    );

    // timed
    assert_eq!(fingerprint(&timed), before[&timed]);
    assert!(!o.join("timed.lrc").exists());
    assert_eq!(
        std::fs::read_to_string(o.join("lrc/timed.lrc")).unwrap(),
        "[00:05.00]旁边\n"
    );
    let t = r.lib.track(&id(&timed)).unwrap();
    assert!(t.has_lyrics);
    assert_eq!(texts(&r.lib.lyrics(&id(&timed)).unwrap()), ["内嵌"]);

    // plain
    assert_eq!(reveal_core::read_lyrics(&plain).as_deref(), Some(plain_lrc));
    assert!(!o.join("sub/plain.lrc").exists());
    assert_eq!(
        std::fs::read_to_string(o.join("lrc/sub/plain.lrc")).unwrap(),
        plain_lrc
    );
    let t = r.lib.track(&id(&plain)).unwrap();
    assert!(t.has_lyrics);
    let born_ms = plain_born.duration_since(std::time::UNIX_EPOCH).unwrap();
    assert_eq!(
        t.added_at as u128,
        born_ms.as_millis(),
        "最近添加的时间不变"
    );
    assert_eq!(born(&plain), plain_born);
    let l = r.lib.lyrics(&id(&plain)).unwrap();
    assert_eq!(
        (l[0].text.as_str(), l[0].trans.as_deref()),
        ("换上", Some("translated"))
    );

    // alone
    assert_eq!(fingerprint(&alone), before[&alone]);
    assert!(!r.lib.track(&id(&alone)).unwrap().has_lyrics);
    assert_eq!(r.lib.lyrics(&id(&alone)), None);

    // untimed
    assert_eq!(fingerprint(&untimed), before[&untimed]);
    assert!(o.join("untimed.lrc").exists() && !o.join("lrc/untimed.lrc").exists());
    assert!(!r.lib.track(&id(&untimed)).unwrap().has_lyrics);

    // homed
    assert_eq!(
        reveal_core::read_lyrics(&homed).as_deref(),
        Some("[00:02.00]在家\n")
    );
    assert!(r.lib.track(&id(&homed)).unwrap().has_lyrics);

    // 再扫一次：一个文件都不改
    let after = fingerprints(o);
    rescan(&r.lib, &r.rx, o, 1);
    assert_eq!(without_markers(fingerprints(o)), after);
    // 重启后（缓存）也一样
    let (lib, rx) = open(&r.data());
    lib.set_dir(o);
    let (_, v2) = settled(&rx);
    assert_eq!(without_markers(fingerprints(o)), after);
    assert_eq!(v2.tracks, r.lib.view().tracks);
}

#[test]
fn lrc_folder_linked_outside_is_left_alone() {
    let r = Rig::new("lrclink");
    let outside = r.t.0.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let victim = outside.join("a.lrc");
    std::fs::write(&victim, "[00:09.00]外部\n").unwrap();
    std::os::unix::fs::symlink(&outside, r.out.join("lrc")).unwrap();
    let a = mp3(&r.out.join("a.mp3"), &[], None);
    let beside = r.out.join("a.lrc");
    std::fs::write(&beside, "[00:01.00]旁边\n").unwrap();
    r.lib.set_dir(&r.out);
    settled(&r.rx);
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "[00:09.00]外部\n"
    );
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    assert!(beside.exists(), "挪不进去就留在旁边");
    let t = r.lib.track(&id(&a)).unwrap();
    assert!(t.has_lyrics);
    assert_eq!(texts(&r.lib.lyrics(&id(&a)).unwrap()), ["旁边"]);
    // 同状态不重试
    r.lib.refresh();
    let quiet = r.rx.recv_timeout(Duration::from_millis(500));
    assert!(quiet.is_err(), "{quiet:?}");
    // 删到废纸篓也不碰外部文件
    r.lib.trash(&id(&a)).unwrap();
    let bin = PathBuf::from(std::env::var_os("HOME").unwrap()).join(".Trash");
    for n in ["a.mp3", "a.lrc"] {
        let _ = std::fs::remove_file(bin.join(n));
    }
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "[00:09.00]外部\n"
    );
}

#[test]
fn read_only_song_keeps_its_lrc_beside_and_is_not_retried() {
    use std::os::unix::fs::PermissionsExt;
    let r = Rig::new("readonly");
    let a = mp3(&r.out.join("a.mp3"), &[], None);
    let lrc = r.out.join("a.lrc");
    std::fs::write(&lrc, "[00:01.00]只读\n").unwrap();
    std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o444)).unwrap();
    let before = fingerprints(&r.out);
    r.lib.set_dir(&r.out);
    settled(&r.rx);
    // 不重试：重扫无事可做，不进入「扫描中」，什么也不推送
    r.lib.refresh();
    let quiet = r.rx.recv_timeout(Duration::from_millis(500));
    assert!(quiet.is_err(), "{quiet:?}");
    for n in 0..2 {
        assert_eq!(without_markers(fingerprints(&r.out)), before);
        assert!(!r.out.join("lrc").exists());
        let t = r.lib.track(&id(&a)).unwrap();
        assert!(t.has_lyrics);
        assert_eq!(t.lrc.as_deref(), Some(&*lrc));
        assert_eq!(texts(&r.lib.lyrics(&id(&a)).unwrap()), ["只读"]);
        rescan(&r.lib, &r.rx, &r.out, n);
    }
    std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn a_thousand_cached_tracks_serialize_under_50ms() {
    let r = Rig::new("perf");
    let a = mp3(&r.out.join("0000.mp3"), &[("title", "歌")], None);
    for i in 1..1000 {
        std::fs::copy(&a, r.out.join(format!("{i:04}.mp3"))).unwrap();
    }
    r.lib.set_dir(&r.out);
    let (partial, _) = settled(&r.rx);
    assert!(
        !partial.is_empty() && partial.iter().all(|p| p.scanning),
        "首次扫描大目录先发部分结果：{}",
        partial.len()
    );

    let (lib, rx) = open(&r.data());
    lib.set_dir(&r.out);
    let (partial, v) = settled(&rx);
    assert!(partial.is_empty(), "全部命中缓存");
    assert_eq!(v.tracks.len(), 1000);
    let t0 = std::time::Instant::now();
    let json = serde_json::to_string(&lib.view()).unwrap();
    let took = t0.elapsed();
    eprintln!("1000 首 get_library 序列化：{took:?}，{} 字节", json.len());
    assert!(took < Duration::from_millis(50), "{took:?}");
}

/// 核对真实输出文件夹（默认 ~/Music/musicReveal，可用 REVEAL_LIBRARY_DIR 覆盖）：扫描会补嵌歌词、
/// 挪 .lrc，所以扫的是它在临时目录里的 APFS 克隆（`cp -c`，不占空间）；原文件夹只读。
#[test]
#[ignore = "读本机真实输出文件夹"]
fn real_output_dir_read_only_scan() {
    let real = std::env::var_os("REVEAL_LIBRARY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap()).join("Music/musicReveal")
        });
    let t = Tmp::new("real");
    let dir = t.0.join("library");
    let st = Command::new("cp")
        .arg("-cR")
        .arg(&real)
        .arg(&dir)
        .status()
        .unwrap();
    assert!(st.success(), "无法克隆 {}", real.display());
    let (lib, rx) = open(&t.0.join("data"));
    let t0 = std::time::Instant::now();
    lib.set_dir(&dir);
    let (partial, v) = settled(&rx);
    let cold = t0.elapsed();
    let covers = v.tracks.iter().filter(|t| t.cover_url.is_some()).count();
    let lyrics = v.tracks.iter().filter(|t| t.has_lyrics).count();
    eprintln!(
        "{}：{} 首，有封面 {covers}，有歌词 {lyrics}；首次扫描 {cold:?}（部分结果 {} 次）",
        dir.display(),
        v.tracks.len(),
        partial.len()
    );
    assert!(!v.tracks.is_empty());
    // 补嵌失败的歌，.lrc 还留在歌旁边
    let beside = v
        .tracks
        .iter()
        .filter(|t| Path::new(&t.id).with_extension("lrc").exists())
        .count();
    eprintln!("补嵌后仍有 {beside} 首的 .lrc 留在歌旁边");
    drop(lib);

    let (lib, rx) = open(&t.0.join("data"));
    let t0 = std::time::Instant::now();
    lib.set_dir(&dir);
    let (_, v2) = settled(&rx);
    let warm = t0.elapsed();
    let t1 = std::time::Instant::now();
    let json = serde_json::to_string(&lib.view()).unwrap();
    eprintln!(
        "缓存命中重扫 {warm:?}；get_library 序列化 {:?}（{} 字节）",
        t1.elapsed(),
        json.len()
    );
    assert_eq!(v2, v);
}

/// 扫描不长时间占着资料库：1000 首各配千行歌词，重扫期间 `view()` 每次都很快返回。
#[test]
fn view_stays_responsive_while_lyrics_are_parsed() {
    let r = Rig::new("lrclock");
    let a = mp3(&r.out.join("0000.mp3"), &[], None);
    for i in 1..1000 {
        std::fs::copy(&a, r.out.join(format!("{i:04}.mp3"))).unwrap();
    }
    r.lib.set_dir(&r.out);
    settled(&r.rx);
    let text: String = (0..1000)
        .map(|i| format!("[{:02}:{:02}.00]第 {i} 句歌词\n", i / 60, i % 60))
        .collect();
    for i in 0..1000 {
        std::fs::write(r.out.join(format!("{i:04}.lrc")), &text).unwrap();
    }

    r.lib.refresh();
    let t0 = std::time::Instant::now();
    let mut worst = Duration::ZERO;
    let v = loop {
        let t = std::time::Instant::now();
        let v = r.lib.view();
        worst = worst.max(t.elapsed());
        if v.tracks.iter().all(|t| t.has_lyrics) {
            break v;
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "60s 内没解析完");
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(v.tracks.len(), 1000);
    eprintln!("重扫期间 view() 最长 {worst:?}，总耗时 {:?}", t0.elapsed());
    assert!(worst < Duration::from_millis(50), "{worst:?}");
}

/// 扫描途中换目录：作废的那次扫描刚生成的缩略图不留在数据目录里。
#[test]
fn abandoned_scan_leaves_no_orphan_thumbnails() {
    let r = Rig::new("orphan");
    let a = mp3(&r.out.join("00.mp3"), &[], Some((700, 700)));
    for i in 1..20 {
        std::fs::copy(&a, r.out.join(format!("{i:02}.mp3"))).unwrap();
    }
    let empty = r.t.0.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    r.lib.set_dir(&r.out);
    // 第一份是读标签之前的空结果：此刻开始逐首读、逐首生成缩略图
    let first = r.rx.recv_timeout(Duration::from_secs(20)).unwrap();
    assert!(first.scanning && first.tracks.is_empty(), "{first:?}");
    r.lib.set_dir(&empty);
    let (_, v) = settled(&r.rx);
    assert_eq!(v.dir, id(&empty));
    let left: Vec<_> = std::fs::read_dir(covers_dir(&r.data()))
        .map(|d| d.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
}

/// 资料库不受监听扫描的 5000 条目上限：大目录里排在后面的歌也在。
#[test]
fn library_is_not_cut_off_by_the_watch_scan_entry_limit() {
    let r = Rig::new("many");
    let junk = r.out.join("junk");
    std::fs::create_dir_all(&junk).unwrap();
    for i in 0..5100 {
        std::fs::write(junk.join(format!("{i}.txt")), b"").unwrap();
    }
    let z = mp3(&r.out.join("zz/歌.mp3"), &[], None);
    r.lib.set_dir(&r.out);
    let (_, v) = settled(&r.rx);
    assert_eq!(ids(&v), [id(&z)]);
}

const CREDIT: &str =
    "{\"t\":0,\"c\":[{\"tx\":\"作词: \"},{\"tx\":\"Hanser\",\"li\":\"http://x/a.jpg\"}]}";

#[test]
fn scan_turns_netease_credit_lines_into_plain_lyric_lines() {
    let r = Rig::new("credits");
    let o = &r.out;
    let fixed = |body: &str| format!("[00:00.00]作词: Hanser\n{body}");
    // 内嵌歌词含署名行：按安全规则重写
    let a = mp3(&o.join("a.mp3"), &[], None);
    reveal_core::embed_lyrics(&a, &format!("{CREDIT}\n[00:01.00]一\n")).unwrap();
    // 没有内嵌，lrc/ 下的 .lrc 含署名行：改写，再嵌入改好的
    let b = mp3(&o.join("b.mp3"), &[], None);
    std::fs::create_dir_all(o.join("lrc")).unwrap();
    std::fs::write(o.join("lrc/b.lrc"), format!("{CREDIT}\n[00:02.00]二\n")).unwrap();
    // 已有内嵌，旁边的 .lrc 含署名行：改写后挪进 lrc/
    let c = mp3(&o.join("c.mp3"), &[], None);
    reveal_core::embed_lyrics(&c, "[00:03.00]三\n").unwrap();
    std::fs::write(o.join("c.lrc"), format!("{CREDIT}\r\n[00:03.00]三\r\n")).unwrap();
    // 解析不了的 JSON 行原样
    let d = mp3(&o.join("d.mp3"), &[], None);
    reveal_core::embed_lyrics(&d, "{坏的}\n[00:04.00]四\n").unwrap();

    let born = |p: &Path| std::fs::metadata(p).unwrap().created().unwrap();
    let (a_born, d_print) = (born(&a), fingerprint(&d));
    r.lib.set_dir(o);
    settled(&r.rx);

    assert_eq!(
        reveal_core::read_lyrics(&a).as_deref(),
        Some(&*fixed("[00:01.00]一\n"))
    );
    assert_eq!(born(&a), a_born, "创建时间不变");
    assert_eq!(
        texts(&r.lib.lyrics(&id(&a)).unwrap()),
        ["作词: Hanser", "一"]
    );
    assert_eq!(
        std::fs::read_to_string(o.join("lrc/b.lrc")).unwrap(),
        fixed("[00:02.00]二\n")
    );
    assert_eq!(
        reveal_core::read_lyrics(&b).as_deref(),
        Some(&*fixed("[00:02.00]二\n"))
    );
    assert!(!o.join("c.lrc").exists());
    assert_eq!(
        std::fs::read_to_string(o.join("lrc/c.lrc")).unwrap(),
        "[00:00.00]作词: Hanser\r\n[00:03.00]三\r\n"
    );
    assert_eq!(fingerprint(&d), d_print);
    assert_eq!(texts(&r.lib.lyrics(&id(&d)).unwrap()), ["四"]);

    // 幂等：再扫、重启后再扫，一个文件都不改
    let after = fingerprints(o);
    rescan(&r.lib, &r.rx, o, 1);
    assert_eq!(without_markers(fingerprints(o)), after);
    let (lib, rx) = open(&r.data());
    lib.set_dir(o);
    settled(&rx);
    assert_eq!(without_markers(fingerprints(o)), after);
}

#[test]
fn display_shows_credit_lines_even_before_cleanup() {
    let r = Rig::new("credits-ro");
    let o = &r.out;
    let a = mp3(&o.join("a.mp3"), &[], None);
    reveal_core::embed_lyrics(&a, &format!("{CREDIT}\n[00:01.00]一\n")).unwrap();
    // 只读：改不了，按失败记下不再试，但歌词视图照样显示成普通行
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o444)).unwrap();
    let print = fingerprint(&a);
    r.lib.set_dir(o);
    settled(&r.rx);
    assert_eq!(fingerprint(&a), print);
    assert_eq!(
        texts(&r.lib.lyrics(&id(&a)).unwrap()),
        ["作词: Hanser", "一"]
    );
    rescan(&r.lib, &r.rx, o, 1);
    assert_eq!(fingerprint(&a), print);
}
