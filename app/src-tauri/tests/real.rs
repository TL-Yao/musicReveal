//! 真实端到端：真实转换器 + 自动轮询，监听与输出都在临时目录。
//! 源文件只读复制自 `~/Downloads/hanser/`（可用 REVEAL_E2E_DIR 覆盖）。

use musicreveal_app::engine::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn pick(dir: &Path, ext: &str, must: &str, n: usize) -> Vec<PathBuf> {
    let mut all: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == ext))
        .filter(|p| p.file_name().is_some_and(|f| f != must))
        .collect();
    all.sort();
    let mut out = vec![dir.join(must)];
    out.extend(all.into_iter().take(n - 1));
    out
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "需要本机真实文件、QQ 登录与网络"]
async fn real_files_dropped_into_watch_folder_get_converted() {
    let src_dir = std::env::var_os("REVEAL_E2E_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("Downloads/hanser"));
    let mut files = pick(&src_dir, "mflac", "hanser-二次元凶.mflac", 3);
    files.extend(pick(&src_dir, "ncm", "Hanser - 啵唧.ncm", 3));

    let tmp = std::env::temp_dir().join(format!("mr-e2e-{}", std::process::id()));
    let (watch, out, data) = (tmp.join("watch"), tmp.join("out"), tmp.join("data"));
    std::fs::create_dir_all(&watch).unwrap();
    let eng = Engine::new(data, Arc::new(RealConverter::new(home())), Box::new(|_| {}))
        .with_poll_interval(Some(Duration::from_millis(500)));
    eng.start().await;
    eng.complete_onboarding(
        vec![watch.to_string_lossy().into()],
        out.to_string_lossy().into(),
        "original",
    )
    .unwrap();
    for f in &files {
        std::fs::copy(f, watch.join(f.file_name().unwrap())).unwrap();
    }

    let t0 = Instant::now();
    let jobs = loop {
        let s = eng.snapshot();
        let finished = s
            .jobs
            .iter()
            .filter(|j| {
                matches!(
                    j.state,
                    JobState::Done | JobState::Failed | JobState::WaitLogin
                )
            })
            .count();
        if finished == files.len() || t0.elapsed() > Duration::from_secs(600) {
            break s.jobs;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    eng.shutdown().await;

    let mut bad = vec![];
    for j in &jobs {
        println!(
            "{:?} {} → {} [{}] 歌手={:?} 封面={} 歌词={} {:?}",
            j.state,
            j.file_name,
            j.output.as_deref().unwrap_or("-"),
            j.quality.as_deref().unwrap_or("-"),
            j.artists,
            j.has_cover,
            j.has_lyrics,
            j.error
        );
        if j.state != JobState::Done {
            bad.push(format!("{} 状态 {:?}", j.file_name, j.state));
            continue;
        }
        let o = PathBuf::from(j.output.as_ref().unwrap());
        assert!(o.starts_with(&out));
        assert!(std::fs::metadata(&o).unwrap().len() > 0);
        // 校验解码用完整 ffmpeg：`REVEAL_GEN_FFMPEG` 优先（随包精简版没有 null 输出）
        let ff = std::env::var_os("REVEAL_GEN_FFMPEG")
            .map(PathBuf::from)
            .or_else(reveal_core::find_ffmpeg)
            .unwrap();
        let r = std::process::Command::new(ff)
            .args(["-v", "error", "-i"])
            .arg(&o)
            .args(["-f", "null", "-"])
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&r.stderr);
        if !r.status.success() || !err.trim().is_empty() {
            bad.push(format!("{} ffmpeg: {err}", o.display()));
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert_eq!(jobs.len(), files.len(), "{jobs:?}");
    assert!(bad.is_empty(), "{bad:#?}");
}
