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
        .filter(|&y: &u16| y > 0)
    {
        meta.year = Some(y);
    }
    if let Some(n) = pos(s, "/index_album") {
        meta.track = Some(n);
    }
    if let Some(n) = s
        .get("index_cd")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n.checked_add(1)?).ok())
    {
        meta.disc = Some(n);
    }
    Ok(str_at(s, "/album/mid").map(str::to_owned))
}

/// 署名角色：后接冒号（半角 / 全角，前后可有空白）即为署名行。
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

/// 不像正文开头的专用角色词：后接空白也算署名（网易云常见 `作曲 Yunomi`）。
const CREDITS_SPACED: &[&str] = &[
    "作词",
    "作曲",
    "编曲",
    "混音",
    "缩混",
    "母带",
    "制作人",
    "监制",
    "和声",
    "作詞",
    "編曲",
    "绘",
];

/// `作曲 Yunomi`、`作词 : 无`、`鼓：张三` 这类署名行。
fn is_credit(text: &str) -> bool {
    let after = |keys: &'static [&'static str]| keys.iter().filter_map(|k| text.strip_prefix(k));
    after(CREDITS).any(|r| r.trim_start().starts_with([':', '：']))
        || after(CREDITS_SPACED).any(|r| r.starts_with(char::is_whitespace))
}

/// 去掉标点与空白后与已知纯音乐占位文案相同。
fn is_placeholder(text: &str) -> bool {
    let t: String = text.chars().filter(|c| c.is_alphanumeric()).collect();
    [
        "纯音乐请欣赏",
        "纯音乐请您欣赏",
        "此歌曲为没有填词的纯音乐请您欣赏",
    ]
    .contains(&t.as_str())
}

/// 有带时间轴的歌词正文才算有歌词：纯音乐占位、只有署名、无时间轴的文本都视为没有。
fn lyrics(lrc: &str, trans: Option<&str>) -> Option<crate::Lyrics> {
    let has_body = lrc.lines().any(|l| {
        let (tags, text) = crate::convert::split_line(l);
        let text = text.trim();
        !tags.is_empty() && !text.is_empty() && !is_credit(text) && !is_placeholder(text)
    });
    if !has_body {
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

/// 最后一条正文行若是「未经许可…」这类版权声明，删掉它。
fn drop_notice(lrc: &str) -> String {
    let mut lines: Vec<&str> = lrc.lines().collect();
    let last = lines.iter().rposition(|l| {
        let (tags, text) = crate::convert::split_line(l);
        !tags.is_empty() && !text.trim().is_empty()
    });
    if let Some(i) = last {
        let text = crate::convert::split_line(lines[i]).1.trim();
        if text.starts_with("未经许可") || text.starts_with("未经著作权人许可") {
            lines.remove(i);
        }
    }
    lines.join("\n")
}

/// QQ `fcg_query_lyric_new`（nobase64）响应 → 歌词；没有歌词返回 None。
pub(crate) fn qq_lyric(body: &[u8]) -> Result<Option<crate::Lyrics>, String> {
    let v = json(body)?;
    let lrc = drop_notice(&html_escape::decode_html_entities(
        v.get("lyric").and_then(Value::as_str).unwrap_or(""),
    ));
    let trans = v
        .get("trans")
        .and_then(Value::as_str)
        .map(html_escape::decode_html_entities);
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

const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/120.0 Safari/537.36";

/// 一个平台的接口：歌曲信息、歌词的 URL 与解析，以及由歌曲信息里的封面引用生成候选封面 URL。
struct Api {
    referer: &'static str,
    song_url: String,
    lyric_url: String,
    song: fn(&[u8], &mut TrackMeta) -> Result<Option<String>, String>,
    lyric: fn(&[u8]) -> Result<Option<crate::Lyrics>, String>,
    covers: fn(&str) -> Vec<String>,
}

fn api(id: &crate::SongId) -> Api {
    match id {
        crate::SongId::QqMid(mid) => Api {
            referer: "https://y.qq.com/",
            song_url: format!(
                "https://c.y.qq.com/v8/fcg-bin/fcg_play_single_song.fcg?songmid={mid}\
&platform=yqq&format=json&needNewCode=0&new_json=1"
            ),
            lyric_url: format!(
                "https://c.y.qq.com/lyric/fcgi-bin/fcg_query_lyric_new.fcg?songmid={mid}\
&format=json&nobase64=1&g_tk=5381&loginUin=0&hostUin=0&inCharset=utf8&outCharset=utf-8\
&notice=0&platform=yqq.json&needNewCode=0"
            ),
            song: qq_song,
            lyric: qq_lyric,
            covers: |album_mid| {
                let base = "https://y.gtimg.cn/music/photo_new";
                [1200, 800, 500]
                    .iter()
                    .map(|s| format!("{base}/T002R{s}x{s}M000{album_mid}.jpg"))
                    .collect()
            },
        },
        crate::SongId::Netease(id) => Api {
            referer: "https://music.163.com/",
            song_url: format!("https://music.163.com/api/song/detail/?ids=%5B{id}%5D"),
            lyric_url: format!("https://music.163.com/api/song/lyric?id={id}&lv=1&tv=1"),
            song: netease_song,
            lyric: netease_lyric,
            covers: |pic| vec![pic.to_owned(), format!("{pic}?param=1400y1400")],
        },
    }
}

/// 按 `TrackMeta.song_id` 精确查 QQ / 网易云公开接口；不带登录凭据。
pub struct OnlineEnricher {
    http: reqwest::Client,
}

impl Default for OnlineEnricher {
    fn default() -> Self {
        Self::new()
    }
}

impl OnlineEnricher {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent(UA)
            .build()
            .unwrap_or_default();
        Self { http }
    }

    async fn get(&self, url: &str, referer: &str) -> Result<Vec<u8>, String> {
        let r = self
            .http
            .get(url)
            .header(reqwest::header::REFERER, referer)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| e.to_string())?;
        Ok(r.bytes().await.map_err(|e| e.to_string())?.to_vec())
    }

    /// 依次试，取第一个 2xx、像张图（> 1KB）且不超过 8MB 的。
    async fn cover(&self, urls: &[String], referer: &str) -> Option<Vec<u8>> {
        for u in urls {
            if let Ok(b) = self.get(u, referer).await {
                if (1025..=8 << 20).contains(&b.len()) {
                    return Some(b);
                }
            }
        }
        None
    }
}

#[async_trait::async_trait]
impl crate::Enricher for OnlineEnricher {
    async fn enrich(&self, meta: &mut TrackMeta) -> Result<(), String> {
        let Some(api) = meta.song_id.as_ref().map(api) else {
            return Ok(());
        };
        let (song, lyric) = tokio::join!(
            self.get(&api.song_url, api.referer),
            self.get(&api.lyric_url, api.referer)
        );
        let mut errs = vec![];
        match song.and_then(|b| (api.song)(&b, meta)) {
            Ok(Some(r)) => match self.cover(&(api.covers)(&r), api.referer).await {
                Some(c) => meta.cover = Some(pick_cover(meta.cover.take(), c)),
                None => errs.push("封面下载失败".to_owned()),
            },
            Ok(None) => {}
            Err(e) => errs.push(format!("歌曲信息：{e}")),
        }
        match lyric.and_then(|b| (api.lyric)(&b)) {
            Ok(Some(l)) => meta.lyrics = Some(l),
            Ok(None) => {}
            Err(e) => errs.push(format!("歌词：{e}")),
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join("；"))
        }
    }
}

/// PNG / JPEG 头里的像素数；认不出返回 0。
fn pixels(b: &[u8]) -> u64 {
    let be16 = |i: usize| {
        b.get(i..i + 2)
            .map(|x| u64::from(u16::from_be_bytes([x[0], x[1]])))
    };
    let be32 = |i: usize| {
        b.get(i..i + 4)
            .map(|x| u64::from(u32::from_be_bytes([x[0], x[1], x[2], x[3]])))
    };
    if b.starts_with(b"\x89PNG\r\n\x1a\n") && b.get(12..16) == Some(b"IHDR") {
        return be32(16).zip(be32(20)).map_or(0, |(w, h)| w * h);
    }
    if !b.starts_with(&[0xFF, 0xD8]) {
        return 0;
    }
    let mut i = 2;
    while b.get(i) == Some(&0xFF) {
        let Some(&m) = b.get(i + 1) else { return 0 };
        match m {
            0xFF => i += 1,
            0x01 | 0xD0..=0xD7 => i += 2,
            0xC0..=0xCF if !matches!(m, 0xC4 | 0xC8 | 0xCC) => {
                return be16(i + 5).zip(be16(i + 7)).map_or(0, |(h, w)| w * h)
            }
            _ => match be16(i + 2) {
                Some(len) => i += 2 + len as usize,
                None => return 0,
            },
        }
    }
    0
}

/// 文件自带与在线封面都按 ID 精确对应：取像素多的，相等取在线。
pub(crate) fn pick_cover(local: Option<Vec<u8>>, online: Vec<u8>) -> Vec<u8> {
    match local {
        Some(l) if pixels(&l) > pixels(&online) => l,
        _ => online,
    }
}
