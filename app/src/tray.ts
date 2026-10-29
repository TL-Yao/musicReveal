// 显影盘：转换队列作为一条托盘压在资料库上方。按 job id 复用 DOM，状态原位变化；封面「显影」由 --dev 驱动。
// 转换完成 → 入库仪式：显影到底、弹出光圈 → 封面飞进资料库 → 盘里这行收起。
import type { Job, Reformat } from "./types";
import { $, afterMotion, artStyle, esc, glyphHTML, hashArt, icon, minutes, reduced, titleFromFile } from "./dom";
import { S, call } from "./store";
import { bumpCount, landing, renderLib, rowEl, toRowMode, trackById } from "./library";

const STEP_NAMES = ["解密", "转码", "元数据"];
const STEP_STATE = ["解密中", "转码中", "写入元数据"];
const STEP: Record<string, number> = { decrypt: 0, transcode: 1, metadata: 2 };
const PLATFORM = { qq: "QQ 音乐", netease: "网易云音乐", other: "" };
/** 没有步内百分比：步骤条当前段缓动逼近这个上限 */
const SP_MAX = 0.92;
/** 处理中 → 下载中 → 待登录 / 失败 → 排队（按 queuePos） */
const RANK: Partial<Record<Job["state"], number>> = { decrypt: 0, transcode: 0, metadata: 0, downloading: 1, waitLogin: 2, failed: 2, queued: 3 };

type Look = "downloading" | "queued" | "processing" | "done" | "waitQQ" | "failed";
const look = (j: Job): Look => (j.state in STEP ? "processing" : j.state === "waitLogin" ? "waitQQ" : j.state) as Look;
const devTarget = (j: Job) => (j.state in STEP ? (STEP[j.state] + 1) / 3 - 0.08 : j.state === "done" ? 1 : 0);
const nameOf = (j: Job) => j.title ?? titleFromFile(j.fileName);

const ROW_HTML = `<div><div class="row"><div class="tile">
    <div class="art latent"><span class="glyph"></span><img alt=""></div>
    <div class="art clear"><span class="glyph"></span><img alt=""></div>
    <div class="wash"></div><div class="ring"></div><div class="dash"></div>
    <div class="overlay lock">${icon("lock")}</div><div class="overlay warn">${icon("warn")}</div>
  </div>
  <div class="info"><div class="t1"></div><div class="t2"></div></div>
  <div class="side"><div class="actions"></div><div class="state"></div><div class="meta"></div></div>
</div></div>`;

type Row = { el: HTMLLIElement; job: Job; dev: number; sp: number; sig: string; ritual?: boolean };
const rows = new Map<string, Row>();
let list: HTMLElement;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export function initTray() {
  list = $("#tasks");
  list.addEventListener("click", (e) => {
    const t = e.target as Element;
    const r = rows.get(t.closest<HTMLElement>(".row-wrap")?.dataset.id ?? "");
    const a = t.closest<HTMLElement>("[data-act]")?.dataset.act;
    if (r && a) call(a === "retry" ? "retry_job" : "remove_job", { id: r.job.id });
  });
  list.addEventListener("scroll", fade);
  list.addEventListener("transitionend", fade); // 行展开 / 收起完才知道还能不能滚
  $("#trayHead").addEventListener("click", (e) => (e.target as Element).closest("#trayQQ") && call("open_qq_music"));
  $("#batch").addEventListener("click", (e) => (e.target as Element).closest("[data-batch]") && dismissBatch());
}

export function renderTray(jobs: Job[], initial: boolean, reformat?: Reformat) {
  const byId = new Map(jobs.map((j) => [j.id, j]));
  const shown = jobs.filter((j) => j.state in RANK).sort((a, b) => RANK[a.state]! - RANK[b.state]! || (a.queuePos ?? 0) - (b.queuePos ?? 0));
  const ids = new Set(shown.map((j) => j.id));
  for (const [id, r] of rows) {
    const j = byId.get(id);
    // 仪式中仍收展示字段：真后台先发无封面的 done，缩略图生成后再发一次带 coverUrl 的
    if (r.ritual && j?.state === "done" && r.sig !== sig(j)) fill(r, j);
    if (ids.has(id) || r.ritual) continue;
    if (j?.state === "done" && j.output && !initial) startRitual(r, j);
    else collapse(r); // 已跳过 / 已移除
  }
  let prev: HTMLElement | null = null;
  const ritual = [...rows.values()].filter((r) => r.ritual).map((r) => r.job);
  for (const j of [...ritual, ...shown]) {
    let r = rows.get(j.id);
    if (!r) {
      const el = document.createElement("li");
      el.className = "row-wrap collapsed";
      el.dataset.id = j.id;
      el.innerHTML = ROW_HTML;
      r = { el, job: j, dev: initial ? devTarget(j) : 0, sp: 0, sig: "" };
      rows.set(j.id, r);
      $(".tile", el).style.setProperty("--dev", String(r.dev));
      place(el, prev);
      fill(r, j);
      requestAnimationFrame(() => requestAnimationFrame(() => el.classList.remove("collapsed")));
    } else {
      if (!r.ritual && r.sig !== sig(j)) fill(r, j);
      if ((prev ? prev.nextElementSibling : list.firstElementChild) !== r.el) place(r.el, prev);
    }
    prev = r.el;
  }
  renderBatch(reformat);
  chrome(shown);
  kick();
}
const place = (el: HTMLElement, prev: HTMLElement | null) => (prev ? prev.after(el) : list.prepend(el));
const sig = (j: Job) => JSON.stringify(j) + S.snap?.format;

function collapse(r: Row) {
  rows.delete(r.job.id);
  r.el.classList.add("collapsed");
  afterMotion(r.el, reduced() ? 200 : 900, () => r.el.remove());
  chrome();
}

/** 盘头计数、「打开 QQ 音乐」、底部渐隐、空盘收起 */
function chrome(shown = [...rows.values()].filter((r) => !r.ritual).map((r) => r.job)) {
  const c = (...st: Job["state"][]) => shown.filter((j) => st.includes(j.state)).length;
  const parts = batchTs ? [batchTs] : [];
  if (c("decrypt", "transcode", "metadata")) parts.push(`${c("decrypt", "transcode", "metadata")} 首转换中`);
  if (c("downloading")) parts.push(`${c("downloading")} 首下载中`);
  if (c("queued")) parts.push(`${c("queued")} 首排队`);
  if (c("waitLogin")) parts.push(`<span class="warn">${c("waitLogin")} 首待登录</span>`);
  if (c("failed")) parts.push(`<span class="bad">${c("failed")} 首失败</span>`);
  const qq = c("waitLogin") && S.snap?.qq.state !== "loggedIn" ? `<button class="btn primary" id="trayQQ">打开 QQ 音乐</button>` : "";
  const html = `<span class="tl">正在转换</span><span class="ts">${parts.join(" · ")}</span>${qq}`;
  const head = $("#trayHead");
  if (head.innerHTML !== html) head.innerHTML = html;
  $("#tray").classList.toggle("off", rows.size === 0 && !batch);
  requestAnimationFrame(fade);
}
const fade = () => $("#tasksWrap").classList.toggle("more", list.scrollHeight - list.scrollTop - list.clientHeight > 2);

function fill(r: Row, j: Job) {
  const row = $(".row", r.el), tile = $(".tile", row);
  const prev = row.dataset.state, L = look(j);
  if (r.job.state !== j.state) r.sp = 0;
  r.job = j;
  row.dataset.state = L;

  const name = nameOf(j);
  paintTile(tile, name, j.coverUrl, r.dev);

  // 歌名：没有标签歌名时用文件名解析的歌名（灰字）；拿到正式歌名时换场
  const t1 = $(".t1", row);
  t1.classList.toggle("parsed", !j.title);
  if (t1.textContent !== name) {
    const had = t1.textContent;
    t1.textContent = name;
    if (had && j.title) {
      t1.classList.remove("swap");
      void t1.offsetWidth;
      t1.classList.add("swap");
    }
  }
  const sub = L === "failed" ? `<span class="reason">${esc(j.error ?? "")}</span>`
    : !j.title ? (PLATFORM[j.platform] ? `<span style="color:var(--text-3)">${PLATFORM[j.platform]}</span>` : "")
    : [j.artists.join("、"), j.album ?? ""].filter(Boolean).map(esc).join('<span class="sep">·</span>');
  const t2 = $(".t2", row);
  if (t2.innerHTML !== sub) t2.innerHTML = sub;

  // 右列
  const fmt = j.quality ?? "";
  let st = "", mt = "", acts = "";
  switch (L) {
    case "downloading": st = "下载中"; break;
    case "queued": st = "排队中"; mt = [`第 ${j.queuePos ?? 1} 位`, fmt].filter(Boolean).join(" · "); break;
    case "processing": {
      const s = STEP[j.state];
      st = STEP_STATE[s];
      mt = `<span class="stepbar">${STEP_NAMES.map((n, i) => `<span class="${i < s ? "done" : i === s ? "on" : ""}" style="--sp:${Math.round(r.sp * 100)}%">${n}</span>`).join("")}</span>`;
      break;
    }
    case "done": st = "转换完成"; mt = [j.quality, j.hasCover && "封面", j.hasLyrics && "歌词"].filter(Boolean).join(" · "); break;
    case "waitQQ": st = "待登录"; mt = "登录 QQ 音乐后自动继续"; break;
    case "failed": acts = `<button class="btn" data-act="retry">${icon("retry")}重试</button><button class="btn" data-act="remove">移除</button>`; break;
  }
  const state = $(".state", row), meta = $(".meta", row), actions = $(".actions", row);
  state.innerHTML = st;
  meta.innerHTML = mt;
  actions.className = "actions " + (acts ? "always" : "");
  actions.innerHTML = acts;
  state.style.display = st ? "" : "none";
  meta.style.display = mt ? "" : "none";

  // 完成的那一刻：封面弹起 + 安全灯光圈
  if (L === "done" && prev !== "done") {
    tile.classList.remove("pop");
    row.classList.remove("just-done");
    void tile.offsetWidth;
    tile.classList.add("pop");
    row.classList.add("just-done");
  }
  r.sig = sig(j);
}

/** 封面：占位色块 + 字形；有 coverUrl 后淡入真实封面 */
function paintTile(tile: HTMLElement, name: string, cover: string | undefined, dev: number) {
  if (tile.dataset.name !== name) {
    tile.dataset.name = name;
    tile.setAttribute("style", `${artStyle(hashArt(name))};--dev:${dev}`);
    for (const g of tile.querySelectorAll(".glyph")) g.outerHTML = glyphHTML(name);
  }
  const imgs = tile.querySelectorAll("img");
  if (!cover) tile.classList.remove("has-cover");
  else if (imgs[0].getAttribute("src") !== cover) {
    tile.classList.remove("has-cover");
    imgs[1].onload = () => tile.classList.add("has-cover");
    imgs.forEach((i) => (i.src = cover));
  }
}

/* —— 格式转换批次：整批只占一行，封面跟着正在转的那首；完成约 3 秒后收起，部分失败等「好」 —— */
const BATCH_HTML = `<div><div class="row"><div class="tile">
    <div class="art latent"><span class="glyph"></span><img alt=""></div>
    <div class="art clear"><span class="glyph"></span><img alt=""></div>
    <div class="wash"></div><div class="overlay warn">${icon("warn")}</div>
  </div>
  <div class="info"><div class="t1"></div><div class="t2"></div></div>
  <div class="side"><div class="actions"></div><div class="state"></div><div class="meta"></div></div>
</div></div>`;
const VERB = { apple: "转为 Apple 无损", original: "转回 FLAC" };
let batch: HTMLLIElement | null = null, batchTs = "", dismissed = false, doneTimer = 0;

function renderBatch(rf?: Reformat) {
  if (!rf?.finished) dismissed = false;
  if (!rf || dismissed) return void (batch && dropBatch());
  if (!batch) {
    const el = (batch = document.createElement("li"));
    el.className = "row-wrap collapsed";
    el.innerHTML = BATCH_HTML;
    $("#batch").append(el);
    requestAnimationFrame(() => requestAnimationFrame(() => el.classList.remove("collapsed")));
  }
  const row = $(".row", batch), n = rf.failed.length;
  const L = !rf.finished ? "convert" : n ? "partial" : "done2";
  row.dataset.state = L;
  // 封面：转换中跟着正在转的那首；部分失败换成没转成的那首；完成时留着最后那首（没见过就用占位）
  const tile = $(".tile", row), fail = rf.failed[0]?.title;
  if (L === "partial") paintTile(tile, fail, S.lib.tracks.find((t) => t.title === fail)?.coverUrl, 1);
  else if (rf.current) paintTile(tile, rf.current, rf.coverUrl, 1);
  else if (!tile.dataset.name) paintTile(tile, VERB[rf.target], undefined, 1);
  $(".t1", row).textContent = VERB[rf.target];
  let sub = "", st = "", mt = "", acts = "";
  if (L === "convert") {
    const artists = S.lib.tracks.find((t) => t.title === rf.current)?.artists.join("、");
    sub = [rf.current, artists].filter(Boolean).map((s) => esc(s!)).join('<span class="sep">·</span>');
    st = `${Math.min(rf.done + 1, rf.total)} / ${rf.total}`;
    mt = `<span class="batchbar"><i style="--p:${((rf.done / Math.max(1, rf.total)) * 100).toFixed(1)}%"></i></span>`;
    batchTs = "格式转换" + (rf.etaSecs == null ? "" : ` · 剩余约 ${minutes(rf.etaSecs)} 分钟`);
  } else if (L === "done2") {
    sub = `${rf.total} 首歌曲 · 音质不变`;
    st = "已完成";
    batchTs = "格式转换已完成";
    doneTimer ||= window.setTimeout(dismissBatch, 3000);
  } else {
    sub = esc(n === 1 ? `${rf.failed[0].title}：${rf.failed[0].reason}` : `${rf.failed.map((f) => f.title).join("、")} 未转换`);
    acts = `<button class="btn" data-batch="ok">好</button>`;
    batchTs = `${rf.total - n} 首已完成 · <span class="warn">${n} 首未转换</span>`;
  }
  const set = (sel: string, html: string) => {
    const el = $(sel, row);
    if (el.innerHTML !== html) el.innerHTML = html;
    el.style.display = html ? "" : "none";
  };
  set(".t2", sub);
  set(".state", st);
  set(".meta", mt);
  set(".actions", acts);
  $(".actions", row).classList.toggle("always", !!acts);
}
function dismissBatch() {
  dismissed = true;
  call("dismiss_reformat");
  if (batch) dropBatch();
}
function dropBatch() {
  const el = batch!;
  batch = null;
  batchTs = "";
  clearTimeout(doneTimer);
  doneTimer = 0;
  el.classList.add("collapsed");
  afterMotion(el, reduced() ? 200 : 900, () => el.remove());
  chrome();
}

/* —— 入库仪式 —— */
async function startRitual(r: Row, j: Job) {
  r.ritual = true;
  fill(r, j);
  const out = j.output!, t0 = performance.now();
  if (toRowMode()) {
    landing.add(out);
    if (trackById(out)) renderLib();
  }
  await sleep(720);
  // 资料库里的这首可能还没到：等 library 事件，3 秒仍未到则只收起
  while (!trackById(out) && performance.now() - t0 < 3000) await sleep(100);
  if (!trackById(out) || !rows.has(j.id)) {
    if (landing.delete(out)) renderLib();
    return collapse(r);
  }
  const tile = $(".tile", r.el), img = $<HTMLImageElement>(".art.clear img", tile);
  // 起飞前封面还没到就用资料库里这首的封面；等它解码完，免得飞出去的是占位字形
  const cover = trackById(out)!.coverUrl;
  if (cover && !r.job.coverUrl) fill(r, { ...r.job, coverUrl: cover });
  if (img.getAttribute("src")) await Promise.race([img.decode().catch(() => {}), sleep(300)]);
  r.el.scrollIntoView({ block: "nearest" });
  const scroll = $("#scroll");
  let row: HTMLElement | undefined;
  if (toRowMode() && landing.has(out)) {
    scroll.scrollTo({ top: 0, behavior: "auto" });
    if (!rowEl(out)) renderLib();
    row = rowEl(out);
  }
  if (!row && landing.delete(out)) renderLib();
  const srcRect = tile.getBoundingClientRect();
  let dest: HTMLElement | null, destRect: DOMRect | undefined;
  if (row) {
    const c = $<HTMLElement>(".cover", row);
    dest = c;
    scroll.classList.add("notrans");
    row.classList.remove("landing");
    destRect = c.getBoundingClientRect();
    row.classList.add("landing");
    void row.offsetHeight;
    scroll.classList.remove("notrans");
    landing.delete(out);
    requestAnimationFrame(() => row!.classList.remove("landing"));
  } else {
    dest = document.getElementById("libCount");
    destRect = dest?.getBoundingClientRect();
  }
  if (destRect && !reduced()) {
    const fly = document.createElement("div");
    fly.className = "fly";
    fly.style.cssText = `${tile.getAttribute("style")};--gs:${srcRect.width}px;left:${srcRect.left}px;top:${srcRect.top}px;width:${srcRect.width}px;height:${srcRect.height}px`;
    fly.innerHTML = $(".art.clear", tile).outerHTML.replace("art clear", "art");
    document.body.appendChild(fly);
    tile.style.opacity = "0";
    const sc = destRect.width / srcRect.width, dx = destRect.left - srcRect.left, dy = destRect.top - srcRect.top;
    const soft = document.documentElement.style.getPropertyValue("--spring-soft") || "ease-out";
    const a = fly.animate(
      [{ transform: "translate(0,0) scale(1)", opacity: 1 }, { transform: `translate(${dx}px,${dy}px) scale(${sc})`, opacity: row ? 1 : 0 }],
      { duration: row ? 900 : 700, easing: row ? soft : "cubic-bezier(.3,.7,.3,1)", fill: "forwards" },
    );
    await Promise.race([a.finished.catch(() => {}), sleep(1200)]); // 隐藏窗口里动画可能不推进
    fly.remove();
  }
  if (row) {
    row.classList.remove("nocover");
    dest?.classList.add("pop");
    row.classList.add("just-done");
  }
  bumpCount();
  collapse(r);
}

// 显影进度：每帧把 --dev 逼近目标；处理中的步骤条同时缓动
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
