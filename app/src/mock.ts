// 开发期假后台：只在 vite dev + ?mock 时由 main.ts 动态加载，生产构建里不存在。
// window.MR 供截图脚本把界面切到任意状态。
import type { Backend, DragDrop } from "./backend";
import type { Job, Snapshot } from "./types";

const HOME = "/Users/me";
const PROCESSING = ["decrypt", "transcode", "metadata"];
type Extra = { title?: string; artists: string[]; album?: string; quality: string; art: string[]; glyph: string };

let snap: Snapshot;
const extra = new Map<string, Extra>();
const runs = new Map<string, number>();
const subs: ((s: Snapshot) => void)[] = [];
let drag: (e: DragDrop) => void = () => {};
let uid = 1;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

// 用设计稿的配色画一张假封面，代替后台的 asset:// 缩略图
function cover([a, b, c]: string[], glyph: string) {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256"><defs><linearGradient id="g" x1="0" y1="0" x2=".6" y2="1"><stop offset="0" stop-color="${a}"/><stop offset="1" stop-color="${b}"/></linearGradient><radialGradient id="r" cx=".18" cy=".12" r=".9"><stop offset="0" stop-color="${c}"/><stop offset=".58" stop-color="${c}" stop-opacity="0"/></radialGradient></defs><rect width="256" height="256" fill="url(#g)"/><rect width="256" height="256" fill="url(#r)"/><text x="150" y="230" font-size="190" font-weight="700" font-family="PingFang SC, sans-serif" fill="#fff" fill-opacity=".55" text-anchor="middle">${glyph}</text></svg>`;
  return "data:image/svg+xml," + encodeURIComponent(svg);
}
const ART: Record<string, string[]> = {
  pink: ["#5B1B5E", "#FF5C8A", "#FFC9A0"], teal: ["#0F2E4C", "#2CC6B0", "#9FF0E2"], ice: ["#B9D7F0", "#5A6FA8", "#FFFFFF"],
  amber: ["#F2B035", "#D14D1F", "#FFF2B0"], comic: ["#F23B3B", "#2E1B4A", "#FFD36E"], grey: ["#55585F", "#1E2024", "#9AA0AA"],
  violet: ["#2B1E6B", "#8A5BFF", "#FFB7E6"], moss: ["#1E3B2A", "#8BC34A", "#F6FFB3"], rose: ["#3A0E22", "#E04A6B", "#FFD1DC"],
};

type Song = { key: string; dir: string; file: string; qq: boolean; title?: string; artists: string[]; album?: string; quality: string; art: string; glyph: string; lyrics?: boolean; renamed?: number };
const SONGS: Record<string, Song> = {
  erciyuan: { key: "erciyuan", dir: "QQ音乐", file: "hanser-二次元凶.mflac", qq: true, title: "二次元凶", artists: ["Hanser"], album: "二次元凶", quality: "FLAC 24bit/48kHz", art: "pink", glyph: "凶" },
  dl: { key: "dl", dir: "QQ音乐", file: "yousa-星夜鸟语.mflac", qq: true, title: "星夜鸟语", artists: ["泠鸢yousa"], album: "星夜鸟语", quality: "FLAC 24bit/48kHz", art: "violet", glyph: "星" },
  rain: { key: "rain", dir: "网易云音乐", file: "DMYoung - 十一月的雨.ncm", qq: false, title: "十一月的雨", artists: ["DMYoung"], album: "十一月的雨", quality: "MP3 320k", art: "moss", glyph: "雨" },
  jiaozhi: { key: "jiaozhi", dir: "网易云音乐", file: "DMYoung,Hanser,泠鸢yousa - 「交织together」.ncm", qq: false, title: "「交织together」", artists: ["DMYoung", "Hanser", "泠鸢yousa"], album: "交织together", quality: "MP3 320k", art: "teal", glyph: "织", renamed: 2 },
  xianxia: { key: "xianxia", dir: "QQ音乐", file: "hanser-不需要陪伴的闲暇.mflac", qq: true, title: "不需要陪伴的闲暇", artists: ["Hanser"], album: "一抹憨色", quality: "FLAC 16bit/44.1kHz", art: "amber", glyph: "闲" },
  xuexiang: { key: "xuexiang", dir: "网易云音乐", file: "著小生zoki,徵羽摩柯,乐正龙牙,Hanser - 雪乡游梦记（伴奏）.ncm", qq: false, title: "雪乡游梦记（伴奏）", artists: ["著小生zoki", "徵羽摩柯", "乐正龙牙", "Hanser"], album: "雪乡游梦记", quality: "MP3 320k", art: "ice", glyph: "雪", lyrics: false },
  dala: { key: "dala", dir: "QQ音乐", file: "hanser-达拉崩吧.mflac", qq: true, title: "达拉崩吧", artists: ["Hanser", "电鸟个灯泡", "尤超白"], album: "达拉崩吧", quality: "FLAC 16bit/44.1kHz", art: "comic", glyph: "崩" },
  broken: { key: "broken", dir: "QQ音乐", file: "逍遥游.mgg", qq: true, artists: [], quality: "", art: "grey", glyph: "?" },
  magic: { key: "magic", dir: "QQ音乐", file: "hanser-魔法少女的午后.mflac", qq: true, title: "魔法少女的午后", artists: ["Hanser"], album: "魔法少女的午后", quality: "FLAC 24bit/48kHz", art: "rose", glyph: "魔" },
  sakura: { key: "sakura", dir: "网易云音乐", file: "泠鸢yousa,Hanser - 千本樱（Live）.ncm", qq: false, title: "千本樱（Live）", artists: ["泠鸢yousa", "Hanser"], album: "千本樱 Live", quality: "MP3 320k", art: "violet", glyph: "樱" },
};

function job(s: Song, state: Job["state"]): Job {
  const id = `${s.key}-${uid++}`;
  extra.set(id, { title: s.title, artists: s.artists, album: s.album, quality: s.quality, art: ART[s.art], glyph: s.glyph });
  const j: Job = { id, source: `${HOME}/Music/${s.dir}/${s.file}`, fileName: s.file, platform: s.qq ? "qq" : "netease", state, artists: [], hasCover: true, hasLyrics: s.lyrics ?? true, renamed: s.renamed };
  if (state !== "downloading") j.title = s.title;
  if (!["downloading", "decrypt", "transcode"].includes(state)) j.album = s.album;
  return setState(j, state);
}
/** 按状态补齐契约里「何时有」的字段：歌名在解密后、专辑在元数据阶段、封面与音质在完成后 */
function setState(j: Job, state: Job["state"]): Job {
  const x = extra.get(j.id)!;
  j.state = state;
  const known = state !== "downloading" && !!x.title && (!!j.title || !["queued", "decrypt"].includes(state));
  j.title = known ? x.title : undefined;
  j.artists = known ? x.artists : [];
  j.album = known && (["metadata", "done", "skipped"].includes(state) || (!!j.album && !["decrypt", "transcode"].includes(state))) ? x.album : undefined;
  const fin = state === "done" || state === "skipped";
  j.quality = fin ? x.quality : undefined;
  j.coverUrl = fin ? cover(x.art, x.glyph) : undefined;
  j.output = fin ? `${HOME}/Music/musicReveal/${j.fileName}` : undefined;
  j.error = state === "failed" ? "文件不完整" : undefined;
  if (state !== "done" && state !== "skipped") j.playing = undefined;
  return j;
}

function push() {
  let pos = 0;
  for (const j of [...snap.jobs].reverse()) j.queuePos = j.state === "queued" ? ++pos : undefined;
  snap.busy = snap.jobs.filter((j) => ["downloading", "queued", ...PROCESSING].includes(j.state)).length;
  snap.waitingLogin = snap.jobs.filter((j) => j.state === "waitLogin").length;
  const s = structuredClone(snap);
  subs.forEach((cb) => cb(s));
}

function base(onboarded: boolean): Snapshot {
  return {
    onboarded,
    folders: onboarded
      ? [
          { path: `${HOME}/Music/QQ音乐`, name: "QQ音乐", accessible: true },
          { path: `${HOME}/Music/网易云音乐`, name: "网易云音乐", accessible: true },
        ]
      : [],
    outputDir: `${HOME}/Music/musicReveal`,
    format: "original",
    qq: { state: "loggedIn", account: "12****89" },
    jobs: [],
    notices: [],
    busy: 0,
    waitingLogin: 0,
  };
}
snap = base(true);

const find = (id: string) => snap.jobs.find((j) => j.id === id);
const first = () => snap.jobs.find((j) => j.id.startsWith("erciyuan"))!;

// 一首歌走完整条流水线；时间点与设计稿的 runTask 相同
async function run(j: Job, fromDownload = false) {
  const tok = (runs.get(j.id) ?? 0) + 1;
  runs.set(j.id, tok);
  const alive = () => runs.get(j.id) === tok && snap.jobs.includes(j);
  const step = async (s: Job["state"], ms: number) => {
    if (!alive()) return false;
    setState(j, s);
    push();
    await sleep(ms);
    return alive();
  };
  if (fromDownload && !(await step("downloading", 1800))) return;
  if (!(await step("queued", 700))) return;
  if (j.platform === "qq" && snap.qq.state !== "loggedIn") return void (setState(j, "waitLogin"), push());
  for (const [s, ms] of [["decrypt", 1100], ["transcode", 1500], ["metadata", 1000]] as const) if (!(await step(s, ms))) return;
  setState(j, "done");
  push();
}

const DAILY: [string, Job["state"]][] = [
  ["erciyuan", "transcode"], ["dl", "downloading"], ["rain", "queued"], ["jiaozhi", "done"],
  ["xianxia", "done"], ["xuexiang", "done"], ["dala", "skipped"], ["broken", "failed"],
];
const PICKS = ["/Volumes/外置盘/音乐/下载/网易云备份", `${HOME}/Downloads`, `${HOME}/Downloads/hanser`];
let failPlay = "";

export const mockBackend: Backend = {
  async invoke<T>(cmd: string, a: Record<string, any> = {}): Promise<T> {
    const r = (() => {
      switch (cmd) {
        case "get_snapshot": return structuredClone(snap);
        case "complete_onboarding":
          Object.assign(snap, { onboarded: true, outputDir: a.outputDir, format: a.format, folders: a.folders.map((p: string) => ({ path: p, name: p.split("/").pop(), accessible: true })) });
          break;
        case "add_watch_folder": snap.folders.push({ path: a.path, name: a.path.split("/").pop(), accessible: true }); return { existing: 0 };
        case "remove_watch_folder": snap.folders = snap.folders.filter((f) => f.path !== a.path); break;
        case "convert_existing": case "ignore_existing": snap.notices = snap.notices.filter((n) => n.folder !== a.path); break;
        case "dismiss_notice": snap.notices = snap.notices.filter((n) => n.id !== a.id); break;
        case "set_output_dir": snap.outputDir = a.path; break;
        case "set_format": snap.format = a.format; break;
        case "add_files":
          for (const p of a.paths as string[]) { const j = job({ ...SONGS.magic, key: "file", file: p.split("/").pop()! }, "queued"); snap.jobs.unshift(j); run(j); }
          break;
        case "retry_job": { const j = find(a.id); if (j) run(j); break; }
        case "remove_job": snap.jobs = snap.jobs.filter((j) => j.id !== a.id); break;
        case "play_preview":
          if (failPlay) throw failPlay;
          snap.jobs.forEach((j) => (j.playing = j.id === a.id || undefined));
          break;
        case "stop_preview": snap.jobs.forEach((j) => (j.playing = undefined)); break;
        case "open_qq_music": case "open_auth_settings": MR.qq("loggedIn"); break;
        case "pick_folder": return PICKS.find((p) => !snap.folders.some((f) => f.path === p)) ?? null;
        case "reveal_in_finder": break;
        default: throw `mock: unknown command ${cmd}`;
      }
      return undefined;
    })();
    if (cmd !== "get_snapshot" && cmd !== "pick_folder") setTimeout(push, 50);
    return r as T;
  },
  onSnapshot: (cb) => void subs.push(cb),
  onDragDrop: (cb) => void (drag = cb),
  pickFiles: async () => [`${HOME}/Downloads/hanser-魔法少女的午后.mflac`],
};

const frames = (n = 2) => new Promise<void>((r) => { const f = () => (--n ? requestAnimationFrame(f) : r()); requestAnimationFrame(f); });
const click = (sel: string) => document.querySelector<HTMLElement>(sel)?.click();

const MR = {
  get snap() { return snap; },
  push,
  scene(name: "onboarding" | "empty" | "daily") {
    runs.clear();
    snap = base(name !== "onboarding");
    if (name === "daily") snap.jobs = DAILY.map(([k, s]) => job(SONGS[k], s));
    push();
  },
  /** 引导第 n 步：从当前步点「上一步」「继续」走过去 */
  async obStep(n: number) {
    for (let i = 0; i < 3; i++) click("#obBack");
    for (let i = 1; i < n; i++) click("#obNext");
    await frames();
  },
  qq(state: Snapshot["qq"]["state"]) {
    snap.qq = state === "loggedIn" ? { state, account: "12****89" } : { state };
    if (state === "loggedIn") snap.jobs.filter((j) => j.state === "waitLogin").forEach((j, i) => setTimeout(() => run(j), i * 600));
    else snap.jobs.filter((j) => j.platform === "qq" && ["queued", ...PROCESSING].includes(j.state)).forEach((j) => (runs.delete(j.id), setState(j, "waitLogin")));
    push();
  },
  task(state: Job["state"]) {
    const j = first();
    runs.delete(j.id);
    setState(j, state);
    push();
  },
  drag(on: boolean) {
    drag(on ? { type: "enter", paths: [`${HOME}/Downloads/hanser-魔法少女的午后.mflac`, `${HOME}/Downloads/泠鸢yousa - 病名为爱.ncm`] } : { type: "leave" });
  },
  bad(on: boolean) {
    snap.folders[snap.folders.length - 1].accessible = !on;
    push();
  },
  ask() {
    const path = PICKS[0];
    snap.folders.push({ path, name: "网易云备份", accessible: true });
    snap.notices.unshift({ id: `n${uid++}`, kind: "existingSongs", folder: path, count: 14 });
    push();
  },
  caughtUp() {
    const j = job(SONGS.magic, "queued");
    snap.jobs.unshift(j);
    snap.notices.unshift({ id: `n${uid++}`, kind: "caughtUp", count: 1 });
    push();
  },
  failPlay(msg: string) { failPlay = msg; },
  /** 新下载到达：下载中 → 排队 → 解密 → 转码 → 元数据 → 完成 */
  simulateNew(key = "magic") {
    const j = job(SONGS[key], "downloading");
    snap.jobs.unshift(j);
    return run(j, true);
  },
  /** 第 5 条验收：连推 50 次快照，同一行的 DOM 节点不得被替换 */
  async selfTest() {
    MR.scene("daily");
    await sleep(1000); // 等上一场景的行收起移除
    const nodes = () => new Map([...document.querySelectorAll<HTMLElement>("#tasks .row-wrap:not(.collapsed)")].map((el) => [el.dataset.id!, [el, el.querySelector(".row"), el.querySelector(".tile"), el.querySelector(".t1")]]));
    const before = nodes();
    const cycle: Job["state"][] = ["downloading", "queued", "decrypt", "transcode", "metadata", "done", "playing" as never];
    const j = first(), done = snap.jobs.find((x) => x.state === "done")!, q = snap.jobs.find((x) => x.state === "queued")!;
    for (let i = 0; i < 50; i++) {
      const s = cycle[i % cycle.length];
      if (s === ("playing" as never)) j.playing = true;
      else setState(j, s);
      done.playing = i % 2 ? true : undefined;
      if (i % 5 === 4) snap.jobs.push(snap.jobs.splice(snap.jobs.indexOf(q), 1)[0]);
      push();
      await sleep(16);
    }
    await frames(3);
    const after = nodes();
    let replaced = 0;
    for (const [id, els] of before) if (!after.has(id) || els.some((el, i) => el !== after.get(id)![i] || !el?.isConnected)) replaced++;
    const res = { pushes: 50, rows: before.size, rowsAfter: after.size, replaced, ok: replaced === 0 && after.size === before.size };
    console.log("[selfTest]", JSON.stringify(res));
    return res;
  },
};
(window as any).MR = MR;
MR.scene("daily");
