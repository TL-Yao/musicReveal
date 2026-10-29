// 资料库：页头、歌曲列表（排序 / 搜索 / 选中 / 播放 / 右键菜单）、空状态。数据来自 `library` 事件。
import type { Library, Track } from "./types";
import { $, $$, artStyle, esc, fmtTime, glyphHTML, GREY, hashArt, icon } from "./dom";
import { S, call } from "./store";
import { openLyricsFor, togglePlay } from "./player";

type Key = "added" | "title" | "artist" | "album" | "dur";
const SORT_LABEL: Record<Key, string> = { added: "最近添加", title: "歌名", artist: "歌手", album: "专辑", dur: "时长" };
const col = new Intl.Collator("zh-Hans-CN", { numeric: true });

const sort = { key: "added" as Key, dir: -1 };
let query = "";
let selected: string | null = null;
let view: Track[] = [];
const byId = new Map<string, Track>();
let rows = new Map<string, HTMLElement>();
/** 入库仪式进行中的行：先以高度 0、无封面渲染，由仪式揭开 */
export const landing = new Set<string>();
/** 「移到废纸篓」的行收起期间推迟重绘 */
let holdUntil = 0, holdTimer = 0;

export const trackById = (id?: string) => (id ? byId.get(id) : undefined);
export const rowEl = (id: string) => rows.get(id);
export const visibleIds = () => view.map((t) => t.id);
/** 「最近添加」降序且未搜索时，新歌落在第一行；否则飞进计数 */
export const toRowMode = () => sort.key === "added" && sort.dir === -1 && !query.trim();
export const play = (id: string) => {
  const ids = visibleIds();
  call("player_play", { ids, index: Math.max(0, ids.indexOf(id)) });
};
export async function addFiles() {
  const paths = await S.B.pickFiles().catch(() => null);
  if (paths) call("add_files", { paths });
}

/* —— 封面（资料库 / LCD / 歌词视图共用） —— */
const OV = `<div class="ov play">${icon("play")}</div><div class="ov pause">${icon("pause")}</div><div class="ov eq"><span class="eq"><i></i><i></i><i></i></span></div>`;
export const coverArt = (t: { title: string; coverUrl?: string }) =>
  `<div class="art">${t.coverUrl ? `<img src="${esc(t.coverUrl)}" alt="" loading="lazy" decoding="async">` : glyphHTML(t.title)}</div>`;
export const coverHTML = (t: Track, cls = "") => `<div class="cover ${cls}" style="${artStyle(hashArt(t.title))}">${coverArt(t)}${OV}</div>`;

export function setLibrary(l: Library) {
  S.lib = l;
  byId.clear();
  for (const t of l.tracks) byId.set(t.id, t);
  renderLib();
}

function compute() {
  const q = query.trim().toLowerCase();
  let a = S.lib.tracks;
  if (q) a = a.filter((t) => `${t.title} ${t.artists.join(" ")} ${t.album ?? ""}`.toLowerCase().includes(q));
  const { key, dir } = sort;
  const inAlbum = (x: Track, y: Track) => col.compare(x.album ?? "", y.album ?? "") || (x.disc ?? 0) - (y.disc ?? 0) || (x.track ?? 0) - (y.track ?? 0);
  const cmp = (x: Track, y: Track) =>
    key === "added" ? x.addedAt - y.addedAt
    : key === "dur" ? x.durationMs - y.durationMs
    : key === "title" ? col.compare(x.title, y.title)
    : key === "artist" ? col.compare(x.artists.join("、"), y.artists.join("、")) || inAlbum(x, y)
    : inAlbum(x, y);
  return [...a].sort((x, y) => cmp(x, y) * dir || col.compare(x.title, y.title) || (x.id < y.id ? -1 : 1));
}

export function renderLib() {
  const wait = holdUntil - performance.now();
  if (wait > 0) return void (clearTimeout(holdTimer), (holdTimer = window.setTimeout(renderLib, wait)));
  view = compute();
  renderHead();
  renderSongs();
  renderVoid();
}

function renderHead() {
  const n = S.lib.tracks.length, total = view.reduce((a, t) => a + t.durationMs, 0) / 1000;
  const h = Math.floor(total / 3600), m = Math.round((total % 3600) / 60);
  $("#libSub").innerHTML = !n && S.lib.scanning ? "正在读取…"
    : query.trim() ? `<b id="libCount">${view.length}</b> 首匹配`
    : `<b id="libCount">${n}</b> 首歌曲 · ${h ? h + " 小时 " : ""}${m} 分钟`;
  const sb = $("#sortBtn");
  sb.innerHTML = `排序：${SORT_LABEL[sort.key]}${icon("chev")}`;
  sb.style.display = n ? "" : "none";
  $("#navCount").textContent = String(n);
  const cols: [Key | "fmt", string, string?][] = [["title", "歌曲"], ["album", "专辑"], ["dur", "时长", "r"], ["fmt", "格式", "r"]];
  $("#thead").innerHTML = cols
    .map(([k, l, r]) =>
      k === "fmt" ? `<span class="${r}" style="display:inline-flex;justify-content:flex-end">${l}</span>`
      : `<button class="${r ?? ""} ${sort.key === k ? "on" : ""} ${sort.dir > 0 ? "asc" : ""}" data-k="${k}">${l}${icon("chev")}</button>`)
    .join("");
}

const rowHTML = (t: Track, i: number) => {
  const land = landing.has(t.id) ? " landing nocover" : "";
  return `<li class="song${land}" id="song-${i}" role="option" aria-selected="false" data-id="${esc(t.id)}"><div class="c-title">${coverHTML(t)}<div class="tt"><div class="t1">${esc(t.title)}</div><div class="t2">${esc(t.artists.join("、"))}</div></div></div><div class="c-album">${esc(t.album ?? "")}</div><div class="c-dur">${fmtTime(t.durationMs)}</div>${fmtCell(t)}</li>`;
};

/* 格式转换批次：正在转的那首（按歌名匹配）格式列显示「正在转换…」，转完那一下格式字样弹一下 */
let busyTitle: string | undefined;
let flip = { title: "", until: 0 };
const fmtCell = (t: Track) => {
  const busy = t.title === busyTitle, flipped = t.title === flip.title && performance.now() < flip.until;
  return `<div class="c-fmt${busy ? " busy" : flipped ? " flip" : ""}">${busy ? "正在转换…" : esc(t.quality)}</div>`;
};
export function setBusy(title?: string) {
  if (title === busyTitle) return;
  const was = busyTitle;
  busyTitle = title;
  if (was) flip = { title: was, until: performance.now() + 1500 };
  for (const t of view) {
    const c = (t.title === was || t.title === title) && rows.get(t.id)?.querySelector(".c-fmt");
    if (c) c.outerHTML = fmtCell(t);
  }
}
function renderSongs() {
  const ul = $("#songs");
  ul.innerHTML = view.map(rowHTML).join("");
  rows = new Map($$("li", ul).map((li) => [li.dataset.id!, li]));
  painted.clear();
  syncRows();
}

/* 选中 / 正在播放 / 暂停：只动有变化的行 */
const painted = new Set<string>();
export function syncRows() {
  const pid = S.player.trackId;
  const ids = new Set(painted);
  if (pid) ids.add(pid);
  if (selected) ids.add(selected);
  painted.clear();
  for (const id of ids) {
    const el = rows.get(id);
    if (!el) continue;
    const sel = id === selected, on = id === pid;
    el.classList.toggle("sel", sel);
    el.setAttribute("aria-selected", String(sel));
    el.classList.toggle("playing", on);
    el.classList.toggle("paused", on && !S.player.playing);
    if (sel || on) painted.add(id);
  }
  const ul = $("#songs"), sEl = selected ? rows.get(selected) : undefined;
  if (sEl) ul.setAttribute("aria-activedescendant", sEl.id);
  else ul.removeAttribute("aria-activedescendant");
}
const select = (id: string | null) => ((selected = id), syncRows());

export function renderVoid() {
  const empty = !S.lib.tracks.length, none = !empty && !view.length;
  $("#thead").classList.toggle("off", empty || none);
  $("#songs").classList.toggle("off", empty || none);
  const folders = S.snap?.folders.filter((f) => f.accessible).length ?? 0;
  let html = "";
  if (empty && !S.lib.scanning)
    html = `<div class="stage-card"><div class="tile" style="${artStyle(GREY)}"><div class="art"></div><div class="dash"></div></div><h2>资料库还是空的</h2><p>${
      folders ? "监听文件夹里新下载的歌曲转换完成后会出现在这里，<br>也可以把音频文件拖进来。" : "把音频文件拖到这里，<br>或在侧栏添加监听文件夹。"
    }</p><div class="cta"><button class="btn" id="voidAdd">添加文件…</button></div></div>`;
  else if (none) html = `<div class="stage-card"><h2>没有找到「<span class="q">${esc(query.trim())}</span>」</h2><p>换个歌名、歌手或专辑试试。</p></div>`;
  const v = $("#void");
  if (v.dataset.sig !== html) (v.dataset.sig = html), (v.innerHTML = html);
}

export function bumpCount() {
  const b = $("#libCount");
  b?.classList.remove("bump");
  void b?.offsetWidth;
  b?.classList.add("bump");
}

function setSort(k: Key, dir?: number) {
  if (dir) sort.dir = dir;
  else if (sort.key === k) sort.dir *= -1;
  else sort.dir = k === "added" ? -1 : 1;
  sort.key = k;
  renderLib();
}
function setQuery(q: string) {
  query = q;
  const inp = $<HTMLInputElement>("#searchInput");
  if (inp.value !== q) inp.value = q;
  $("#searchBox").classList.toggle("has", !!q);
  renderLib();
  $("#scroll").scrollTop = 0;
}

export function trash(id: string) {
  const el = rows.get(id);
  if (el) {
    el.classList.add("gone");
    holdUntil = performance.now() + parseFloat(getComputedStyle(el).getPropertyValue("--dur-slow")) + 40;
  }
  call("trash_track", { id });
  renderLib(); // 收起后按当时的资料库重绘：失败时行会回来
}

/* —— 菜单（右键 / 排序） —— */
type Item = { label: string; on?: boolean; run: () => void } | null;
let menuReturn: HTMLElement | null = null;
/** kbd：由键盘打开，焦点进菜单（↑↓ 选、↩ 执行、Esc 关闭并把焦点还回去） */
export function openMenu(x: number, y: number, items: Item[], kbd = false) {
  const m = $("#menu");
  m.innerHTML = items.map((it, i) => (it ? `<button role="menuitem" tabindex="-1" data-i="${i}" class="${it.on ? "on" : ""}"><svg class="ck"><use href="#i-check"/></svg>${esc(it.label)}</button>` : "<hr>")).join("");
  $$("button", m).forEach((b) => (b.onclick = () => (closeMenu(), items[+b.dataset.i!]!.run())));
  m.style.left = Math.min(x, innerWidth - 220) + "px";
  m.style.top = Math.max(4, Math.min(y, innerHeight - m.offsetHeight - 8)) + "px";
  m.classList.add("on");
  menuReturn = kbd ? (document.activeElement as HTMLElement) : null;
  if (kbd) $("button", m).focus();
}
export function closeMenu() {
  const m = $("#menu");
  if (!m.classList.contains("on")) return;
  m.classList.remove("on");
  if (m.contains(document.activeElement)) menuReturn?.focus();
  menuReturn = null;
}
const songMenu = (id: string): Item[] => [
  { label: "播放", run: () => play(id) },
  { label: "显示歌词", run: () => openLyricsFor(id, () => play(id)) },
  null,
  { label: "在 Finder 中显示", run: () => call("reveal_track", { id }) },
  { label: "移到废纸篓", run: () => trash(id) },
];
const sortMenu = (): Item[] => [
  ...(Object.keys(SORT_LABEL) as Key[]).map((k) => ({ label: SORT_LABEL[k], on: sort.key === k, run: () => setSort(k, k === "added" ? -1 : 1) })),
  null,
  { label: "升序", on: sort.dir > 0, run: () => setSort(sort.key, 1) },
  { label: "降序", on: sort.dir < 0, run: () => setSort(sort.key, -1) },
];

export function initLibrary() {
  const ul = $("#songs");
  ul.addEventListener("click", (e) => {
    const li = (e.target as Element).closest<HTMLElement>(".song");
    if (!li) return;
    const id = li.dataset.id!;
    if ((e.target as Element).closest(".cover")) {
      if (e.detail > 1) return;
      return S.player.trackId === id ? togglePlay() : play(id);
    }
    select(id);
  });
  ul.addEventListener("dblclick", (e) => {
    const t = e.target as Element, li = t.closest<HTMLElement>(".song");
    if (li && !t.closest(".cover")) play(li.dataset.id!);
  });
  // 键盘：整张表一个 tab stop（aria-activedescendant 指向选中行），↑↓/Home/End 选、↩ 播放、⇧F10 菜单
  ul.addEventListener("keydown", (e) => {
    if (e.metaKey || e.ctrlKey || e.altKey || !view.length) return;
    const i = view.findIndex((t) => t.id === selected), last = view.length - 1;
    const to = ({ ArrowDown: i < 0 ? 0 : Math.min(last, i + 1), ArrowUp: i < 0 ? last : Math.max(0, i - 1), Home: 0, End: last } as Record<string, number>)[e.key];
    if (to !== undefined) {
      e.preventDefault();
      select(view[to].id);
      rows.get(view[to].id)?.scrollIntoView({ block: "nearest" });
    } else if (e.key === "Enter" && selected) {
      e.preventDefault();
      play(selected);
    } else if (((e.key === "F10" && e.shiftKey) || e.key === "ContextMenu") && selected) {
      e.preventDefault();
      const r = rows.get(selected)!.getBoundingClientRect();
      openMenu(r.left + 48, r.bottom, songMenu(selected), true);
    }
  });
  $("#menu").addEventListener("keydown", (e) => {
    const bs = $$<HTMLButtonElement>("button", $("#menu")), k = bs.indexOf(document.activeElement as HTMLButtonElement);
    const d = e.key === "ArrowDown" ? 1 : e.key === "ArrowUp" ? -1 : 0;
    if (d) e.preventDefault(), bs[(k + d + bs.length) % bs.length].focus();
    if (e.key === "Tab") e.preventDefault();
  });
  ul.addEventListener("contextmenu", (e) => {
    const li = (e.target as Element).closest<HTMLElement>(".song");
    if (!li) return;
    e.preventDefault();
    select(li.dataset.id!);
    openMenu(e.clientX, e.clientY, songMenu(li.dataset.id!));
  });
  $("#thead").addEventListener("click", (e) => {
    const k = (e.target as Element).closest<HTMLElement>("button[data-k]")?.dataset.k;
    if (k) setSort(k as Key);
  });
  $("#sortBtn").onclick = (e) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    openMenu(r.left, r.bottom + 4, sortMenu(), (e as MouseEvent).detail === 0);
  };
  $("#addFiles").onclick = addFiles;
  $("#void").addEventListener("click", (e) => (e.target as Element).closest("#voidAdd") && addFiles());
  $<HTMLInputElement>("#searchInput").oninput = (e) => setQuery((e.target as HTMLInputElement).value);
  $("#searchClear").onclick = () => (setQuery(""), $("#searchInput").focus());
  document.addEventListener("mousedown", (e) => !(e.target as Element).closest("#menu") && closeMenu());
  window.addEventListener("blur", closeMenu);
}
