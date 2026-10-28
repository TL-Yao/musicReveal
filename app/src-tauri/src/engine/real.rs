//! 真实转换器：reveal-core 的 `convert` + 在线元数据补全，凭据每次现读。

use super::Converter;
use reveal_core::{
    CancelToken, ConvertError, ConvertOptions, ConvertOutcome, OnlineEnricher, QqStatus, Stage,
};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const PLISTS: [&str; 2] = [
    "Library/Preferences/com.tencent.QQMusicMac.plist",
    "Library/Containers/com.tencent.QQMusicMac/Data/Library/Preferences/com.tencent.QQMusicMac.plist",
];

pub struct RealConverter {
    home: PathBuf,
    enricher: OnlineEnricher,
    ffmpeg: Option<PathBuf>,
}

impl RealConverter {
    pub fn new(home: PathBuf) -> Self {
        RealConverter {
            home,
            enricher: OnlineEnricher::new(),
            ffmpeg: reveal_core::find_ffmpeg(),
        }
    }
}

#[async_trait::async_trait]
impl Converter for RealConverter {
    async fn convert(
        &self,
        input: &Path,
        opts: &ConvertOptions,
        cancel: &CancelToken,
        progress: &mut (dyn FnMut(Stage) + Send),
    ) -> Result<ConvertOutcome, ConvertError> {
        let creds = reveal_core::load_credentials(&self.home).ok();
        let mut opts = opts.clone();
        opts.ffmpeg = opts.ffmpeg.or_else(|| self.ffmpeg.clone());
        reveal_core::convert(
            input,
            &opts,
            &self.enricher,
            creds.as_ref(),
            cancel,
            progress,
        )
        .await
    }

    fn qq_status(&self) -> QqStatus {
        reveal_core::credentials_status(&self.home)
    }

    fn qq_stamp(&self) -> Option<SystemTime> {
        PLISTS
            .iter()
            .filter_map(|p| std::fs::metadata(self.home.join(p)).ok()?.modified().ok())
            .max()
    }
}
