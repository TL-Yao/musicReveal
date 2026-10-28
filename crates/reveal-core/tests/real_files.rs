//! 本机真实文件测试：`cargo test -p reveal-core -- --ignored --nocapture`。
//! 只读本机路径、输出写临时目录，不把任何真实音频或 ekey 带进仓库。

use aes::cipher::{BlockDecrypt, KeyInit};
use base64::Engine;
use reveal_core::{decrypt, sniff, Container, SourceKind};
use std::path::PathBuf;
use std::process::Command;

fn home(rel: &str) -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap()).join(rel)
}

/// 本机 NCM 所在目录；文件被挪走时用 `REVEAL_NCM_DIR` 指定。
fn ncm_dir() -> PathBuf {
    std::env::var_os("REVEAL_NCM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home("Music/网易云音乐"))
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
    let dir = ncm_dir();
    let names = [
        "著小生zoki,徵羽摩柯,乐正龙牙 - 雪乡游梦记（伴奏）.ncm",
        "DMYoung,Hanser,泠鸢yousa - 「交织together」.ncm",
        "Hanser - 啵唧.ncm",
    ];
    let tmp = std::env::temp_dir().join(format!("reveal-real-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    for (i, name) in names.iter().enumerate() {
        let src = dir.join(name);
        let json = ncm_json(&std::fs::read(&src).unwrap());
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
    let p = home("Downloads/hanser/hanser-二次元凶.mflac");
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
    for e in std::fs::read_dir(ncm_dir()).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) != Some("ncm") {
            continue;
        }
        // 只要容器判定与完整解密是否成功，输出丢进 sink，不落盘
        match sniff(&p).and_then(|k| decrypt(&p, &k, None, &mut std::io::sink())) {
            Ok(d) => *stats.entry(format!("{:?}", d.container)).or_default() += 1,
            Err(err) => failed.push(format!("{}: {err}", p.display())),
        }
    }
    println!("NCM 统计：{stats:?}，失败 {}：{failed:#?}", failed.len());
    assert!(failed.is_empty());
}
