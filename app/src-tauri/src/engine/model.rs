//! 前端契约：`Snapshot` 及其组成部分（camelCase JSON）。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fmt {
    #[default]
    Original,
    Mp3,
}

impl Fmt {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "original" => Ok(Fmt::Original),
            "mp3" => Ok(Fmt::Mp3),
            _ => Err(format!("未知的输出格式：{s}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub onboarded: bool,
    pub suggestions: Vec<Suggestion>,
    pub folders: Vec<Folder>,
    pub output_dir: String,
    pub format: Fmt,
    pub qq: Qq,
    pub jobs: Vec<Job>,
    pub notices: Vec<Notice>,
    pub busy: u32,
    pub waiting_login: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Suggestion {
    pub path: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Folder {
    pub path: String,
    pub name: String,
    pub accessible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Qq {
    pub state: QqState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum QqState {
    LoggedIn,
    NotInstalled,
    LoggedOut,
    NeedsAuth,
    Expired,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Platform {
    Qq,
    Netease,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum JobState {
    Downloading,
    Queued,
    Decrypt,
    Transcode,
    Metadata,
    Done,
    Skipped,
    WaitLogin,
    Failed,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub source: String,
    pub file_name: String,
    pub platform: Platform,
    pub state: JobState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_pos: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub artists: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    pub has_cover: bool,
    pub has_lyrics: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renamed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub playing: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum NoticeKind {
    ExistingSongs,
    CaughtUp,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Notice {
    pub id: String,
    pub kind: NoticeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AddFolderResult {
    pub existing: u32,
}
