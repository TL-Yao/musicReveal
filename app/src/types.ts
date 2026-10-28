// 后台 `snapshot` 事件与 `get_snapshot` 的载荷（camelCase）。与 app/src-tauri/src/engine/model.rs 保持一致。

export type Snapshot = {
  onboarded: boolean;
  /** accessible=false 时 reason 给出原因（不存在 / 无权限 / 条目超过 5000） */
  folders: { path: string; name: string; accessible: boolean; reason?: string }[];
  outputDir: string;
  format: "original" | "mp3";
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
  platform: "qq" | "netease";
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
  /** 输出文件绝对路径；done 与 skipped 有 */
  output?: string;
  /** 正在试听；播放自然结束或 stop_preview 后消失 */
  playing?: boolean;
};

export type AddFolderResult = { existing: number };
