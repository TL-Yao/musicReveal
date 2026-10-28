//! 按文件内歌曲 ID 在线补全元数据与歌词。解析与网络分开：解析函数只吃响应字节。

use crate::TrackMeta;
use serde_json::Value;

fn json(body: &[u8]) -> Result<Value, String> {
    serde_json::from_slice(body).map_err(|e| format!("响应不是 JSON：{e}"))
}

fn str_at<'a>(v: &'a Value, ptr: &str) -> Option<&'a str> {
    v.pointer(ptr)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn names(v: &Value, ptr: &str) -> Vec<String> {
    v.pointer(ptr)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|a| str_at(a, "/name"))
        .map(str::to_owned)
        .collect()
}

fn set(slot: &mut Option<String>, v: Option<&str>) {
    if let Some(v) = v {
        *slot = Some(v.to_owned());
    }
}

fn set_list(meta: &mut TrackMeta, artists: Vec<String>, album_artists: Vec<String>) {
    if !artists.is_empty() {
        meta.artists = artists;
    }
    if !album_artists.is_empty() {
        meta.album_artist = Some(album_artists.join(", "));
    }
}

fn pos(v: &Value, ptr: &str) -> Option<u32> {
    v.pointer(ptr)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|&n| n > 0)
}

/// QQ `fcg_play_single_song` 响应 → meta；返回专辑 MID（取封面用）。
pub(crate) fn qq_song(body: &[u8], meta: &mut TrackMeta) -> Result<Option<String>, String> {
    let v = json(body)?;
    let s = v.pointer("/data/0").ok_or("QQ 曲库没有这首歌")?;
    set(&mut meta.title, str_at(s, "/title").or(str_at(s, "/name")));
    set(&mut meta.album, str_at(s, "/album/name"));
    set_list(meta, names(s, "/singer"), names(s, "/album/singer"));
    if let Some(y) = str_at(s, "/time_public")
        .or(str_at(s, "/album/time_public"))
        .and_then(|t| t.get(..4)?.parse().ok())
    {
        meta.year = Some(y);
    }
    if let Some(n) = pos(s, "/index_album") {
        meta.track = Some(n);
    }
    if let Some(n) = s
        .get("index_cd")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n + 1).ok())
    {
        meta.disc = Some(n);
    }
    Ok(str_at(s, "/album/mid").map(str::to_owned))
}

/// `&#58;` / `&#x3A;` / 常见命名实体；认不出的原样保留。
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out += &rest[..i];
        rest = &rest[i..];
        let decoded = rest
            .bytes()
            .skip(1)
            .take(11)
            .position(|b| b == b';')
            .and_then(|end| {
                let name = &rest[1..end + 1];
                let c = match name {
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "quot" => '"',
                    "apos" => '\'',
                    "nbsp" => ' ',
                    _ => {
                        let n = name.strip_prefix('#')?;
                        let code = match n.strip_prefix(['x', 'X']) {
                            Some(h) => u32::from_str_radix(h, 16).ok()?,
                            None => n.parse().ok()?,
                        };
                        char::from_u32(code)?
                    }
                };
                Some((c, end + 2))
            });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out + rest
}

const CREDITS: &[&str] = &[
    "作词",
    "作曲",
    "编曲",
    "词",
    "曲",
    "混音",
    "缩混",
    "母带",
    "录音",
    "制作人",
    "制作",
    "监制",
    "出品",
    "发行",
    "和声",
    "和音",
    "吉他",
    "贝斯",
    "鼓",
    "弦乐",
    "绘",
    "画师",
    "美工",
    "视频",
    "策划",
    "统筹",
    "演唱",
    "原唱",
    "调教",
    "调校",
    "后期",
    "作詞",
    "編曲",
    "Lyrics",
    "Composer",
    "Arranger",
    "Producer",
];

/// `作曲 Yunomi`、`作词 : 无` 这类署名行。
fn is_credit(text: &str) -> bool {
    CREDITS.iter().any(|k| {
        text.strip_prefix(k)
            .is_some_and(|r| r.starts_with(|c: char| c.is_whitespace() || c == ':' || c == '：'))
    })
}

/// 有带时间轴的歌词正文才算有歌词：纯音乐占位、只有署名、无时间轴的文本都视为没有。
fn lyrics(lrc: &str, trans: Option<&str>) -> Option<crate::Lyrics> {
    let body: Vec<&str> = lrc
        .lines()
        .filter_map(|l| {
            let (tags, text) = crate::convert::split_line(l);
            let text = text.trim();
            (!tags.is_empty() && !text.is_empty() && !is_credit(text)).then_some(text)
        })
        .collect();
    if body.is_empty() || (body.len() == 1 && body[0].contains("纯音乐")) {
        return None;
    }
    Some(crate::Lyrics {
        lrc: lrc.trim().to_owned(),
        translated_lrc: trans
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned),
    })
}

/// QQ `fcg_query_lyric_new`（nobase64）响应 → 歌词；没有歌词返回 None。
pub(crate) fn qq_lyric(body: &[u8]) -> Result<Option<crate::Lyrics>, String> {
    let v = json(body)?;
    let lrc = unescape(v.get("lyric").and_then(Value::as_str).unwrap_or(""));
    let trans = v.get("trans").and_then(Value::as_str).map(unescape);
    Ok(lyrics(&lrc, trans.as_deref()))
}

/// 毫秒时间戳按北京时间（UTC+8）取年份；civil-from-days 算法。
fn year_utc8(ms: i64) -> Option<u16> {
    let z = (ms.div_euclid(1000) + 8 * 3600).div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let march_based_month = (5 * doy + 2) / 153;
    u16::try_from(yoe + era * 400 + i64::from(march_based_month >= 10)).ok()
}

/// 网易云 `api/song/detail` 响应 → meta；返回专辑封面 URL。
pub(crate) fn netease_song(body: &[u8], meta: &mut TrackMeta) -> Result<Option<String>, String> {
    let v = json(body)?;
    let s = v.pointer("/songs/0").ok_or("网易云曲库没有这首歌")?;
    set(&mut meta.title, str_at(s, "/name"));
    set(&mut meta.album, str_at(s, "/album/name"));
    set_list(meta, names(s, "/artists"), names(s, "/album/artists"));
    if let Some(y) = s
        .pointer("/album/publishTime")
        .and_then(Value::as_i64)
        .filter(|&t| t > 0)
        .and_then(year_utc8)
    {
        meta.year = Some(y);
    }
    if let Some(n) = pos(s, "/no") {
        meta.track = Some(n);
    }
    if let Some(n) = str_at(s, "/disc")
        .and_then(|d| d.parse::<u32>().ok())
        .filter(|&n| n > 0)
    {
        meta.disc = Some(n);
    }
    Ok(str_at(s, "/album/picUrl").map(str::to_owned))
}

/// 网易云 `api/song/lyric` 响应 → 歌词；`nolyric` / `uncollected` / 纯音乐占位返回 None。
pub(crate) fn netease_lyric(body: &[u8]) -> Result<Option<crate::Lyrics>, String> {
    let v = json(body)?;
    let flag = |k: &str| v.get(k).and_then(Value::as_bool) == Some(true);
    if flag("nolyric") || flag("uncollected") {
        return Ok(None);
    }
    Ok(str_at(&v, "/lrc/lyric").and_then(|l| lyrics(l, str_at(&v, "/tlyric/lyric"))))
}
