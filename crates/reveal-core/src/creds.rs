//! QQ 音乐 macOS 登录凭据读取与在线取 ekey。凭据值不得进入任何日志或错误消息。

use plist::Value;
use std::fs::File;
use std::io::{Cursor, ErrorKind};
use std::path::Path;

#[derive(Clone)]
pub struct Credentials {
    pub uin: String,
    pub authst: String,
    pub login_type: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("uin", &mask(&self.uin))
            .field("authst", &"<redacted>")
            .field("login_type", &self.login_type)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum QqStatus {
    LoggedIn { masked_uin: String },
    NotInstalled,
    LoggedOut,
    NeedsAuth,
    Expired,
}

/// 保留前 2 位和后 2 位，如 `12****89`。
fn mask(uin: &str) -> String {
    let c: Vec<char> = uin.chars().collect();
    if c.len() < 5 {
        return "****".into();
    }
    let head: String = c[..2].iter().collect();
    let tail: String = c[c.len() - 2..].iter().collect();
    format!("{head}****{tail}")
}

/// 只会返回 LoggedIn / NotInstalled / LoggedOut / NeedsAuth。
pub fn credentials_status(home: &Path) -> QqStatus {
    match load_credentials(home) {
        Ok(c) => QqStatus::LoggedIn {
            masked_uin: mask(&c.uin),
        },
        Err(s) => s,
    }
}

pub fn load_credentials(home: &Path) -> Result<Credentials, QqStatus> {
    load_in(home, Path::new("/Applications"))
}

const PLIST: &str = "Library/Preferences/com.tencent.QQMusicMac.plist";
const CONTAINER: &str = "Library/Containers/com.tencent.QQMusicMac";

/// 先用非沙盒 plist；没有登录信息时才看旧沙盒容器，容器进不去就是 NeedsAuth。
/// `system_apps` 是系统级 Applications 目录，测试时可替换。
pub(crate) fn load_in(home: &Path, system_apps: &Path) -> Result<Credentials, QqStatus> {
    if let Some(c) = File::open(home.join(PLIST)).ok().and_then(parse) {
        return Ok(c);
    }
    let container = home.join(CONTAINER);
    match std::fs::metadata(&container) {
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::PermissionDenied => return Err(QqStatus::NeedsAuth),
        Err(_) if home.join(PLIST).exists()
            || system_apps.join("QQMusic.app").exists()
            || home.join("Applications/QQMusic.app").exists() =>
        {
            return Err(QqStatus::LoggedOut)
        }
        Err(_) => return Err(QqStatus::NotInstalled),
    }
    match File::open(container.join("Data").join(PLIST)) {
        Ok(f) => parse(f).ok_or(QqStatus::LoggedOut),
        Err(e) if e.kind() == ErrorKind::PermissionDenied => Err(QqStatus::NeedsAuth),
        Err(_) => Err(QqStatus::LoggedOut),
    }
}

fn parse(f: File) -> Option<Credentials> {
    let outer = Value::from_reader(f).ok()?;
    let archived = outer.as_dictionary()?.get("AutoLoginUserInfo")?.as_data()?;
    let inner = Value::from_reader(Cursor::new(archived)).ok()?;
    let objects = inner.as_dictionary()?.get("$objects")?.as_array()?;
    objects
        .iter()
        .filter_map(Value::as_dictionary)
        .find_map(|d| {
            let get = |k: &str| resolve(d.get(k)?, objects).filter(|s| !s.is_empty());
            Some(Credentials {
                authst: get("strAuthst")?,
                uin: get("nUserId").or_else(|| get("strUserAccount"))?,
                login_type: get("loginType").unwrap_or_else(|| "3".into()),
            })
        })
}

fn resolve(v: &Value, objects: &[Value]) -> Option<String> {
    match v {
        Value::Uid(u) => resolve(objects.get(u.get() as usize)?, objects),
        Value::String(s) => Some(s.clone()),
        Value::Integer(i) => Some(i.to_string()),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EkeyError {
    /// 服务器明确不给（凭据过期 / 无权限）。
    Rejected,
    Network(String),
}

/// 协议同 qmunlock `core/ekey.rs`；错误消息只含状态码，不含凭据。
pub async fn fetch_ekey(
    song_mid: &str,
    media_filename: &str,
    creds: &Credentials,
) -> Result<String, EkeyError> {
    let body = serde_json::json!({
        "comm": { "authst": creds.authst, "ct": "19", "cv": "1859", "uin": creds.uin, "tmeLoginType": creds.login_type },
        "req_1": { "module": "music.vkey.GetEVkey", "method": "CgiGetEVkey", "param": {
            "filename": [media_filename], "guid": "10000", "songmid": [song_mid], "songtype": [1],
            "uin": creds.uin, "loginflag": 1, "platform": "20", "ctx": 1 } }
    });
    let net = |e: reqwest::Error| EkeyError::Network(e.without_url().to_string());
    let resp: serde_json::Value = reqwest::Client::new()
        .post("https://u.y.qq.com/cgi-bin/musicu.fcg")
        .header("User-Agent", "QQMusic/20")
        .json(&body)
        .send()
        .await
        .map_err(net)?
        .error_for_status()
        .map_err(net)?
        .json()
        .await
        .map_err(net)?;
    resp.pointer("/req_1/data/midurlinfo/0/ekey")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or(EkeyError::Rejected)
}
