//! 只经由 `sniff` / `decrypt` 测行为；合成样本在此生成，不入库二进制。
//! 样本构造法参考 music-geshizhuanhuan（MIT）tests/builders.py。

use super::*;
use aes::cipher::{BlockEncrypt, KeyInit};
use std::path::PathBuf;

mod creds;
mod enrich;
mod flac_trim;
mod lyrics;
mod naming;
mod pipeline;
mod track;

// ---------- 通用工具 ----------

struct Tmp(pub(super) PathBuf);
impl Tmp {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "reveal-core-{}-{}-{tag}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Tmp(dir)
    }
    fn write(&self, name: &str, data: &[u8]) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, data).unwrap();
        p
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 伪随机填充（xorshift），前面接容器魔数。
fn audio(magic: &[u8], len: usize) -> Vec<u8> {
    let mut v = magic.to_vec();
    let mut x: u32 = 0x1234_5678;
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        v.push(x as u8);
    }
    v
}

fn mp3(len: usize) -> Vec<u8> {
    audio(b"ID3\x03\x00\x00\x00\x00\x00\x00", len)
}

fn run(path: &Path, ekey: Option<&str>) -> Result<(SourceKind, Decrypted, Vec<u8>), Error> {
    let kind = sniff(path)?;
    let mut out = Vec::new();
    let d = decrypt(path, &kind, ekey, &mut out)?;
    Ok((kind, d, out))
}

// ---------- NCM ----------

const NCM_CORE_KEY: &[u8; 16] = b"hzHRAmso5kInbaxW";
const NCM_META_KEY: &[u8; 16] = b"#14ljk_!\\]&0U<'(";

fn aes_ecb_pkcs7(key: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let c = aes::Aes128::new(key.into());
    let pad = 16 - data.len() % 16;
    let mut v = data.to_vec();
    v.extend(std::iter::repeat_n(pad as u8, pad));
    for b in v.chunks_mut(16) {
        c.encrypt_block(b.into());
    }
    v
}

fn b64(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn build_ncm(plain: &[u8], rc4_key: &[u8], meta_json: &str, cover: Option<&[u8]>) -> Vec<u8> {
    let key_blob: Vec<u8> = aes_ecb_pkcs7(NCM_CORE_KEY, &[b"neteasecloudmusic", rc4_key].concat())
        .into_iter()
        .map(|b| b ^ 0x64)
        .collect();
    let meta_enc = aes_ecb_pkcs7(NCM_META_KEY, format!("music:{meta_json}").as_bytes());
    let meta_blob: Vec<u8> = format!("163 key(Don't modify):{}", b64(&meta_enc))
        .into_bytes()
        .into_iter()
        .map(|b| b ^ 0x63)
        .collect();
    // RC4-KSA + box 派生，音频字节 i 用 box[(i+1) % 256]
    let mut s: Vec<u8> = (0..=255).collect();
    let mut j = 0u8;
    for i in 0..256 {
        j = j
            .wrapping_add(s[i])
            .wrapping_add(rc4_key[i % rc4_key.len()]);
        s.swap(i, j as usize);
    }
    let bx: Vec<u8> = (0..256)
        .map(|i| {
            let a = s[i] as usize;
            s[(a + s[(i + a) & 0xff] as usize) & 0xff]
        })
        .collect();
    let mut out = b"CTENFDAM\0\0".to_vec();
    out.extend((key_blob.len() as u32).to_le_bytes());
    out.extend(&key_blob);
    out.extend((meta_blob.len() as u32).to_le_bytes());
    out.extend(&meta_blob);
    out.extend([0u8; 5]);
    let cover = cover.unwrap_or(&[]);
    out.extend((cover.len() as u32).to_le_bytes());
    out.extend((cover.len() as u32).to_le_bytes());
    out.extend(cover);
    out.extend(
        plain
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ bx[(i + 1) & 0xff]),
    );
    out
}

const META_MULTI: &str = r#"{"musicId":"1352449924","musicName":"雪乡游梦记（伴奏）","artist":[["著小生zoki","12558238"],["徵羽摩柯",0],["乐正龙牙",0],["Hanser","1049179"]],"album":"测试专辑","bitrate":320000,"duration":216032,"format":"mp3"}"#;

#[test]
fn ncm_decrypts_audio_and_metadata() {
    let t = Tmp::new("ncm");
    let plain = mp3(300_000);
    let cover = b"\x89PNG\r\n\x1a\nfake-cover";
    let p = t.write(
        "a.ncm",
        &build_ncm(&plain, b"0123456789abcdef", META_MULTI, Some(cover)),
    );
    let (kind, d, out) = run(&p, None).unwrap();
    assert_eq!(kind, SourceKind::Ncm);
    assert_eq!(out, plain);
    assert_eq!(d.container, Container::Mp3);
    let m = d.meta;
    assert_eq!(m.platform, Platform::Netease);
    assert_eq!(m.title.as_deref(), Some("雪乡游梦记（伴奏）"));
    assert_eq!(m.artists, ["著小生zoki", "徵羽摩柯", "乐正龙牙", "Hanser"]);
    assert_eq!(m.album.as_deref(), Some("测试专辑"));
    assert_eq!(m.song_id, Some(SongId::Netease(1352449924)));
    assert_eq!(m.cover.as_deref(), Some(&cover[..]));
}

#[test]
fn ncm_numeric_music_id_and_no_cover() {
    let t = Tmp::new("ncm2");
    let plain = audio(b"fLaC", 5000);
    let meta = r#"{"musicId":42,"musicName":"x","artist":[["a",1]],"format":"flac"}"#;
    let p = t.write("b.ncm", &build_ncm(&plain, b"k3y", meta, None));
    let (_, d, out) = run(&p, None).unwrap();
    assert_eq!(out, plain);
    assert_eq!(d.container, Container::Flac);
    assert_eq!(d.meta.song_id, Some(SongId::Netease(42)));
    assert_eq!(d.meta.cover, None);
}

#[test]
fn ncm_truncated_header_is_incomplete() {
    let t = Tmp::new("ncm3");
    let full = build_ncm(&mp3(1000), b"0123456789abcdef", META_MULTI, None);
    let p = t.write("c.ncm", &full[..60]);
    assert!(matches!(sniff(&p), Err(Error::Incomplete)));
}

// ---------- QMC v1 / Plain ----------

fn enc(c: &crate::qmc::Cipher, plain: &[u8]) -> Vec<u8> {
    let mut v = plain.to_vec();
    c.xor(&mut v, 0); // 流密码，加密即解密
    v
}

fn qq_empty() -> EmbeddedMeta {
    EmbeddedMeta {
        title: None,
        artists: vec![],
        album: None,
        cover: None,
        song_id: None,
        platform: Platform::Qq,
    }
}

#[test]
fn static_key_known_answer() {
    let mask = |off: u64| {
        let mut b = [0u8; 4];
        crate::qmc::Cipher::static_v1().xor(&mut b, off);
        b
    };
    assert_eq!(mask(0), [0xC3, 0x4A, 0xD6, 0xCA]);
    assert_eq!(mask(0x7FFF)[0], 0x4A); // key[127]，0x7FFF 本身不回绕
    assert_eq!(mask(0x8000), mask(1)); // 超过 0x7FFF 才按 % 0x7FFF 回绕
}

#[test]
fn qmc_v1_decrypts_across_boundary() {
    let t = Tmp::new("v1");
    let plain = audio(b"fLaC", 200_000);
    let p = t.write("a.bkcflac", &enc(&crate::qmc::Cipher::static_v1(), &plain));
    let (kind, d, out) = run(&p, None).unwrap();
    assert_eq!(kind, SourceKind::QmcV1);
    assert!(out == plain);
    assert_eq!(d.container, Container::Flac);
    assert_eq!(d.meta, qq_empty());
}

#[test]
fn renamed_v1_without_footer_is_v1() {
    let t = Tmp::new("v1r");
    let plain = audio(b"OggS", 50_000);
    let p = t.write("a.qmcogg", &enc(&crate::qmc::Cipher::static_v1(), &plain));
    let (kind, d, out) = run(&p, None).unwrap();
    assert_eq!(kind, SourceKind::QmcV1);
    assert!(out == plain);
    assert_eq!(d.container, Container::Ogg);
}

#[test]
fn plain_audio_with_encrypted_extension() {
    let t = Tmp::new("plain");
    let plain = audio(b"fLaC", 50_000);
    let p = t.write("a.mflac", &plain);
    let (kind, d, out) = run(&p, None).unwrap();
    assert_eq!(kind, SourceKind::Plain(Container::Flac));
    assert!(out == plain);
    assert_eq!(d.container, Container::Flac);
}

// ---------- QMC v2（QTag / PcV1Legacy，Map / RC4，ekey v1 / EncV2）----------

const SIMPLE_KEY: [u8; 8] = [0x69, 0x56, 0x46, 0x38, 0x2b, 0x20, 0x15, 0x0b];
const ENC_V2_KEY1: &[u8; 16] = b"386ZJY!@#*$%^&)(";
const ENC_V2_KEY2: &[u8; 16] = b"**#!(#$%&^a1cZ,T";

/// Tencent TEA tweaked-CBC 加密（builders.py `tea_cbc_encrypt`，盐全 0）。
fn tea_cbc_encrypt(plain: &[u8], key: &[u8; 16]) -> Vec<u8> {
    let k: [u32; 4] =
        std::array::from_fn(|i| u32::from_be_bytes(key[i * 4..i * 4 + 4].try_into().unwrap()));
    let mix = |v: u32, s: u32, a: u32, b: u32| {
        (v << 4).wrapping_add(a) ^ (v >> 5).wrapping_add(b) ^ s.wrapping_add(v)
    };
    let enc_block = |blk: u64| {
        let (mut hi, mut lo) = ((blk >> 32) as u32, blk as u32);
        let mut s = 0u32;
        for _ in 0..16 {
            s = s.wrapping_add(0x9E37_79B9);
            hi = hi.wrapping_add(mix(lo, s, k[0], k[1]));
            lo = lo.wrapping_add(mix(hi, s, k[2], k[3]));
        }
        (u64::from(hi) << 32) | u64::from(lo)
    };
    let pad = (8 - ((10 + plain.len()) & 7)) & 7;
    let mut data = vec![0u8; 1 + pad + 2];
    data[0] = pad as u8;
    data.extend(plain);
    data.extend([0u8; 7]);
    let (mut iv1, mut iv2) = (0u64, 0u64);
    let mut out = Vec::with_capacity(data.len());
    for ch in data.chunks(8) {
        let b = u64::from_be_bytes(ch.try_into().unwrap());
        let next = b ^ iv1;
        let c = enc_block(next) ^ iv2;
        iv1 = c;
        iv2 = next;
        out.extend(c.to_be_bytes());
    }
    out
}

fn make_ekey_v1(master: &[u8]) -> String {
    let mut tk = [0u8; 16];
    for i in 0..8 {
        tk[i * 2] = SIMPLE_KEY[i];
        tk[i * 2 + 1] = master[i];
    }
    b64(&[&master[..8], &tea_cbc_encrypt(&master[8..], &tk)[..]].concat())
}

fn make_ekey_v2(master: &[u8]) -> String {
    let inner = make_ekey_v1(master);
    let layer = tea_cbc_encrypt(&tea_cbc_encrypt(inner.as_bytes(), ENC_V2_KEY2), ENC_V2_KEY1);
    b64(&[&b"QQMusic EncV2,Key:"[..], &layer].concat())
}

fn qtag(enc: &[u8], ekey: &str) -> Vec<u8> {
    let csv = format!("{ekey},12345,2");
    [
        enc,
        csv.as_bytes(),
        &(csv.len() as u32).to_be_bytes(),
        b"QTag",
    ]
    .concat()
}

fn pc_legacy(enc: &[u8], ekey: &str) -> Vec<u8> {
    [enc, ekey.as_bytes(), &(ekey.len() as u32).to_le_bytes()].concat()
}

fn qmc2(master: &[u8], plain: &[u8]) -> Vec<u8> {
    enc(&crate::qmc::Cipher::new(master), plain)
}

#[test]
fn map_known_answer() {
    // qmunlock qmc2.rs map_matches_qmstreamencrypt_mapl
    let mut v = [0u8; 16];
    crate::qmc::Cipher::new(b"ABCDEFGHIJKLMNOP").xor(&mut v, 0);
    assert_eq!(
        v,
        [
            0x3f, 0x8a, 0xc1, 0x49, 0x3f, 0x49, 0xc1, 0x8a, 0x3f, 0x8a, 0xc1, 0x49, 0x3f, 0x49,
            0xc1, 0x8a
        ]
    );
}

#[test]
fn rc4_known_answer() {
    // 参考 Python Rc4Stream 对 512 字节密钥输出的密钥流
    let key: Vec<u8> = (0..512).map(|i| ((i * 37 + 11) & 0xff) as u8).collect();
    let mut ks = vec![0u8; 0x3000];
    crate::qmc::Cipher::new(&key).xor(&mut ks, 0);
    let want = [
        (0, 237),
        (1, 228),
        (127, 255),
        (128, 248),
        (5119, 195),
        (5120, 12),
        (5121, 171),
        (10240, 39),
        (12000, 123),
    ];
    for (o, b) in want {
        assert_eq!(ks[o], b, "offset {o}");
    }
    // 从中途偏移开始解密，结果与整段一致
    let mut tail = vec![0u8; 0x3000 - 5000];
    crate::qmc::Cipher::new(&key).xor(&mut tail, 5000);
    assert!(tail == ks[5000..]);
}

#[test]
fn python_generated_ekeys_decrypt() {
    let master: Vec<u8> = (0..16).collect();
    let v1 = "AAECAwQFBgeBzTnbw+nPorrHOcngVrnWGcZGCK4Pm2Q=";
    let v2 = "UVFNdXNpYyBFbmNWMixLZXk6cK1WMq9P7V5Ywyhh4Hmh14562uFpZK9n6q8ZFrWorXb7SKCsiuR9UbU0Gaeo3sCDMSeFvr2yJi/TzhdagaBUVnqmYNiNfQbM";
    assert_eq!(make_ekey_v1(&master), v1);
    let t = Tmp::new("pyekey");
    let plain = mp3(10_000);
    for (i, ek) in [v1, v2].into_iter().enumerate() {
        let p = t.write(&format!("{i}.mflac"), &qtag(&qmc2(&master, &plain), ek));
        let (kind, _, out) = run(&p, None).unwrap();
        assert_eq!(kind, SourceKind::QmcV2 { ekey: ek.into() });
        assert!(out == plain);
    }
}

#[test]
fn qmc_v2_map_and_rc4_with_both_footers_and_ekey_versions() {
    let t = Tmp::new("v2");
    let map_key: Vec<u8> = (0..256).map(|i| (i * 7 + 1) as u8).collect();
    // 600 字节：n ≠ 512 覆盖段密钥 % n；EncV2 编码后仍在 PcV1Legacy 0x500 长度上限内
    let rc4_key: Vec<u8> = (0..600).map(|i| (i * 13 + 5) as u8).collect();
    let plain = audio(b"fLaC", 300_000);
    let mut n = 0;
    for master in [&map_key, &rc4_key] {
        for ekey in [make_ekey_v1(master), make_ekey_v2(master)] {
            for footer in [qtag, pc_legacy] {
                n += 1;
                let p = t.write(&format!("{n}.mgg"), &footer(&qmc2(master, &plain), &ekey));
                let (kind, d, out) = run(&p, None).unwrap();
                assert_eq!(kind, SourceKind::QmcV2 { ekey: ekey.clone() }, "case {n}");
                assert!(out == plain, "case {n}");
                assert_eq!(d.container, Container::Flac);
                assert_eq!(d.meta, qq_empty());
            }
        }
    }
}

#[test]
fn stag_is_unsupported() {
    let t = Tmp::new("stag");
    let csv = b"123,2,003TestMid";
    let data = [
        &mp3(5000)[..],
        csv,
        &(csv.len() as u32).to_be_bytes(),
        b"STag",
    ]
    .concat();
    let p = t.write("a.mflac", &data);
    match sniff(&p) {
        Err(Error::Unsupported(msg)) => assert!(msg.contains("STag"), "{msg}"),
        other => panic!("{other:?}"),
    }
}

// ---------- musicex（PC 新版，ekey 需外部提供）----------

const MID: &str = "003TestSongMid";
const MEDIA: &str = "F0M0003TestMedia1.mflac";

fn musicex(enc: &[u8]) -> Vec<u8> {
    let utf16 = |s: &str| {
        s.encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<u8>>()
    };
    let mut f = [0u8; 0xC0];
    f[0..12].copy_from_slice(&[0xAA; 12]);
    let m = utf16(MID);
    f[0x0C..0x0C + m.len()].copy_from_slice(&m);
    let n = utf16(MEDIA);
    f[0x48..0x48 + n.len()].copy_from_slice(&n);
    f[0xB0..0xB4].copy_from_slice(&0xC0u32.to_le_bytes());
    f[0xB4..0xB8].copy_from_slice(&1u32.to_le_bytes());
    f[0xB8..].copy_from_slice(b"musicex\0");
    [enc, &f[..]].concat()
}

fn musicex_kind() -> SourceKind {
    SourceKind::MusicEx {
        song_mid: MID.into(),
        media_filename: MEDIA.into(),
    }
}

#[test]
fn musicex_with_ekey_decrypts() {
    let t = Tmp::new("mx");
    let master: Vec<u8> = (0..704).map(|i| (i * 31 + 7) as u8).collect();
    let ekey = make_ekey_v2(&master);
    let plain = audio(b"fLaC", 200_000);
    let p = t.write("a.mflac", &musicex(&qmc2(&master, &plain)));
    let (kind, d, out) = run(&p, Some(&ekey)).unwrap();
    assert_eq!(kind, musicex_kind());
    assert!(out == plain);
    assert_eq!(d.container, Container::Flac);
    assert_eq!(
        d.meta,
        EmbeddedMeta {
            song_id: Some(SongId::QqMid(MID.into())),
            ..qq_empty()
        }
    );
}

#[test]
fn musicex_without_or_with_wrong_ekey() {
    let t = Tmp::new("mx2");
    let master: Vec<u8> = (0..256).map(|i| (i * 3 + 1) as u8).collect();
    let other: Vec<u8> = (0..256).map(|i| (i * 5 + 2) as u8).collect();
    let p = t.write("a.mgg", &musicex(&qmc2(&master, &audio(b"OggS", 50_000))));
    let kind = sniff(&p).unwrap();
    let mut out = Vec::new();
    assert!(matches!(
        decrypt(&p, &kind, None, &mut out),
        Err(Error::NeedsEkey)
    ));
    for bad in [make_ekey_v1(&other), "!!not base64!!".into(), b64(b"short")] {
        let r = decrypt(&p, &kind, Some(&bad), &mut out);
        assert!(matches!(r, Err(Error::BadEkey)), "{bad}: {r:?}");
    }
    assert!(out.is_empty(), "失败时不应写出任何字节");
}

#[test]
fn truncated_musicex_is_incomplete() {
    let t = Tmp::new("mx3");
    let master: Vec<u8> = (0..256).map(|i| (i * 3 + 1) as u8).collect();
    let full = musicex(&qmc2(&master, &audio(b"fLaC", 50_000)));
    let p = t.write("a.mflac", &full[..30_000]);
    assert!(matches!(sniff(&p), Err(Error::Incomplete)));
    let ekey = make_ekey_v1(&master);
    let r = decrypt(&p, &musicex_kind(), Some(&ekey), &mut Vec::new());
    assert!(matches!(r, Err(Error::Incomplete)), "{r:?}");
    let empty = t.write("b.mflac", b"");
    assert!(matches!(sniff(&empty), Err(Error::Incomplete)));
}

// ---------- 审查回归：下载中的前缀、非法长度、任意输入不 panic ----------

fn is_incomplete<T: std::fmt::Debug>(r: Result<T, Error>) -> bool {
    matches!(r, Err(Error::Incomplete))
}

fn musicex_trailer(size: u32) -> Vec<u8> {
    [&size.to_le_bytes()[..], &1u32.to_le_bytes(), b"musicex\0"].concat()
}

fn qtag_trailer(n: u32) -> Vec<u8> {
    [&n.to_be_bytes()[..], b"QTag"].concat()
}

#[test]
fn short_musicex_tail_is_incomplete() {
    let t = Tmp::new("r1");
    for data in [&b"musicex\0"[..], b"1234musicex\0"] {
        let p = t.write("a.mflac", data);
        assert!(is_incomplete(sniff(&p)), "{data:?}");
    }
}

#[test]
fn arbitrary_tails_never_panic() {
    let t = Tmp::new("r1b");
    let rnd = audio(b"", 5000);
    let mut suffixes: Vec<Vec<u8>> = vec![
        vec![],
        b"musicex\0".to_vec(),
        b"QTag".to_vec(),
        b"STag".to_vec(),
    ];
    for v in [0u32, 1, 4, 5, 0xBF, 0xC0, 0x1000, 0x20_0000, u32::MAX] {
        suffixes.push(musicex_trailer(v));
        suffixes.push(qtag_trailer(v));
        suffixes.push(v.to_le_bytes().to_vec());
    }
    let kinds = [
        SourceKind::Ncm,
        SourceKind::QmcV1,
        SourceKind::QmcV2 {
            ekey: "AAAAAAAAAAAA".into(),
        },
        musicex_kind(),
        SourceKind::Plain(Container::Mp3),
    ];
    for ext in ["mflac", "bkcflac", "ncm"] {
        for suf in &suffixes {
            for n in (0..24).chain([100, 0xC0, 1000, 4095, 5000]) {
                let p = t.write(&format!("x.{ext}"), &[&rnd[..n], suf].concat());
                let _ = sniff(&p);
                for k in &kinds {
                    let _ = decrypt(&p, k, Some("AAAAAAAAAAAA"), &mut std::io::sink());
                }
            }
        }
    }
}

#[test]
fn ncm_magic_prefix_is_incomplete() {
    let t = Tmp::new("r2");
    for data in [&b""[..], b"C", b"CTENFDA", b"CTENFDAM", b"CTENFDAM\0\0\x80"] {
        let p = t.write("a.ncm", data);
        assert!(is_incomplete(sniff(&p)), "{data:?}");
    }
    for data in [
        &b"X"[..],
        b"CTENFDAX",
        b"ID3\x03\x00\x00\x00\x00\x00\x00 plain mp3",
    ] {
        let p = t.write("b.ncm", data);
        assert!(matches!(sniff(&p), Err(Error::Corrupt(_))), "{data:?}");
    }
}

#[test]
fn tiny_audio_prefix_is_incomplete_on_every_path() {
    let t = Tmp::new("r3");
    let plain = audio(b"fLaC", 50_000);
    let master: Vec<u8> = (0..256).map(|i| (i * 3 + 1) as u8).collect();
    let ekey = make_ekey_v1(&master);
    let v1 = enc(&crate::qmc::Cipher::static_v1(), &plain);
    for n in [1, 5, 11] {
        let p = t.write("a.bkcflac", &v1[..n]);
        assert!(is_incomplete(sniff(&p)), "v1 sniff {n}");
        assert!(
            is_incomplete(decrypt(&p, &SourceKind::QmcV1, None, &mut Vec::new())),
            "v1 {n}"
        );
        let p = t.write("a.mflac", &plain[..n]);
        assert!(is_incomplete(sniff(&p)), "plain sniff {n}");
        let r = decrypt(
            &p,
            &SourceKind::Plain(Container::Flac),
            None,
            &mut Vec::new(),
        );
        assert!(is_incomplete(r), "plain {n}");
        let p = t.write("a.ncm", &build_ncm(&plain[..n], b"k3y", META_MULTI, None));
        assert!(
            is_incomplete(decrypt(&p, &SourceKind::Ncm, None, &mut Vec::new())),
            "ncm {n}"
        );
        let p = t.write("b.mflac", &qtag(&qmc2(&master, &plain[..n]), &ekey));
        let kind = sniff(&p).unwrap();
        assert!(
            is_incomplete(decrypt(&p, &kind, None, &mut Vec::new())),
            "qtag {n}"
        );
        let p = t.write("c.mflac", &musicex(&qmc2(&master, &plain[..n])));
        let r = decrypt(&p, &musicex_kind(), Some(&ekey), &mut Vec::new());
        assert!(is_incomplete(r), "musicex {n}");
    }
}

#[test]
fn ncm_oversized_sections_are_corrupt() {
    let t = Tmp::new("r4");
    let u32le = |d: &[u8], p: usize| u32::from_le_bytes(d[p..p + 4].try_into().unwrap()) as usize;
    let ok = build_ncm(&mp3(5000), b"0123456789abcdef", META_MULTI, Some(b"cover"));
    let key_at = 10;
    let meta_at = 14 + u32le(&ok, key_at);
    let space_at = meta_at + 4 + u32le(&ok, meta_at) + 5;
    let patch = |at: usize, vals: &[u32]| {
        let mut d = ok.clone();
        for (i, v) in vals.iter().enumerate() {
            d[at + i * 4..at + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        d
    };
    for (what, data) in [
        ("key", patch(key_at, &[4097])),
        ("key huge", patch(key_at, &[0xFFFF_FFF0])),
        ("meta", patch(meta_at, &[(1 << 20) + 1])),
        ("cover", patch(space_at, &[(32 << 20) + 1, (32 << 20) + 1])),
    ] {
        let p = t.write("a.ncm", &data);
        assert!(
            matches!(sniff(&p), Err(Error::Corrupt(_))),
            "{what}: {:?}",
            sniff(&p)
        );
    }
}

#[test]
fn ncm_unrecognized_audio_is_corrupt_without_output() {
    let t = Tmp::new("r5");
    let p = t.write(
        "a.ncm",
        &build_ncm(&audio(b"", 5000), b"k3y", META_MULTI, None),
    );
    let mut out = Vec::new();
    let r = decrypt(&p, &SourceKind::Ncm, None, &mut out);
    assert!(matches!(r, Err(Error::Corrupt(_))), "{r:?}");
    assert!(out.is_empty());
}

#[test]
fn mp3_sniff_validates_frame_header() {
    let t = Tmp::new("r7");
    let p = t.write("a.mflac", &audio(&[0xFF, 0xFB, 0x90, 0x64], 5000));
    assert_eq!(sniff(&p).unwrap(), SourceKind::Plain(Container::Mp3));
    let master: Vec<u8> = (0..256).map(|i| (i * 3 + 1) as u8).collect();
    for head in [
        [0xFF, 0xE9, 0x90, 0x64],
        [0xFF, 0xF9, 0x90, 0x64],
        [0xFF, 0xFB, 0xF0, 0x64],
        [0xFF, 0xFB, 0x9C, 0x64],
    ] {
        let p = t.write("b.mflac", &musicex(&qmc2(&master, &audio(&head, 5000))));
        let r = decrypt(
            &p,
            &musicex_kind(),
            Some(&make_ekey_v1(&master)),
            &mut Vec::new(),
        );
        assert!(matches!(r, Err(Error::BadEkey)), "{head:02x?}: {r:?}");
    }
}

#[test]
fn qtag_longer_than_4k_is_read_fully() {
    let t = Tmp::new("r8");
    let master: Vec<u8> = (0..4096).map(|i| (i * 13 + 5) as u8).collect();
    let ekey = make_ekey_v1(&master);
    assert!(ekey.len() > 5000);
    let plain = mp3(100_000);
    let p = t.write("a.mgg", &qtag(&qmc2(&master, &plain), &ekey));
    let (kind, _, out) = run(&p, None).unwrap();
    assert_eq!(kind, SourceKind::QmcV2 { ekey });
    assert!(out == plain);
}

#[test]
fn footer_lengths_beyond_data_are_incomplete_and_illegal_are_corrupt() {
    let t = Tmp::new("r9");
    let body = mp3(3000);
    let cases: [(&str, Vec<u8>, bool); 6] = [
        (
            "qtag > 数据",
            [&body[..], &qtag_trailer(100_000)].concat(),
            true,
        ),
        ("qtag < 最小", [&body[..], &qtag_trailer(3)].concat(), false),
        (
            "qtag > 上限",
            [&body[..], &qtag_trailer(0x7FFF_FFFF)].concat(),
            false,
        ),
        (
            "musicex > 数据",
            [&body[..], &musicex_trailer(100_000)].concat(),
            true,
        ),
        (
            "musicex < 最小",
            [&body[..], &musicex_trailer(0x10)].concat(),
            false,
        ),
        (
            "musicex > 上限",
            [&body[..], &musicex_trailer(0x7FFF_FFFF)].concat(),
            false,
        ),
    ];
    for (what, data, incomplete) in cases {
        let p = t.write("a.mflac", &data);
        let r = sniff(&p);
        if incomplete {
            assert!(is_incomplete(r), "{what}");
        } else {
            assert!(matches!(r, Err(Error::Corrupt(_))), "{what}: {r:?}");
        }
    }
}
