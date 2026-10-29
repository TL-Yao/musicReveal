//! 本机真实文件测试：`cargo test -p reveal-core -- --ignored --nocapture`。
//! 样本在主仓库的 `testdata/real-samples`（不入库，可用 `REVEAL_NCM_DIR` 换目录），先复制到临时目录
//! 再用，样本本身只读；输出写临时目录，不把任何真实音频或 ekey 带进仓库。

use aes::cipher::{BlockDecrypt, KeyInit};
use base64::Engine;
use reveal_core::{decrypt, read_track, sniff, Container, Platform, SongId, SourceKind};
use std::path::PathBuf;
use std::process::Command;

/// 真实样本目录：主仓库（从 worktree 里跑也找得到）的 `testdata/real-samples`，版权音乐，不入库。
fn samples() -> PathBuf {
    let o = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("需要 git 来定位主仓库");
    assert!(o.status.success(), "定位不到主仓库");
    let git = PathBuf::from(String::from_utf8(o.stdout).unwrap().trim());
    git.parent().unwrap().join("testdata/real-samples")
}

/// 样本目录：`REVEAL_NCM_DIR` 或 `samples()`。
fn sample_dir() -> PathBuf {
    std::env::var_os("REVEAL_NCM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(samples)
}

/// 样本目录里扩展名为 `ext` 的文件复制进本测试自己的临时目录；离开作用域时删掉。
struct Staged(PathBuf);

impl Staged {
    fn new(tag: &str, ext: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("reveal-real-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = sample_dir();
        for e in std::fs::read_dir(&src)
            .unwrap_or_else(|e| panic!("{}：{e}", src.display()))
            .flatten()
        {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some(ext) {
                std::fs::copy(&p, dir.join(p.file_name().unwrap())).unwrap();
            }
        }
        Staged(dir)
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 独立于被测代码，直接解出 NCM 的元数据 JSON，作为断言基准。
fn ncm_json(data: &[u8]) -> serde_json::Value {
    let u32le = |p: usize| u32::from_le_bytes(data[p..p + 4].try_into().unwrap()) as usize;
    let key_len = u32le(10);
    let p = 14 + key_len;
    let meta_len = u32le(p);
    let x: Vec<u8> = data[p + 4..p + 4 + meta_len]
        .iter()
        .map(|b| b ^ 0x63)
        .collect();
    let mut enc = base64::engine::general_purpose::STANDARD
        .decode(&x[22..])
        .unwrap();
    let c = aes::Aes128::new(b"#14ljk_!\\]&0U<'(".into());
    for b in enc.chunks_mut(16) {
        c.decrypt_block(b.into());
    }
    let pad = *enc.last().unwrap() as usize;
    serde_json::from_slice(&enc[6..enc.len() - pad]).unwrap()
}

fn ffmpeg(bin: &str, before: &[&str], file: &PathBuf, after: &[&str]) -> String {
    let o = Command::new(bin)
        .args(before)
        .arg(file)
        .args(after)
        .output()
        .unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
#[ignore]
fn real_ncm_files_decode_cleanly() {
    let staged = Staged::new("ncm", "ncm");
    let dir = &staged.0;
    let names = [
        "著小生zoki,徵羽摩柯,乐正龙牙 - 雪乡游梦记（伴奏）.ncm",
        "DMYoung,Hanser,泠鸢yousa - 「交织together」.ncm",
        "Hanser - 啵唧.ncm",
    ];
    let tmp = std::env::temp_dir().join(format!("reveal-real-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    for (i, name) in names.iter().enumerate() {
        let src = dir.join(name);
        let json = ncm_json(&std::fs::read(&src).unwrap_or_else(|e| panic!("缺样本 {name}：{e}")));
        let want_artists: Vec<&str> = json["artist"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a[0].as_str().unwrap())
            .collect();

        assert_eq!(sniff(&src).unwrap(), SourceKind::Ncm);
        let out = tmp.join(format!("{i}.mp3"));
        let mut w = std::io::BufWriter::new(std::fs::File::create(&out).unwrap());
        let d = decrypt(&src, &SourceKind::Ncm, None, &mut w).unwrap();
        drop(w);

        assert_eq!(d.container, Container::Mp3, "{name}");
        assert_eq!(d.meta.artists, want_artists, "{name}");
        if i == 0 {
            assert_eq!(d.meta.artists.len(), 4);
        }
        assert!(
            d.meta.cover.as_ref().is_some_and(|c| !c.is_empty()),
            "{name}"
        );

        let errs = ffmpeg("ffmpeg", &["-v", "error", "-i"], &out, &["-f", "null", "-"]);
        assert_eq!(errs, "", "{name}: ffmpeg 报错");
        let probe = [
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ];
        let dur: f64 = ffmpeg("ffprobe", &probe, &out, &[]).trim().parse().unwrap();
        let meta_dur = json["duration"].as_f64().unwrap() / 1000.0;
        println!(
            "{name}: artists={:?} cover={}B ffprobe={dur:.3}s meta={meta_dur:.3}s",
            d.meta.artists,
            d.meta.cover.as_ref().unwrap().len()
        );
        assert!(
            (dur - meta_dur).abs() < 1.0,
            "{name}: 时长 {dur} vs {meta_dur}"
        );
    }
    std::fs::remove_dir_all(&tmp).unwrap();
}

#[test]
#[ignore]
fn real_musicex_sniffs() {
    let staged = Staged::new("mflac", "mflac");
    let p = staged.0.join("hanser-二次元凶.mflac");
    match sniff(&p).unwrap() {
        SourceKind::MusicEx {
            song_mid,
            media_filename,
        } => {
            assert!(!song_mid.is_empty() && !media_filename.is_empty());
            let mask = |s: &str| format!("{}…({} 字符)", &s[..4], s.len());
            println!(
                "MusicEx song_mid={} media_filename={}",
                mask(&song_mid),
                mask(&media_filename)
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
#[ignore]
fn all_local_ncm_files_have_sniffable_container() {
    let mut stats = std::collections::BTreeMap::<String, usize>::new();
    let mut failed = Vec::new();
    let mut seen = 0;
    let staged = Staged::new("ncm-all", "ncm");
    for e in std::fs::read_dir(&staged.0).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) != Some("ncm") {
            continue;
        }
        seen += 1;
        // 只要容器判定与完整解密是否成功，输出丢进 sink，不落盘
        match sniff(&p).and_then(|k| decrypt(&p, &k, None, &mut std::io::sink())) {
            Ok(d) => *stats.entry(format!("{:?}", d.container)).or_default() += 1,
            Err(err) => failed.push(format!("{}: {err}", p.display())),
        }
    }
    println!("NCM 统计：{stats:?}，失败 {}：{failed:#?}", failed.len());
    assert!(seen > 0, "{} 下没有 .ncm 文件", sample_dir().display());
    assert!(failed.is_empty());
}

#[test]
#[ignore]
fn real_netease_mp3s_are_plain_with_163_key() {
    let staged = Staged::new("mp3", "mp3");
    let mut mp3s: Vec<PathBuf> = std::fs::read_dir(&staged.0)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("mp3"))
        .collect();
    mp3s.sort();
    assert!(
        mp3s.len() >= 5,
        "{} 下 .mp3 不足 5 个",
        sample_dir().display()
    );
    // 均匀取 5 个；只读，解出的音频丢进 sink
    for p in mp3s.iter().step_by(mp3s.len() / 5).take(5) {
        let kind = sniff(p).unwrap();
        assert_eq!(kind, SourceKind::Plain(Container::Mp3), "{}", p.display());
        let t = read_track(p).unwrap();
        assert!(t.title.is_some(), "{}", p.display());
        let d = decrypt(p, &kind, None, &mut std::io::sink()).unwrap();
        assert_eq!(d.meta.platform, Platform::Netease, "{}", p.display());
        assert!(
            matches!(d.meta.song_id, Some(SongId::Netease(id)) if id > 0),
            "{}",
            p.display()
        );
        println!(
            "{}: {:?} {:?} {}",
            p.display(),
            t.title,
            t.artists,
            t.quality
        );
    }
}
