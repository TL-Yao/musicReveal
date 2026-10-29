// 后台 `snapshot` 事件与 `get_snapshot` 的载荷（camelCase）。与 app/src-tauri/src/engine/model.rs 保持一致。

export type Snapshot = {
  onboarded: boolean;
  /** accessible=false 时 reason 给出原因（不存在 / 无权限 / 条目超过 5000） */
  folders: { path: string; name: string; accessible: boolean; reason?: string }[];
  outputDir: string;
  format: "original" | "apple";
  /** 切换输出格式引发的已有歌曲批量转换；无批次时省略 */
  reformat?: Reformat;
  qq: { state: "loggedIn" | "notInstalled" | "loggedOut" | "needsAuth" | "expired"; account?: string };
  /** 本次运行的任务，新任务在最上面 */
  jobs: Job[];
  notices: { id: string; kind: "existingSongs" | "caughtUp"; folder?: string; count: number }[];
  /** downloading + queued + 处理中 */
  busy: number;
  waitingLogin: number;
};

export type Job = {
  id: string;
  source: string;
  fileName: string;
  /** other：普通音频，无来源平台，界面不显示来源字样 */
  platform: "qq" | "netease" | "other";
  state: "downloading" | "queued" | "decrypt" | "transcode" | "metadata" | "done" | "skipped" | "waitLogin" | "failed";
  /** 仅 queued 有，从 1 开始 */
  queuePos?: number;
  title?: string;
  artists: string[];
  album?: string;
  /** ≤256px JPEG 缩略图的 asset:// 地址，可直接用于 <img> */
  coverUrl?: string;
  quality?: string;
  hasCover: boolean;
  hasLyrics: boolean;
  /** 输出重名时加的编号 n（文件名为「… (n).ext」） */
  renamed?: number;
  /** 中文简短原因，仅 failed 有 */
  error?: string;
  /** 输出文件绝对路径；done 与 skipped 有。等于资料库里对应 Track 的 id */
  output?: string;
};

export type Reformat = {
  /** 这批是转向哪边 */
  target: "apple" | "original";
  /** 本批总首数 */
  total: number;
  /** 已处理（成功 + 失败） */
  done: number;
  /** 正在转的那首的歌名（标题，不是文件名） */
  current?: string;
  /** 正在转的那首的封面（资料库缩略图 asset 地址），可无 */
  coverUrl?: string;
  /** 剩余秒数估计 */
  etaSecs?: number;
  /** 失败的歌名 + 一句中文原因 */
  failed: { title: string; reason: string }[];
  /** 批次结束（全部处理完） */
  finished: boolean;
};
/** preview_format 的返回：若切到该格式，资料库里需要转换的已有歌曲数与预计耗时 */
export type FormatPreview = { count: number; etaSecs: number };

export type AddFolderResult = { existing: number };

// 资料库与播放器（`library` / `player` 事件，get_library / get_player / get_lyrics 的载荷）
export type Track = {
  /** 绝对路径 */
  id: string;
  title: string;
  artists: string[];
  album?: string;
  albumArtist?: string;
  year?: number;
  track?: number;
  disc?: number;
  durationMs: number;
  /** "FLAC 24bit/48kHz" | "ALAC 24bit/48kHz" | "AAC 256k" | "MP3 320k" */
  quality: string;
  /** 可直接作 img src */
  coverUrl?: string;
  hasLyrics: boolean;
  /** ms */
  addedAt: number;
};
export type Library = { dir: string; tracks: Track[]; scanning: boolean };
export type LyricLine = { ms: number; text: string; trans?: string };
export type Repeat = "off" | "all" | "one";
export type PlayerState = {
  trackId?: string;
  playing: boolean;
  positionMs: number;
  durationMs: number;
  /** 0..1 */
  volume: number;
  shuffle: boolean;
  repeat: Repeat;
  error?: string;
};
