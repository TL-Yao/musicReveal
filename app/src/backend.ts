// 界面与后台之间的唯一接缝：真实实现走 Tauri，开发期 ?mock 换成 mock.ts。
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import type { Library, PlayerState, Snapshot } from "./types";

export type DragDrop = { type: "enter" | "over" | "drop" | "leave"; paths?: string[] };

export type Backend = {
  invoke<T = void>(cmd: string, args?: Record<string, unknown>): Promise<T>;
  onSnapshot(cb: (s: Snapshot) => void): void;
  /** 资料库变化时整份推送 */
  onLibrary(cb: (l: Library) => void): void;
  /** 播放状态变化时推送，播放中每秒一次；两次之间界面自行插值进度 */
  onPlayer(cb: (p: PlayerState) => void): void;
  onDragDrop(cb: (e: DragDrop) => void): void;
  /** 点了「编辑 → 撤销」，或焦点在文本框里按的 ⌘Z 落到了菜单上 */
  onMenuUndo(cb: () => void): void;
  /** 「添加文件…」的系统选择器；取消时为 null */
  pickFiles(): Promise<string[] | null>;
};

/** 与 crates/reveal-core 的 is_candidate 一致：ncm + qmc::V1_EXTS + qmc::V2_EXTS，再加普通音频 */
const EXTS = [
  "ncm",
  "tkm", "bkcmp3", "bkcm4a", "bkcflac", "bkcwav", "bkcape", "bkcogg", "bkcwma", "666c6163", "6d7033", "6f6767", "6d3461", "776176",
  "mflac", "mflac0", "mgg", "mgg0", "mgg1", "mggl", "mmp4", "qmcflac", "qmcogg", "qmc0", "qmc2", "qmc3", "qmc4", "qmc6", "qmc8",
  "mp3", "flac", "ogg", "m4a", "wav",
];

export const tauriBackend: Backend = {
  invoke: (cmd, args) => invoke(cmd, args),
  onSnapshot: (cb) => void listen<Snapshot>("snapshot", (e) => cb(e.payload)),
  onLibrary: (cb) => void listen<Library>("library", (e) => cb(e.payload)),
  onPlayer: (cb) => void listen<PlayerState>("player", (e) => cb(e.payload)),
  onDragDrop: (cb) => void getCurrentWebview().onDragDropEvent((e) => cb(e.payload)),
  onMenuUndo: (cb) => void listen("menu-undo", () => cb()),
  pickFiles: async () => {
    const r = await open({ multiple: true, directory: false, filters: [{ name: "音乐", extensions: EXTS }] });
    return r && r.length ? r : null;
  },
};
