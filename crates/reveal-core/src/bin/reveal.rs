//! 用法：reveal <文件...> -o <目录> [--apple]
//! 逐个转换并打印结果；任一失败退出码为 1。

use reveal_core::*;
use std::path::PathBuf;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let mut files = vec![];
    let (mut out, mut format) = (None, OutputFormat::Original);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-o" => out = args.next().map(PathBuf::from),
            "--apple" => format = OutputFormat::Apple,
            _ => files.push(PathBuf::from(a)),
        }
    }
    let Some(output_dir) = out.filter(|_| !files.is_empty()) else {
        eprintln!("用法：reveal <文件...> -o <目录> [--apple]");
        return 2.into();
    };
    if let Err(e) = std::fs::create_dir_all(&output_dir) {
        eprintln!("无法创建输出目录：{e}");
        return 2.into();
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let status = credentials_status(&home);
    eprintln!("QQ 音乐登录状态：{status:?}");
    let creds = load_credentials(&home).ok();
    let opts = ConvertOptions {
        output_dir,
        format,
        ffmpeg: find_ffmpeg(),
    };
    let enricher = OnlineEnricher::new();
    let mut failed = false;
    for f in &files {
        let r = convert(
            f,
            &opts,
            &enricher,
            creds.as_ref(),
            &CancelToken::default(),
            &mut |_| {},
        )
        .await;
        match r {
            Ok(o) => println!(
                "OK  {}\n    -> {}  [{}] 歌手：{} 封面：{}",
                f.display(),
                o.output.display(),
                o.quality,
                o.meta.artists.join(", "),
                if o.has_cover { "有" } else { "无" }
            ),
            Err(e) => {
                failed = true;
                let e = match (&e, creds.is_some()) {
                    (ConvertError::NeedsLogin, true) => {
                        "QQ 音乐登录已过期，请重新登录后再试".into()
                    }
                    _ => e.to_string(),
                };
                println!("ERR {}\n    {e}", f.display());
            }
        }
    }
    u8::from(failed).into()
}
