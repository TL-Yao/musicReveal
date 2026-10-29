// 三份后台状态（快照 / 资料库 / 播放器）与后台接缝，各界面模块共享。
import type { Backend } from "./backend";
import type { Library, PlayerState, Snapshot } from "./types";

export const S = {
  B: undefined as unknown as Backend,
  snap: undefined as Snapshot | undefined,
  lib: { dir: "", tracks: [], scanning: true } as Library,
  player: { playing: false, positionMs: 0, durationMs: 0, volume: 0.7, shuffle: false, repeat: "off", queueRev: 0 } as PlayerState,
  /** 收到 player 状态时的 performance.now()，用来插值进度 */
  playerAt: 0,
};
export const call = (cmd: string, args?: Record<string, unknown>) => S.B.invoke(cmd, args).catch((e) => console.error(cmd, e));
