//! 用法：cargo run --release -p reveal-core --example decrypt_only -- <输入> <输出> [ekey]
//! 供交叉核对与计时用；打印识别结果、容器与解密耗时。

use std::io::BufWriter;
use std::time::Instant;

/// 输出已存在且与输入指向同一 inode（同路径、硬链接、符号链接）时返回 true。
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    let (Ok(x), Ok(y)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        x.dev() == y.dev() && x.ino() == y.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (x, y);
        matches!((a.canonicalize(), b.canonicalize()), (Ok(p), Ok(q)) if p == q)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let [_, input, output, rest @ ..] = &args[..] else {
        return Err("用法：decrypt_only <输入> <输出> [ekey]".into());
    };
    let input = std::path::Path::new(input);
    if same_file(input, output.as_ref()) {
        return Err("输出与输入是同一个文件（含硬链接/符号链接），拒绝执行以免截断输入".into());
    }
    let t = Instant::now();
    let kind = reveal_core::sniff(input)?;
    let mut out = BufWriter::new(std::fs::File::create(output)?);
    let d = reveal_core::decrypt(input, &kind, rest.first().map(String::as_str), &mut out)?;
    drop(out);
    let label = match &kind {
        reveal_core::SourceKind::QmcV2 { .. } => "QmcV2".into(),
        reveal_core::SourceKind::MusicEx { .. } => "MusicEx".into(),
        k => format!("{k:?}"),
    };
    println!(
        "{label} {:?} {}B {:.3}s",
        d.container,
        std::fs::metadata(output)?.len(),
        t.elapsed().as_secs_f64()
    );
    Ok(())
}
