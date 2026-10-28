// 任务行：按 job id 复用 DOM，状态在原位变化；封面「显影」由 --dev 驱动。
import type { Job } from "./types";
import { $, esc, icon, reduced } from "./dom";

// 设计稿 ART 的 9 套配色：没有封面时按 source 哈希稳定取一套
const ART = [
  { a: "#5B1B5E", b: "#FF5C8A", c: "#FFC9A0", angle: "155deg" },
  { a: "#0F2E4C", b: "#2CC6B0", c: "#9FF0E2", angle: "140deg" },
  { a: "#B9D7F0", b: "#5A6FA8", c: "#FFFFFF", angle: "160deg" },
  { a: "#F2B035", b: "#D14D1F", c: "#FFF2B0", angle: "145deg" },
  { a: "#F23B3B", b: "#2E1B4A", c: "#FFD36E", angle: "135deg" },
  { a: "#55585F", b: "#1E2024", c: "#9AA0AA", angle: "150deg" },
  { a: "#2B1E6B", b: "#8A5BFF", c: "#FFB7E6", angle: "150deg" },
  { a: "#1E3B2A", b: "#8BC34A", c: "#F6FFB3", angle: "145deg" },
  { a: "#3A0E22", b: "#E04A6B", c: "#FFD1DC", angle: "160deg" },
];
const STEP_NAMES = ["解密", "转码", "元数据"];
const STEP_STATE = ["解密中", "转码中", "写入元数据"];
const STEP: Record<string, number> = { decrypt: 0, transcode: 1, metadata: 2 };
/** 没有步内百分比：步骤条当前段缓动逼近这个上限 */
const SP_MAX = 0.92;

type Look = "downloading" | "queued" | "processing" | "done" | "skipped" | "waitQQ" | "failed";
const look = (j: Job): Look => (j.state in STEP ? "processing" : j.state === "waitLogin" ? "waitQQ" : j.state) as Look;
const devTarget = (j: Job) => (j.state in STEP ? (STEP[j.state] + 1) / 3 - 0.08 : j.state === "done" || j.state === "skipped" ? 1 : 0);

export function hashArt(s: string) {
  let h = 0;
  for (const ch of s) h = (Math.imul(h, 31) + ch.codePointAt(0)!) | 0;
  return ART[Math.abs(h) % ART.length];
}
export const glyphOf = (s: string) => s.match(/[\p{L}\p{N}]/u)?.[0] ?? "?";
export const tileStyle = (a: (typeof ART)[number]) => `--art-a:${a.a};--art-b:${a.b};--art-c:${a.c};--art-angle:${a.angle}`;

const ROW_HTML = `<div><div class="row"><div class="tile">
    <div class="art latent"><span class="glyph"></span><img class="cover" alt=""></div>
    <div class="art clear"><span class="glyph"></span><img class="cover" alt=""></div>
    <div class="wash"></div><div class="ring"></div><div class="dash"></div>
    <div class="overlay lock">${icon("lock")}</div><div class="overlay warn">${icon("warn")}</div>
    <div class="overlay play">${icon("play")}</div><div class="overlay eq"><span class="eq"><i></i><i></i><i></i></span></div>
  </div>
  <div class="info"><div class="t1"></div><div class="t2"></div></div>
  <div class="side"><div class="actions"></div><div class="state"></div><div class="meta"></div></div>
</div></div>`;

type Row = { el: HTMLLIElement; job: Job; dev: number; sp: number; sig: string };
export type RowAct = "play" | "finder" | "retry" | "remove";

const rows = new Map<string, Row>();
const errs = new Map<string, { msg: string; timer: number }>();
let list: HTMLElement;

export function initRows(el: HTMLElement, act: (j: Job, a: RowAct) => void) {
  list = el;
  list.addEventListener("click", (e) => {
    const t = e.target as Element;
    const r = rows.get(t.closest<HTMLElement>(".row-wrap")?.dataset.id ?? "");
    if (!r) return;
    const b = t.closest<HTMLElement>("[data-act]");
    if (b) act(r.job, b.dataset.act as RowAct);
    else if (t.closest(".tile") && (r.job.state === "done" || r.job.state === "skipped")) act(r.job, "play");
  });
}

/** 副行临时显示一条错误，3 秒后恢复 */
export function flashError(id: string, msg: string) {
  clearTimeout(errs.get(id)?.timer);
  const timer = window.setTimeout(() => (errs.delete(id), refill(id)), 3000);
  errs.set(id, { msg, timer });
  refill(id);
}
const refill = (id: string) => {
  const r = rows.get(id);
  if (r) fill(r, r.job);
};

export function renderTasks(jobs: Job[], initial: boolean) {
  const ids = new Set(jobs.map((j) => j.id));
  for (const [id, r] of rows) {
    if (ids.has(id)) continue;
    rows.delete(id);
    r.el.classList.add("collapsed");
    r.el.addEventListener("transitionend", (e) => e.target === r.el && r.el.remove());
  }
  let prev: HTMLElement | null = null;
  for (const j of jobs) {
    let r = rows.get(j.id);
    if (!r) {
      const el = document.createElement("li");
      el.className = "row-wrap collapsed";
      el.dataset.id = j.id;
      el.innerHTML = ROW_HTML;
      $(".tile", el).setAttribute("style", tileStyle(hashArt(j.source)));
      r = { el, job: j, dev: initial || j.state === "done" || j.state === "skipped" ? devTarget(j) : 0, sp: 0, sig: "" };
      rows.set(j.id, r);
      $(".tile", el).style.setProperty("--dev", String(r.dev));
      place(el, prev);
      fill(r, j);
      requestAnimationFrame(() => requestAnimationFrame(() => el.classList.remove("collapsed")));
    } else {
      if (r.sig !== sig(j)) fill(r, j);
      if ((prev ? prev.nextElementSibling : list.firstElementChild) !== r.el) place(r.el, prev);
    }
    prev = r.el;
  }
  kick();
}
const place = (el: HTMLElement, prev: HTMLElement | null) => (prev ? prev.after(el) : list.prepend(el));
const sig = (j: Job) => JSON.stringify(j) + (errs.get(j.id)?.msg ?? "");

function fill(r: Row, j: Job) {
  const row = $(".row", r.el), tile = $(".tile", row);
  const prev = row.dataset.state, L = look(j);
  if (r.job.state !== j.state) r.sp = 0;
  r.job = j;
  row.dataset.state = L;
  row.classList.toggle("playing", !!j.playing);

  // 封面：占位色块 + 字形；有 coverUrl 后淡入真实封面
  const name = j.title ?? j.fileName, g = glyphOf(name);
  for (const el of row.querySelectorAll<HTMLElement>(".glyph")) {
    el.textContent = g;
    el.classList.toggle("latin", /^[A-Za-z]/.test(g));
  }
  const imgs = row.querySelectorAll<HTMLImageElement>(".cover");
  if (!j.coverUrl) tile.classList.remove("has-cover");
  else if (imgs[0].getAttribute("src") !== j.coverUrl) {
    tile.classList.remove("has-cover");
    imgs[1].onload = () => tile.classList.add("has-cover");
    imgs.forEach((i) => (i.src = j.coverUrl!));
  }

  // 标题：没有歌名时显示文件名；拿到歌名时换场
  const t1 = $(".t1", row);
  t1.classList.toggle("file", !j.title);
  if (t1.textContent !== name) {
    t1.textContent = name;
    if (prev && j.title) {
      t1.classList.remove("swap");
      void t1.offsetWidth;
      t1.classList.add("swap");
    }
  }

  // 副行
  const err = errs.get(j.id)?.msg;
  const sub = err
    ? `<span class="reason">${esc(err)}</span>`
    : L === "failed"
      ? `<span class="reason">${esc(j.error ?? "")}</span>`
      : L === "downloading"
        ? `<span style="color:var(--text-3)">${j.platform === "qq" ? "QQ 音乐" : "网易云音乐"}</span>`
        : !j.title
          ? ""
          : [j.artists.join(", "), j.album ?? ""].filter(Boolean).map(esc).join('<span class="sep">·</span>');
  const t2 = $(".t2", row);
  if (t2.innerHTML !== sub) t2.innerHTML = sub;
  t2.style.display = sub ? "" : "none";

  // 右列
  const src = `<span class="src">${j.platform === "qq" ? "QQ" : "网易云"}</span>`;
  const play = `<button class="abtn ${j.playing ? "on" : ""}" data-act="play" title="${j.playing ? "停止" : "试听"}">${icon(j.playing ? "stop" : "play")}</button><button class="abtn" data-act="finder" title="在 Finder 中显示">${icon("finder")}</button>`;
  let st = "", mt = "", acts = "", always = false;
  switch (L) {
    case "downloading": st = "下载中"; break;
    case "queued": st = "排队中"; mt = `${src} · 第 ${j.queuePos ?? 1} 位`; break;
    case "processing": {
      const s = STEP[j.state];
      st = STEP_STATE[s];
      mt = `<span class="stepbar">${STEP_NAMES.map((n, i) => `<span class="${i < s ? "done" : i === s ? "on" : ""}" style="--sp:${Math.round(r.sp * 100)}%">${n}</span>`).join("")}</span>`;
      break;
    }
    case "done":
      st = esc(j.quality ?? "");
      mt = `${src} · ${j.hasCover ? "封面 · " : ""}${j.hasLyrics ? "歌词" : "无歌词"}${j.renamed ? ` · 已重命名 (${j.renamed})` : ""}`;
      acts = play;
      break;
    case "skipped": st = "已跳过"; mt = `${src} · 输出已存在`; acts = play; break;
    case "waitQQ": st = "待登录"; mt = `${src} · 登录 QQ 音乐后自动继续`; break;
    case "failed": always = true; acts = `<button class="btn" data-act="retry">重试</button><button class="btn" data-act="remove">移除</button>`; break;
  }
  const state = $(".state", row), meta = $(".meta", row), actions = $(".actions", row);
  state.innerHTML = st;
  meta.innerHTML = mt;
  actions.className = "actions " + (always ? "always" : "hover");
  actions.innerHTML = acts;
  state.style.display = st ? "" : "none";
  meta.style.display = mt ? "" : "none";

  // 完成的那一刻：封面弹起 + 安全灯光圈（初次加载就是 done 的不播放）
  if (L === "done" && prev === "processing") {
    tile.classList.remove("pop");
    row.classList.remove("just-done");
    void tile.offsetWidth;
    tile.classList.add("pop");
    row.classList.add("just-done");
  }
  r.sig = sig(j);
}

// 显影进度：每帧把 --dev 逼近目标（设计稿的 tick）；处理中的步骤条同时缓动
let raf = 0;
function tick() {
  let active = false;
  const k = reduced() ? 1 : 0.085;
  for (const r of rows.values()) {
    const tile = $(".tile", r.el), target = devTarget(r.job);
    if (Math.abs(target - r.dev) > 0.002) {
      r.dev += (target - r.dev) * k;
      active = true;
    } else r.dev = target;
    tile.style.setProperty("--dev", r.dev.toFixed(3));
    if (look(r.job) === "processing") {
      r.sp += (SP_MAX - r.sp) * 0.02;
      if (SP_MAX - r.sp > 0.005) active = true;
      r.el.querySelector<HTMLElement>(".stepbar .on")?.style.setProperty("--sp", Math.round(r.sp * 100) + "%");
    }
  }
  raf = active ? requestAnimationFrame(tick) : 0;
}
const kick = () => void (raf ||= requestAnimationFrame(tick));
