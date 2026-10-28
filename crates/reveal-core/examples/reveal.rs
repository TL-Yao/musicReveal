//! 用法：cargo run --release -p reveal-core --example reveal -- <输入> <输出> [ekey]
//! 供交叉核对与计时用；打印识别结果、容器与解密耗时。

use std::io::BufWriter;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let [_, input, output, rest @ ..] = &args[..] else {
        return Err("用法：reveal <输入> <输出> [ekey]".into());
    };
    let input = std::path::Path::new(input);
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
