// 开发期假后台：只在 vite dev + ?mock 时由 main.ts 动态加载，生产构建里不存在。
// 按同一份 IPC 契约实现快照 / 资料库 / 播放器 / 歌词；window.MR 供截图脚本把界面切到任意状态。
import type { Backend, DragDrop } from "./backend";
import type { Job, Library, LyricLine, PlayerState, QueueView, Repeat, Snapshot, Track } from "./types";
import { titleFromFile } from "./dom";

const HOME = "/Users/me";
const OUT = `${HOME}/Music/musicReveal`;
const PROCESSING = ["decrypt", "transcode", "metadata"];
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const clone = <T>(v: T): T => structuredClone(v);

/* ===================================================================
   数据：取自设计稿 player.html（歌词为自编，非真实歌词）
   =================================================================== */
const ART: Record<string, string[]> = {
  pink: ["#5B1B5E", "#FF5C8A", "#FFC9A0"], teal: ["#0F2E4C", "#2CC6B0", "#9FF0E2"], ice: ["#B9D7F0", "#5A6FA8", "#FFFFFF"],
  amber: ["#F2B035", "#D14D1F", "#FFF2B0"], comic: ["#F23B3B", "#2E1B4A", "#FFD36E"], grey: ["#55585F", "#1E2024", "#9AA0AA"],
  violet: ["#2B1E6B", "#8A5BFF", "#FFB7E6"], moss: ["#1E3B2A", "#8BC34A", "#F6FFB3"], rose: ["#3A0E22", "#E04A6B", "#FFD1DC"],
};
const ARTS = Object.keys(ART);
// 歌名 | 歌手 | 专辑 | 年份 | 秒 | 格式 | 音质 | 封面 | 歌词(L / LT 带翻译 / 空)
const SONG_ROWS: any[] = [
  ['魔法少女的午后','Hanser','魔法少女的午后',2024,209,'FLAC','24bit/48kHz','rose','L'],
  ['二次元凶','Hanser','二次元凶',2023,225,'FLAC','24bit/48kHz','pink','L'],
  ['「交织together」','DMYoung、Hanser、泠鸢yousa','交织together',2021,252,'MP3','320k','teal','L'],
  ['雪乡游梦记（伴奏）','著小生zoki、徵羽摩柯、乐正龙牙、Hanser','雪乡游梦记',2020,280,'MP3','320k','ice',''],
  ['雪乡游梦记','著小生zoki、徵羽摩柯、乐正龙牙、Hanser','雪乡游梦记',2020,280,'MP3','320k','ice','L'],
  ['不需要陪伴的闲暇','Hanser','一抹憨色',2019,238,'FLAC','16bit/44.1kHz','amber','L'],
  ['一抹憨色','Hanser','一抹憨色',2019,213,'FLAC','16bit/44.1kHz','amber','L'],
  ['达拉崩吧','Hanser、电鸟个灯泡、尤超白','达拉崩吧',2017,248,'FLAC','16bit/44.1kHz','comic','L'],
  ['千本樱（Live）','泠鸢yousa、Hanser','千本樱 Live',2019,242,'MP3','320k','violet','LT'],
  ['星夜鸟语','泠鸢yousa','星夜鸟语',2022,216,'FLAC','24bit/48kHz','violet','L'],
  ['病名为爱','泠鸢yousa','病名为爱',2016,231,'FLAC','16bit/44.1kHz','rose','LT'],
  ['十一月的雨','DMYoung','十一月的雨',2018,261,'MP3','320k','moss','L'],
  ['逍遥游','Hanser','逍遥游',2022,233,'FLAC','24bit/48kHz','grey','L'],
  ['风铃与猫的午睡','Hanser','魔法少女的午后',2024,198,'FLAC','24bit/48kHz','rose','L'],
  ['风铃与猫的午睡（伴奏）','Hanser','魔法少女的午后',2024,198,'FLAC','24bit/48kHz','rose',''],
  ['月光下的小卖部','Hanser、小缘','月光下的小卖部',2021,227,'MP3','320k','teal','L'],
  ['春日便当','Hanser','二次元凶',2023,204,'FLAC','24bit/48kHz','pink','L'],
  ['云端电台（Live at 上海）','Hanser','云端电台 Live',2023,318,'FLAC','24bit/48kHz','ice','L'],
  ['纸飞机航线','泠鸢yousa、Hanser','纸飞机航线',2020,244,'MP3','320k','violet','L'],
  ['星期三的雨伞','DMYoung、Hanser','十一月的雨',2018,236,'MP3','320k','moss','L'],
  ['木星来信','Hanser','一抹憨色',2019,255,'FLAC','16bit/44.1kHz','amber','L'],
  ['夏夜拾音','著小生zoki、Hanser','拾音集',2021,229,'MP3','320k','moss','L'],
  ['小镇电影院','Hanser','二次元凶',2023,247,'FLAC','24bit/48kHz','pink','L'],
  ['梦游者的早餐','Hanser','魔法少女的午后',2024,221,'FLAC','24bit/48kHz','rose','L'],
  ['流星便利店（翻唱）','Hanser','翻唱集 2022',2022,235,'MP3','320k','grey','L'],
  ['暗房','Hanser','二次元凶',2023,256,'FLAC','24bit/48kHz','pink','L'],
  ['猫与收音机','花园Serena、Hanser','猫与收音机',2020,207,'MP3','320k','teal','L'],
  ['旧照片显影液','Hanser','魔法少女的午后',2024,263,'FLAC','24bit/48kHz','rose','L'],
  ['深夜巴士终点站','泠鸢yousa','星夜鸟语',2022,249,'FLAC','24bit/48kHz','violet','L'],
  ['糖霜与雪','Hanser、祖娅纳惜','糖霜与雪',2018,226,'FLAC','16bit/44.1kHz','ice','L'],
  ['糖霜与雪（伴奏）','Hanser、祖娅纳惜','糖霜与雪',2018,226,'FLAC','16bit/44.1kHz','ice',''],
  ['海边的无线电塔','DMYoung','十一月的雨',2018,271,'MP3','320k','moss','L'],
  ['周末修理铺','Hanser','逍遥游',2022,212,'FLAC','24bit/48kHz','grey','L'],
  ['橘子汽水与晚霞','Hanser','一抹憨色',2019,219,'FLAC','16bit/44.1kHz','amber','L'],
  ['午夜动物园广播','Hanser、三无MarBlue','午夜动物园广播',2021,240,'MP3','320k','comic','L'],
  ['电车上的回声','Hanser','逍遥游',2022,232,'FLAC','24bit/48kHz','grey','L'],
  ['蝴蝶结与雨衣','小缘','蝴蝶结与雨衣',2019,205,'MP3','320k','pink','L'],
  ['雾中灯塔','Hanser','二次元凶',2023,288,'FLAC','24bit/48kHz','pink','L'],
  ['日落前的三十秒','Hanser、泠鸢yousa、DMYoung','交织together',2021,230,'MP3','320k','teal','L'],
  ['秋刀鱼之味','Hanser','翻唱集 2022',2022,243,'MP3','320k','grey','L'],
  ['候鸟图鉴','徵羽摩柯、Hanser','候鸟图鉴',2020,258,'MP3','320k','moss','L'],
  ['便签上的歌','Hanser','魔法少女的午后',2024,194,'FLAC','24bit/48kHz','rose','L'],
  ['长途电话','Hanser','一抹憨色',2019,266,'FLAC','16bit/44.1kHz','amber','L'],
  ['显影','Hanser','二次元凶',2023,241,'FLAC','24bit/48kHz','pink','L'],
];
const EXTRA_ROWS: any[] = [
  ["在很久很久以前的一个下着小雨的星期天下午我们约好去看海（2024 重新编曲版）", "Hanser", "很长的歌名合集", 2024, 302, "FLAC", "24bit/48kHz", "teal", "L"],
  ["无封面的小样", "Hanser", "", 0, 187, "MP3", "320k", "", ""],
  ["录音棚里的第三遍", "DMYoung", "未发表", 0, 214, "MP3", "320k", "", "L"],
];
const QPOOL: QSong[] = [
  { file:'hanser-薄荷糖与旧电影.mflac', title:'薄荷糖与旧电影', artists:['Hanser'], album:'薄荷糖与旧电影', src:'QQ', fmt:'FLAC', quality:'24bit/48kHz', art:'teal', needsQQ:true },
  { file:'泠鸢yousa - 深海邮筒.ncm', title:'深海邮筒', artists:['泠鸢yousa'], album:'深海邮筒', src:'网易云', fmt:'MP3', quality:'320k', art:'violet' },
  { file:'hanser-雨季备忘录.mflac', title:'雨季备忘录', artists:['Hanser'], album:'雨季备忘录', src:'QQ', fmt:'FLAC', quality:'24bit/48kHz', art:'moss', needsQQ:true },
  { file:'DMYoung,Hanser - 回声实验室（Live）.ncm', title:'回声实验室（Live）', artists:['DMYoung','Hanser'], album:'回声实验室 Live', src:'网易云', fmt:'MP3', quality:'320k', art:'comic' },
  { file:'hanser-地铁末班车的猫.mflac', title:'地铁末班车的猫', artists:['Hanser'], album:'地铁末班车的猫', src:'QQ', fmt:'FLAC', quality:'16bit/44.1kHz', art:'amber', needsQQ:true },
  { file:'hanser-失物招领处.mflac', title:'失物招领处', artists:['Hanser'], album:'失物招领处', src:'QQ', fmt:'FLAC', quality:'24bit/48kHz', art:'rose', needsQQ:true },
];
const MANY_TITLES: any[] = ['冰箱贴日记','阁楼天文台','第七个夏天','蓝色信号灯','午后三点的猫','风筝线','便利店夜班','星图练习','雨伞合奏','纸月亮','海盐棒冰','旧电台','候车室','萤火虫路线','口袋里的海','小夜曲 No.3','晚安实验','周日早餐','暖气片协奏曲','阳台种子','秋千与雪','书页间的风','环线列车','午夜修表匠','洗衣店的猫','烟花回放','泡沫纪念日','玻璃弹珠','手帐里的雨'];
const LYR_A: any[] = [[0,'暗房里的灯 亮成一小片橙'],[8,'底片上的你 还没有轮廓'],[16,'时间往回倒 把声音泡进水里'],[24,'等一张脸 慢慢浮出来'],[33,'显影 显影 别眨眼'],[40,'颜色一层一层 回到从前'],[48,'显影 显影 别说话'],[56,'我在等那首歌 把你喊回家'],[66,'窗外有人在练琴 弹错同一个音'],[74,'我把它录下来 当作今天的签名'],[82,'旧磁带转到一半 突然想起你'],[90,'那年夏天的风 还卡在齿轮里'],[100,'显影 显影 别眨眼'],[108,'颜色一层一层 回到从前'],[116,'显影 显影 别说话'],[124,'我在等那首歌 把你喊回家'],[134,'…'],[152,'如果照片会唱歌 它会先唱哪一句'],[160,'大概是你笑着说 别拍了别拍了'],[170,'显影 显影 灯灭之前'],[178,'把今天留在 相纸的边缘'],[188,'显影 显影 别睡着'],[196,'我把这首歌 洗成了你的样子'],[210,'晾干的夜 挂在绳上 轻轻摇']];
const LYR_B: any[] = [[0,'夜の窓に 光が落ちて','夜晚的窗上 落下一片光'],[9,'まだ名前のない 歌を拾う','捡起一首 还没有名字的歌'],[18,'止まった時計 針を回して','把停住的时钟 指针拨回去'],[27,'声を水に浸す 君が浮かぶまで','把声音浸在水里 直到你浮现'],[36,'現像 現像 目を閉じないで','显影 显影 别闭上眼'],[45,'色がひとつずつ 帰ってくる','颜色一层一层 回来了'],[54,'現像 現像 何も言わないで','显影 显影 什么都别说'],[63,'あの歌が君を 連れて帰る','那首歌会把你 带回家'],[74,'…'],[96,'写真が歌えるなら 最初の一行は','如果照片会唱歌 第一句会是'],[105,'たぶん君の 「撮らないで」','大概是你那句「别拍了」'],[114,'現像 現像 灯りが消える前に','显影 显影 灯灭之前'],[123,'今日を紙の端に 残しておく','把今天留在 相纸的边缘'],[135,'干した夜が 風に揺れている','晾干的夜 在风里轻轻摇']];

type QSong = { file: string; title?: string; artists: string[]; album?: string; src: string; fmt: string; quality: string; art: string; needsQQ?: boolean };
const glyphOf = (s: string) => s.match(/[\p{L}\p{N}]/u)?.[0] ?? "?";
// 用设计稿的配色画一张假封面，代替后台的 asset:// 缩略图
const covers = new Map<string, string>();
function cover(art: string, glyph: string) {
  const k = art + glyph;
  if (!covers.has(k)) {
    const [a, b, c] = ART[art];
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256"><defs><linearGradient id="g" x1="0" y1="0" x2=".6" y2="1"><stop offset="0" stop-color="${a}"/><stop offset="1" stop-color="${b}"/></linearGradient><radialGradient id="r" cx=".18" cy=".12" r=".9"><stop offset="0" stop-color="${c}"/><stop offset=".58" stop-color="${c}" stop-opacity="0"/></radialGradient></defs><rect width="256" height="256" fill="url(#g)"/><rect width="256" height="256" fill="url(#r)"/><text x="150" y="230" font-size="190" font-weight="700" font-family="PingFang SC, sans-serif" fill="#fff" fill-opacity=".55" text-anchor="middle">${glyph}</text></svg>`;
    covers.set(k, "data:image/svg+xml," + encodeURIComponent(svg));
  }
  return covers.get(k)!;
}
const LYRICS = new Map<string, LyricLine[]>();
const toLines = (L: any[]): LyricLine[] => L.map(([s, text, trans]) => ({ ms: s * 1000, text, ...(trans ? { trans } : {}) }));
const EXT: Record<string, string> = { ALAC: "m4a", AAC: "m4a" };
const pathOf = (artists: string[], title: string, fmt: string) => `${OUT}/${artists.join("、")} - ${title}.${EXT[fmt] ?? fmt.toLowerCase()}`;
/** Apple Music 兼容：无损 → ALAC，OGG / Opus → AAC 256k，其余不变 */
const appleQ = (q: string) => q.replace(/^(FLAC|WAV)\b/, "ALAC").replace(/^(OGG|Opus)\b.*/, "AAC 256k");

function mkTrack(r: any[], i: number, now = Date.now()): Track {
  const [title, artists, album, year, sec, fmt, q, art, lyr] = r;
  const as = (artists as string).split("、");
  const t: Track = {
    id: pathOf(as, title, fmt), title, artists: as, album: album || undefined, year: year || undefined, durationMs: sec * 1000,
    quality: `${fmt} ${q}`, coverUrl: art ? cover(art, glyphOf(title)) : undefined, hasLyrics: !!lyr, addedAt: now - i * 3_600_000,
  };
  if (lyr) LYRICS.set(t.id, toLines(lyr === "LT" ? LYR_B : LYR_A));
  return t;
}
const fullLibrary = () => [...SONG_ROWS, ...EXTRA_ROWS].map((r, i) => mkTrack(r, i));

/* ===================================================================
   状态
   =================================================================== */
let snap: Snapshot;
let lib: Library = { dir: OUT, tracks: [], scanning: false };
/** queue 为实际播放顺序（随机时打乱），ids 为点播时的原顺序 */
const P = { ids: [] as string[], queue: [] as string[], i: -1, rev: 0, playing: false, base: 0, at: 0, volume: 0.7, shuffle: false, repeat: "off" as Repeat, error: undefined as string | undefined };
type Extra = { title?: string; artists: string[]; album?: string; quality: string; art: string; noTrack?: boolean };
const extra = new Map<string, Extra>();
const runs = new Map<string, number>();
const subs = { snapshot: [] as ((s: Snapshot) => void)[], library: [] as ((l: Library) => void)[], player: [] as ((p: PlayerState) => void)[] };
let drag: (e: DragDrop) => void = () => {};
let uid = 1;
/** 前端发出的每个 invoke（截图 / 交互脚本断言用） */
const calls: [string, Record<string, any>][] = [];

function base(onboarded: boolean): Snapshot {
  return {
    onboarded,
    folders: onboarded
      ? [
          { path: `${HOME}/Downloads/hanser`, name: "hanser", accessible: true },
          { path: "/Volumes/外置盘/音乐/下载", name: "下载", accessible: true },
        ]
      : [],
    outputDir: OUT, format: "original", qq: { state: "loggedIn", account: "12****89" },
    jobs: [], notices: [], busy: 0, waitingLogin: 0,
  };
}
snap = base(true);

function pushSnap() {
  let pos = 0;
  for (const j of [...snap.jobs].reverse()) j.queuePos = j.state === "queued" ? ++pos : undefined;
  snap.busy = snap.jobs.filter((j) => ["downloading", "queued", ...PROCESSING].includes(j.state)).length;
  snap.waitingLogin = snap.jobs.filter((j) => j.state === "waitLogin").length;
  const s = clone(snap);
  subs.snapshot.forEach((cb) => cb(s));
}
const pushLib = () => {
  const l = clone(lib);
  subs.library.forEach((cb) => cb(l));
};
const track = (id?: string) => lib.tracks.find((t) => t.id === id);
/** 播放器直接读文件，不看资料库（同真后台）：资料库没有这首也照样播 */
const files = new Map<string, number>();
const durOf = (id?: string) => (id ? track(id)?.durationMs ?? files.get(id) ?? 0 : 0);
/** 打不开的文件：play / next 时跳过并写 error，全部失败则清空 trackId */
const broken = new Set<string>();
const position = () => (P.playing ? P.base + Date.now() - P.at : P.base);
function playerState(): PlayerState {
  const id = P.queue[P.i], d = durOf(id);
  return { ...(id ? { trackId: id } : {}), playing: P.playing && !!id, positionMs: Math.min(position(), d), durationMs: d, volume: P.volume, shuffle: P.shuffle, repeat: P.repeat, ...(P.error ? { error: P.error } : {}), queueRev: P.rev };
}
function load() {
  while (P.queue[P.i] && broken.has(P.queue[P.i])) {
    const id = P.queue[P.i];
    P.error = `「${titleFromFile(id.split("/").pop() ?? id)}」文件不存在，已跳过`;
    P.queue = P.queue.filter((x) => x !== id);
    P.ids = P.ids.filter((x) => x !== id);
    P.rev++;
    if (P.i >= P.queue.length) P.i = P.queue.length ? 0 : -1;
  }
  if (P.i < 0) P.playing = false;
}
const pushPlayer = () => {
  const p = playerState();
  subs.player.forEach((cb) => cb(p));
};
const setPos = (ms: number) => ((P.base = ms), (P.at = Date.now()));
function advance(auto: boolean, dir = 1) {
  if (!P.queue.length) return;
  if (auto && P.repeat === "one") return setPos(0);
  let i = P.i + dir;
  if (i >= P.queue.length) {
    if (auto && P.repeat === "off") return (P.playing = false), setPos(0);
    i = 0;
  }
  if (i < 0) i = P.queue.length - 1;
  P.i = i;
  P.error = undefined;
  setPos(0);
  load();
}
/** 同真后台 PlayQueue::set_shuffle：开则当前曲排第一、其余打乱；关则回原顺序 */
function reorder() {
  const cur = P.queue[P.i];
  if (P.shuffle) {
    const rest = P.ids.filter((x) => x !== cur);
    for (let k = rest.length - 1; k > 0; k--) {
      const j = Math.floor(Math.random() * (k + 1));
      [rest[k], rest[j]] = [rest[j], rest[k]];
    }
    P.queue = cur ? [cur, ...rest] : rest;
  } else P.queue = [...P.ids];
  P.i = cur ? P.queue.indexOf(cur) : -1;
  P.rev++;
}
/** 同真后台 Player::remove：移出队列；是当前曲就接着放下一首（末首且非循环全部则停、清空当前曲） */
function removeFromQueue(id: string) {
  P.ids = P.ids.filter((x) => x !== id);
  const k = P.queue.indexOf(id);
  if (k < 0) return;
  P.queue.splice(k, 1);
  P.rev++;
  if (k < P.i) P.i--;
  if (k !== P.i) return;
  if (P.i >= P.queue.length) P.i = P.repeat === "all" && P.queue.length ? 0 : -1;
  if (P.i < 0) return void ((P.playing = false), setPos(0));
  P.playing = true;
  P.error = undefined;
  setPos(0);
  load();
}
// 播放中每秒推一次（同真后台），播完按循环 / 随机走下一首
setInterval(() => {
  if (!P.playing) return;
  if (position() >= durOf(P.queue[P.i])) advance(true);
  pushPlayer();
}, 1000);

/* —— 转换流水线 —— */
function job(s: QSong, state: Job["state"]): Job {
  const id = `j${uid++}`;
  extra.set(id, { title: s.title, artists: s.artists, album: s.album, quality: s.fmt === "MP3" ? `MP3 ${s.quality}` : `${s.fmt} ${s.quality}`, art: s.art });
  const platform = s.src === "QQ" ? "qq" : s.src === "网易云" ? "netease" : "other";
  const j: Job = { id, source: `${HOME}/Downloads/hanser/${s.file}`, fileName: s.file, platform, state, artists: [], hasCover: true, hasLyrics: true };
  return setState(j, state);
}
/** 按状态补齐契约里「何时有」的字段：歌名在解密后、专辑在元数据阶段、封面与音质在完成后 */
function setState(j: Job, state: Job["state"]): Job {
  const x = extra.get(j.id)!;
  j.state = state;
  const known = !!x.title && !["downloading", "queued", "decrypt", "waitLogin"].includes(state) || (!!j.title && state !== "downloading");
  j.title = known ? x.title : undefined;
  j.artists = known ? x.artists : [];
  j.album = known && ["metadata", "done", "skipped"].includes(state) ? x.album : undefined;
  const fin = state === "done" || state === "skipped";
  const q = snap.format === "apple" ? appleQ(x.quality) : x.quality;
  j.quality = fin ? q : undefined;
  j.coverUrl = fin ? cover(x.art, glyphOf(x.title ?? "?")) : undefined;
  j.output = fin && x.title ? pathOf(x.artists, x.title, q.split(" ")[0]) : undefined;
  j.error = state === "failed" ? "文件不完整，可能还没下载完" : undefined;
  return j;
}
/** 完成后稍晚由「资料库监听」发现新文件 */
async function arrive(j: Job) {
  const x = extra.get(j.id)!;
  if (x.noTrack || !j.output) return;
  await sleep(5);
  if (track(j.output)) return;
  const [fmt, ...q] = j.quality!.split(" ");
  const t = mkTrack([x.title, x.artists.join("、"), x.album, 2026, 200 + (uid * 37) % 90, fmt, q.join(" "), x.art, "L"], 0);
  lib.tracks.push(t);
  pushLib();
}
/** 完成的真实顺序（真后台实测）：done 但缩略图还没生成 → 约 50ms 后带 coverUrl 的 done → 随后 library 收到这首 */
async function finish(j: Job) {
  setState(j, "done");
  const c = j.coverUrl;
  j.coverUrl = undefined;
  pushSnap();
  await sleep(53);
  if (!snap.jobs.includes(j)) return;
  j.coverUrl = c;
  pushSnap();
  await arrive(j);
}
async function run(j: Job, fromDownload = false) {
  const tok = (runs.get(j.id) ?? 0) + 1;
  runs.set(j.id, tok);
  const alive = () => runs.get(j.id) === tok && snap.jobs.includes(j);
  const step = async (s: Job["state"], ms: number) => {
    if (!alive()) return false;
    setState(j, s);
    pushSnap();
    await sleep(ms);
    return alive();
  };
  if (fromDownload && !(await step("downloading", 1800))) return;
  if (!(await step("queued", 700))) return;
  if (j.platform === "qq" && snap.qq.state !== "loggedIn") return void (setState(j, "waitLogin"), pushSnap());
  for (const [s, ms] of [["decrypt", 1100], ["transcode", 1500], ["metadata", 1000]] as const) if (!(await step(s, ms))) return;
  finish(j);
}

/* —— 切换输出格式：资料库里已有的歌逐首转换（同真后台：转完一首推一次资料库与快照） —— */
const RF = { step: 700, list: [] as Track[], fail: new Set<string>(), reject: "", timer: 0 };
const needs = (to: Snapshot["format"]) =>
  lib.tracks.filter((t) => (to === "apple" ? /^(FLAC|WAV) /.test(t.quality) : /^ALAC /.test(t.quality))).sort((a, b) => b.addedAt - a.addedAt);
const SEC_PER_SONG = 5;
/** 转换后路径（扩展名）变了：资料库 id、播放队列、歌词一起跟着换 */
function convertTrack(t: Track, to: Snapshot["format"]) {
  const fmt = to === "apple" ? "ALAC" : "FLAC", id = pathOf(t.artists, t.title, fmt), old = t.id;
  P.queue = P.queue.map((x) => (x === old ? id : x));
  files.set(id, t.durationMs);
  if (LYRICS.has(old)) LYRICS.set(id, LYRICS.get(old)!);
  t.id = id;
  t.quality = t.quality.replace(/^\S+/, fmt);
}
function startReformat(to: Snapshot["format"]) {
  RF.list = needs(to);
  snap.reformat = { target: to, total: RF.list.length, done: 0, failed: [], finished: false };
  aim();
}
/** current / 封面 / 剩余时间指向下一首 */
function aim() {
  const rf = snap.reformat!, t = RF.list[rf.done];
  if (rf.done >= rf.total) return Object.assign(rf, { finished: true, current: undefined, coverUrl: undefined, etaSecs: undefined });
  Object.assign(rf, { current: t.title, coverUrl: t.coverUrl, etaSecs: (rf.total - rf.done) * SEC_PER_SONG });
}
function stepReformat() {
  const rf = snap.reformat;
  if (!rf || rf.finished) return false;
  const t = RF.list[rf.done];
  if (RF.fail.has(t.title)) rf.failed.push({ title: t.title, reason: "转换后逐项比对未通过，原文件保持不变" });
  else convertTrack(t, rf.target);
  rf.done++;
  aim();
  return true;
}
function runReformat() {
  clearInterval(RF.timer);
  RF.timer = window.setInterval(() => {
    if (!stepReformat()) return clearInterval(RF.timer);
    pushLib();
    pushSnap();
    pushPlayer();
  }, RF.step);
}
const checkFmt = (f: string) => {
  if (f !== "original" && f !== "apple") throw `无效的输出格式：${f}`;
};

const PICKS = ["/Volumes/外置盘/音乐/备份", `${HOME}/Downloads`, `${HOME}/Music/网易云音乐`];

export const mockBackend: Backend = {
  async invoke<T>(cmd: string, a: Record<string, any> = {}): Promise<T> {
    calls.push([cmd, clone(a)]);
    let snapChanged = true;
    const r = (() => {
      switch (cmd) {
        case "get_snapshot": snapChanged = false; return clone(snap);
        case "get_library": snapChanged = false; return clone(lib);
        case "get_player": snapChanged = false; return playerState();
        case "get_lyrics": snapChanged = false; return track(a.id) ? clone(LYRICS.get(a.id) ?? null) : null; // 同真后台：资料库不认识的歌没有歌词
        case "complete_onboarding":
          checkFmt(a.format);
          Object.assign(snap, { onboarded: true, outputDir: a.outputDir, format: a.format, folders: a.folders.map((p: string) => ({ path: p, name: p.split("/").pop(), accessible: true })) });
          break;
        case "add_watch_folder": snap.folders.push({ path: a.path, name: a.path.split("/").pop(), accessible: true }); return { existing: 0 };
        case "remove_watch_folder": snap.folders = snap.folders.filter((f) => f.path !== a.path); break;
        case "convert_existing": {
          snap.notices = snap.notices.filter((n) => n.folder !== a.path);
          MANY_TITLES.slice(0, 14).forEach((t: string, i: number) => {
            const j = job({ file: `${t}.ncm`, title: t, artists: ["Hanser"], album: t, src: "网易云", fmt: "MP3", quality: "320k", art: ARTS[(i + 3) % ARTS.length] }, "queued");
            snap.jobs.push(j);
            setTimeout(() => run(j), i * 900);
          });
          break;
        }
        case "ignore_existing": snap.notices = snap.notices.filter((n) => n.folder !== a.path); break;
        case "dismiss_notice": snap.notices = snap.notices.filter((n) => n.id !== a.id); break;
        case "set_output_dir":
          snap.outputDir = a.path;
          // 同真后台：换文件夹即清空并重扫
          lib = { dir: a.path, tracks: [], scanning: true };
          setTimeout(pushLib, 50);
          setTimeout(() => ((lib.scanning = false), pushLib()), 900);
          break;
        case "preview_format": {
          snapChanged = false;
          checkFmt(a.format);
          const n = a.format === snap.format ? 0 : needs(a.format).length;
          return { count: n, etaSecs: n * SEC_PER_SONG };
        }
        case "set_format": {
          checkFmt(a.format);
          if (snap.reformat && !snap.reformat.finished) throw "正在转换，完成前不能更改";
          if (RF.reject) throw RF.reject;
          const n = a.format === snap.format ? 0 : needs(a.format).length;
          snap.format = a.format;
          if (n) startReformat(a.format), runReformat();
          break;
        }
        case "dismiss_reformat": if (snap.reformat?.finished) delete snap.reformat; break;
        case "add_files":
          for (const p of a.paths as string[]) {
            const file = p.split("/").pop()!;
            const j = job({ ...QPOOL[uid % QPOOL.length], file, src: /\.ncm$/.test(file) ? "网易云" : /\.(mp3|flac|ogg|m4a|wav)$/.test(file) ? "" : "QQ", needsQQ: false }, "queued");
            snap.jobs.unshift(j);
            run(j);
          }
          break;
        case "retry_job": { const j = snap.jobs.find((x) => x.id === a.id); if (j) run(j); break; }
        case "remove_job": snap.jobs = snap.jobs.filter((j) => j.id !== a.id); break;
        case "open_qq_music": case "open_auth_settings": MR.qq("loggedIn"); break;
        case "pick_folder": snapChanged = false; return PICKS.find((p) => !snap.folders.some((f) => f.path === p)) ?? null;
        case "reveal_in_finder": case "reveal_track": snapChanged = false; break;
        case "open_settings": snapChanged = false; window.open(location.pathname + "?mock&view=settings", "settings", "width=520,height=440"); break;
        case "trash_track":
          snapChanged = false;
          lib.tracks = lib.tracks.filter((t) => t.id !== a.id);
          removeFromQueue(a.id);
          setTimeout(pushLib, 120);
          setTimeout(pushPlayer, 30);
          break;
        default: {
          snapChanged = false;
          if (!cmd.startsWith("player_")) throw `mock: unknown command ${cmd}`;
          const out = player(cmd.slice(7), a);
          setTimeout(pushPlayer, 30);
          return out;
        }
      }
      return undefined;
    })();
    if (snapChanged) setTimeout(pushSnap, 50);
    return r as T;
  },
  onSnapshot: (cb) => void subs.snapshot.push(cb),
  onLibrary: (cb) => void subs.library.push(cb),
  onPlayer: (cb) => void subs.player.push(cb),
  onDragDrop: (cb) => void (drag = cb),
  pickFiles: async () => [`${HOME}/Downloads/hanser-魔法少女的午后.mflac`],
};

function player(cmd: string, a: Record<string, any>): QueueView | void {
  switch (cmd) {
    case "play":
      P.ids = [...a.ids]; P.queue = [...a.ids]; P.i = a.index; P.playing = true; P.error = undefined;
      if (P.shuffle) reorder();
      P.rev++; setPos(0); load();
      break;
    case "queue": return { ids: [...P.queue], pos: P.i < 0 ? P.queue.length : P.i };
    case "jump":
      if (!(a.index >= 0 && a.index < P.queue.length)) return;
      P.i = a.index; P.playing = true; P.error = undefined; setPos(0); load();
      break;
    case "toggle":
      P.error = undefined;
      if (P.i < 0) return;
      setPos(position());
      P.playing = !P.playing;
      break;
    case "next": P.error = undefined; advance(false); break;
    case "prev": P.error = undefined; if (position() > 3000) setPos(0); else advance(false, -1); break;
    case "seek": setPos(a.ms); break;
    case "set_volume": P.volume = a.volume; break;
    case "set_shuffle": P.shuffle = a.on; reorder(); break;
    case "set_repeat": P.repeat = a.mode; break;
    default: throw `mock: unknown command player_${cmd}`;
  }
}

/* ===================================================================
   window.MR：把界面切到任意状态（截图脚本用）
   =================================================================== */
const $ = (s: string) => document.querySelector<HTMLElement>(s);
const frames = (n = 2) => new Promise<void>((r) => { const f = () => (--n ? requestAnimationFrame(f) : r()); requestAnimationFrame(f); });
function search(q: string) {
  const inp = $("#searchInput") as HTMLInputElement | null;
  if (!inp) return; // 设置窗口没有资料库
  inp.value = q;
  inp.dispatchEvent(new Event("input"));
}
function lyrics(on: boolean) {
  if (!!$("#lyrics.on") !== on) $("#lcdCover")?.click();
}
function playTitle(title: string, atSec = 0, playing = true) {
  const ids = lib.tracks.map((t) => t.id), i = lib.tracks.findIndex((t) => t.title === title);
  player("play", { ids, index: Math.max(0, i) });
  setPos(atSec * 1000);
  P.playing = playing;
  pushPlayer();
}
function stop() {
  P.ids = []; P.queue = []; P.i = -1; P.rev++; P.playing = false; P.error = undefined; setPos(0);
  pushPlayer();
}
function proc(): Job { const j = job(QPOOL[0], "transcode"); return j; }
const queued = (p: QSong) => job(p, "queued");
function tray(mode: string) {
  runs.clear();
  if (mode === "few") snap.jobs = [proc(), job(QPOOL[1], "downloading"), queued(QPOOL[2])];
  if (mode === "many")
    snap.jobs = [proc(), job(QPOOL[1], "downloading"), ...MANY_TITLES.map((n: string, i: number) => queued({ file: `hanser-${n}.mflac`, title: n, artists: ["Hanser"], album: n, src: i % 3 ? "QQ" : "网易云", fmt: i % 3 ? "FLAC" : "MP3", quality: i % 3 ? "24bit/48kHz" : "320k", art: ARTS[i % ARTS.length] }))];
  if (mode === "failed") snap.jobs = [proc(), queued(QPOOL[2]), job({ file: "逍遥游.mgg", artists: [], src: "QQ", fmt: "OGG", quality: "", art: "grey" }, "failed")];
  if (mode === "waitqq") snap.jobs = [queued(QPOOL[1]), job(QPOOL[4], "waitLogin"), job(QPOOL[5], "waitLogin")];
  if (mode === "empty") snap.jobs = [];
}

const MR = {
  get snap() { return snap; },
  get lib() { return lib; },
  get player() { return playerState(); },
  calls,
  /** 恢复日常：有歌、显影盘少量、播放中、已登录、无通知、无搜索 / 歌词 / 菜单 / 拖放 */
  reset() {
    document.body.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" })); // 关掉设置窗口里开着的确认 sheet
    search("");
    lyrics(false);
    drag({ type: "leave" });
    snap = base(true);
    clearInterval(RF.timer);
    RF.fail.clear();
    RF.reject = "";
    lib = { dir: OUT, tracks: fullLibrary(), scanning: false };
    lib.tracks.forEach((t) => files.set(t.id, t.durationMs));
    broken.clear();
    tray("few");
    pushSnap();
    pushLib();
    playTitle("二次元凶", 41);
  },
  async scene(name: string) {
    MR.reset();
    switch (name) {
      case "daily": break;
      case "queue-empty": tray("empty"); break;
      case "queue-many": tray("many"); break;
      case "queue-failed": tray("failed"); break;
      case "queue-waitqq": snap.qq = { state: "loggedOut" }; tray("waitqq"); break;
      case "paused": tray("empty"); P.playing = false; break;
      case "not-playing": tray("empty"); stop(); break;
      case "lib-empty": tray("empty"); lib.tracks = []; stop(); break;
      case "first-run": lib.tracks = []; stop(); break;
      case "no-result": tray("empty"); break;
      case "lyrics": tray("empty"); playTitle("二次元凶", 41); break;
      case "lyrics-trans": tray("empty"); playTitle("千本樱（Live）", 20); break;
      case "lyrics-none": tray("empty"); playTitle("雪乡游梦记（伴奏）", 30); break;
      case "play-error": tray("empty"); P.error = "无法打开音频设备"; P.playing = false; break;
      // 队列里的歌全部打不开：trackId 清空、只剩 error
      case "play-failed": tray("empty"); broken.add(lib.tracks[3].id); player("play", { ids: [lib.tracks[3].id], index: 0 }); break;
      // 资料库还在读（启动 / 刚换输出文件夹），播放器已经在放：用 MR.finishScan() 读完
      case "scanning": tray("empty"); lib = { dir: OUT, tracks: [], scanning: true }; break;
      case "drag": break;
      case "ask": MR.ask(); break;
      case "caught-up": snap.notices.unshift({ id: `n${uid++}`, kind: "caughtUp", count: 3 }); break;
      case "qq-expired": snap.qq = { state: "expired" }; break;
      case "qq-needauth": snap.qq = { state: "needsAuth" }; break;
      case "onboarding": snap = base(false); lib.tracks = []; stop(); break;
      case "menu": tray("empty"); break;
      // 输出格式「Apple Music 兼容」：主窗口（?mock）与设置窗口（?mock&view=settings）各开一份、同名场景
      case "format-off": case "format-confirm-on": case "format-error": tray("empty"); RF.reject = name === "format-error" ? "无法保存设置，请稍后再试" : ""; break;
      case "format-on": case "format-confirm-off": tray("empty"); snap.format = "apple"; needs("apple").forEach((t) => convertTrack(t, "apple")); break;
      case "format-converting": case "format-done": case "format-partial":
        tray("empty");
        snap.format = "apple";
        if (name === "format-partial") RF.fail.add("不需要陪伴的闲暇");
        startReformat("apple");
        // format-done 留最后一首到界面渲染之后再转，同真实流程（主窗口那一行先见过转换中）
        for (let i = 0; i < (name === "format-converting" ? 3 : RF.list.length - (name === "format-done" ? 1 : 0)); i++) stepReformat();
        break;
      default: throw new Error("unknown scene " + name);
    }
    pushSnap();
    pushLib();
    pushPlayer();
    await frames(3);
    if (name === "no-result") search("周杰伦");
    if (name.startsWith("lyrics")) lyrics(true);
    if (name === "drag") MR.drag(true);
    if (name === "menu") await sleep(700), MR.menu(2);
    if (name === "format-done") stepReformat(), pushLib(), pushSnap(), await frames(2);
    // 设置窗口：拨开关 → 确认 sheet（format-error 再点确认，后台拒绝）
    if (/^format-(confirm|error)/.test(name) && $("#fmtSwitch")) {
      $("#fmtSwitch")!.click();
      await sleep(80);
      if (name === "format-error") $("#shOk")?.click(), await sleep(80);
    }
    await frames(2);
  },
  /** 引导第 n 步：从当前步点「上一步」「继续」走过去 */
  async obStep(n: number) {
    for (let i = 0; i < 3; i++) $("#obBack")?.click();
    for (let i = 1; i < n; i++) $("#obNext")?.click();
    await frames();
  },
  qq(state: Snapshot["qq"]["state"]) {
    snap.qq = state === "loggedIn" ? { state, account: "12****89" } : { state };
    if (state === "loggedIn") snap.jobs.filter((j) => j.state === "waitLogin").forEach((j, i) => setTimeout(() => run(j), i * 600));
    else snap.jobs.filter((j) => j.platform === "qq" && ["queued", ...PROCESSING].includes(j.state)).forEach((j) => (runs.delete(j.id), setState(j, "waitLogin")));
    pushSnap();
  },
  drag(on: boolean) {
    drag(on ? { type: "enter", paths: [`${HOME}/Downloads/hanser-魔法少女的午后.mflac`, `${HOME}/Downloads/泠鸢yousa - 病名为爱.ncm`] } : { type: "leave" });
  },
  ask() {
    const path = PICKS[0];
    if (!snap.folders.some((f) => f.path === path)) snap.folders.push({ path, name: "备份", accessible: true });
    snap.notices.unshift({ id: `n${uid++}`, kind: "existingSongs", folder: path, count: 14 });
    pushSnap();
  },
  /** 右键第 n 行（0 起） */
  menu(n: number) {
    const li = document.querySelectorAll<HTMLElement>("#songs .song")[n];
    const r = li.getBoundingClientRect();
    li.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: r.left + r.width * 0.45, clientY: r.top + r.height / 2 }));
  },
  search,
  lyrics,
  stop,
  /** 给正在资料库里的某首放进同名 .lrc：library 事件里 hasLyrics false → true */
  addLrc(title: string) {
    const t = lib.tracks.find((x) => x.title === title)!;
    t.hasLyrics = true;
    LYRICS.set(t.id, toLines(LYR_A));
    pushLib();
  },
  finishScan() {
    lib = { dir: OUT, tracks: fullLibrary(), scanning: false };
    pushLib();
  },
  /** 模拟一首转换完成：处理中的那首跑完三步 → 完成 → 资料库收到新歌（noTrack：资料库始终收不到） */
  async simulateDone(opts: { noTrack?: boolean } = {}) {
    let j = snap.jobs.find((x) => PROCESSING.includes(x.state));
    if (!j) {
      j = job(QPOOL[(uid + 2) % QPOOL.length], "queued");
      snap.jobs.unshift(j);
    }
    if (opts.noTrack) extra.get(j.id)!.noTrack = true;
    runs.set(j.id, (runs.get(j.id) ?? 0) + 1);
    const tok = runs.get(j.id);
    for (const [s, ms] of [["decrypt", 700], ["transcode", 900], ["metadata", 700]] as const) {
      if (PROCESSING.indexOf(j.state) > PROCESSING.indexOf(s)) continue;
      setState(j, s);
      pushSnap();
      await sleep(ms);
      if (runs.get(j.id) !== tok) return;
    }
    await finish(j);
  },
  /** 新下载到达：下载中 → 排队 → 解密 → 转码 → 元数据 → 完成 → 入库 */
  simulateNew() {
    const j = job(QPOOL[(uid + 1) % QPOOL.length], "downloading");
    snap.jobs.unshift(j);
    return run(j, true);
  },
  skip() {
    const j = snap.jobs.find((x) => x.state === "queued");
    if (j) setState(j, "skipped"), pushSnap();
  },
  rf: RF,
  /** 现场跑一遍格式切换（同前端调 set_format） */
  convert(to: Snapshot["format"]) {
    return mockBackend.invoke("set_format", { format: to });
  },
  /** n 首的大资料库（性能验收） */
  bigLibrary(n = 2000) {
    const now = Date.now();
    lib = {
      dir: OUT, scanning: false,
      tracks: Array.from({ length: n }, (_, i) => {
        const r = SONG_ROWS[i % SONG_ROWS.length], m = MANY_TITLES[i % MANY_TITLES.length];
        return mkTrack([`${m}${i % 7 ? "" : "（Live）"} ${i + 1}`, r[1], `${r[2]} ${Math.floor(i / 12) + 1}`, r[3], r[4], r[5], r[6], i % 11 ? r[7] : "", r[8]], i, now);
      }),
    };
    pushLib();
  },
};
(window as any).MR = MR;
MR.reset();
