//! musicReveal 解密核心：识别 NCM / QMC 加密音频并流式解密。平台无关、无网络。

use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum Platform {
    Qq,
    Netease,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SongId {
    QqMid(String),
    Netease(u64),
}

/// 按解密后音频头嗅探出的容器格式。
#[derive(Debug, Clone, PartialEq)]
pub enum Container {
    Flac,
    Ogg,
    M4a,
    Mp3,
    Wav,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceKind {
    Ncm,
    QmcV1,
    /// 文件尾内嵌 ekey（QTag / PcV1Legacy）。
    QmcV2 {
        ekey: String,
    },
    /// 新版 musicex V1 footer，ekey 需在线获取。
    MusicEx {
        song_mid: String,
        media_filename: String,
    },
    /// 扩展名像加密格式但内容是普通音频。
    Plain(Container),
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddedMeta {
    pub title: Option<String>,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub cover: Option<Vec<u8>>,
    pub song_id: Option<SongId>,
    pub platform: Platform,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decrypted {
    pub container: Container,
    pub meta: EmbeddedMeta,
}

#[derive(Debug)]
pub enum Error {
    Unsupported(String),
    Incomplete,
    NeedsEkey,
    BadEkey,
    Io(std::io::Error),
    Corrupt(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported(s) => write!(f, "不支持的格式：{s}"),
            Error::Incomplete => f.write_str("文件不完整（可能仍在下载或已被截断）"),
            Error::NeedsEkey => f.write_str("该文件需要 ekey 才能解密"),
            Error::BadEkey => f.write_str("ekey 无效或与该文件不匹配"),
            Error::Io(e) => write!(f, "读写失败：{e}"),
            Error::Corrupt(s) => write!(f, "文件已损坏：{s}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

mod ncm;
mod qmc;

const BUF: usize = 1 << 20;

fn eof(e: std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::UnexpectedEof {
        Error::Incomplete
    } else {
        Error::Io(e)
    }
}

fn sniff_container(h: &[u8]) -> Option<Container> {
    Some(match h {
        [b'f', b'L', b'a', b'C', ..] => Container::Flac,
        [b'O', b'g', b'g', b'S', ..] => Container::Ogg,
        [b'I', b'D', b'3', ..] => Container::Mp3,
        [0xFF, b, ..] if b & 0xE0 == 0xE0 => Container::Mp3,
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'A', b'V', b'E', ..] => Container::Wav,
        [_, _, _, _, b'f', b't', b'y', b'p', ..] => Container::M4a,
        _ => return None,
    })
}

/// 分块读 → 就地解密（`xor(buf, 音频内偏移)`）→ 写出；首块先嗅探容器，认不出时交给 `unknown`。
fn pump(
    mut r: impl Read,
    out: &mut dyn Write,
    mut xor: impl FnMut(&mut [u8], u64),
    unknown: impl FnOnce() -> Result<Container, Error>,
) -> Result<Container, Error> {
    let mut buf = Vec::with_capacity(BUF);
    (&mut r).take(BUF as u64).read_to_end(&mut buf)?;
    if buf.is_empty() {
        return Err(Error::Incomplete);
    }
    xor(&mut buf, 0);
    let container = match sniff_container(&buf) {
        Some(c) => c,
        None => unknown()?,
    };
    out.write_all(&buf)?;
    let mut off = buf.len() as u64;
    buf.resize(BUF, 0);
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            return Ok(container);
        }
        xor(&mut buf[..n], off);
        out.write_all(&buf[..n])?;
        off += n as u64;
    }
}

fn read_head(f: &mut File, n: usize) -> Result<Vec<u8>, Error> {
    let mut v = Vec::with_capacity(n);
    f.take(n as u64).read_to_end(&mut v)?;
    Ok(v)
}

fn read_tail(f: &mut File) -> Result<Vec<u8>, Error> {
    let len = f.metadata()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(qmc::TAIL)))?;
    read_head(f, qmc::TAIL as usize)
}

/// QMC2：按文件尾确定音频长度，用 ekey 派生的主密钥流式解密。
fn decrypt_qmc2(
    mut f: File,
    ekey: &str,
    bad: fn() -> Error,
    out: &mut dyn Write,
) -> Result<Container, Error> {
    let len = f.metadata()?.len();
    let ft = qmc::parse_footer(&read_tail(&mut f)?)?.ok_or(Error::Incomplete)?;
    let cipher = qmc::Cipher::new(&qmc::derive_key(ekey).ok_or_else(bad)?);
    f.seek(SeekFrom::Start(0))?;
    pump(
        f.take(len - ft.size),
        out,
        |b, o| cipher.xor(b, o),
        || Err(bad()),
    )
}

pub fn sniff(path: &Path) -> Result<SourceKind, Error> {
    let mut f = File::open(path)?;
    let head = read_head(&mut f, 16)?;
    if head.starts_with(ncm::MAGIC) {
        ncm::read_header(&mut f)?;
        return Ok(SourceKind::Ncm);
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let (v1, v2) = (
        qmc::V1_EXTS.contains(&ext.as_str()),
        qmc::V2_EXTS.contains(&ext.as_str()),
    );
    if !v1 && !v2 {
        return Err(Error::Unsupported(format!("无法识别的扩展名 .{ext}")));
    }
    if head.is_empty() {
        return Err(Error::Incomplete);
    }
    if v2 {
        if let Some(ft) = qmc::parse_footer(&read_tail(&mut f)?)? {
            return Ok(ft.kind);
        }
    }
    if let Some(c) = sniff_container(&head) {
        return Ok(SourceKind::Plain(c));
    }
    let mut h = head.clone();
    qmc::Cipher::static_v1().xor(&mut h, 0);
    if v1 || sniff_container(&h).is_some() {
        return Ok(SourceKind::QmcV1);
    }
    Err(Error::Incomplete)
}

fn qq_meta() -> EmbeddedMeta {
    EmbeddedMeta {
        title: None,
        artists: vec![],
        album: None,
        cover: None,
        song_id: None,
        platform: Platform::Qq,
    }
}

/// ekey 仅 MusicEx 需要；QmcV2 用 kind 里的；其余忽略。流式写出。
pub fn decrypt(
    path: &Path,
    kind: &SourceKind,
    ekey: Option<&str>,
    out: &mut dyn Write,
) -> Result<Decrypted, Error> {
    let f = File::open(path)?;
    let corrupt = || Err(Error::Corrupt("解密后不是可识别的音频".into()));
    let container = match kind {
        SourceKind::Ncm => return ncm::decrypt(f, out),
        SourceKind::QmcV1 => {
            let c = qmc::Cipher::static_v1();
            pump(f, out, |b, o| c.xor(b, o), corrupt)?
        }
        SourceKind::Plain(_) => pump(f, out, |_, _| {}, corrupt)?,
        SourceKind::QmcV2 { ekey } => decrypt_qmc2(
            f,
            ekey,
            || Error::Corrupt("内嵌 ekey 无法解密该文件".into()),
            out,
        )?,
        SourceKind::MusicEx { song_mid, .. } => {
            let ekey = ekey.ok_or(Error::NeedsEkey)?;
            let container = decrypt_qmc2(f, ekey, || Error::BadEkey, out)?;
            return Ok(Decrypted {
                container,
                meta: EmbeddedMeta {
                    song_id: Some(SongId::QqMid(song_mid.clone())),
                    ..qq_meta()
                },
            });
        }
    };
    Ok(Decrypted {
        container,
        meta: qq_meta(),
    })
}

#[cfg(test)]
mod tests;
