// 播放条（走带 / LCD / 歌词开关 / 音量）与歌词视图。状态来自 `player` 事件，两次事件之间按时间插值进度。
import type { LyricLine, PlayerState, QueueView, Repeat, Track } from "./types";
import { $, $$, artStyle, esc, fmtTime, GREY, hashArt, icon, reduced, titleFromFile } from "./dom";
import { S, call } from "./store";
import { coverArt, syncRows, trackById, trash, visibleIds } from "./library";

const now = () => performance.now();
/** 播放器直接读文件，资料库可能还没读到它（扫描中 / 已移走）：先按文件名显示 */
const fileTrack = (id: string): Track =>
  trackById(id) ?? { id, title: titleFromFile(id.split("/").pop()!), artists: [], durationMs: 0, quality: "", hasLyrics: false, addedAt: 0 };
/** 正在播放的曲目 */
const cur = (): Track | undefined => {
  const id = S.player.trackId;
  if (!id) return;
  return trackById(id) ?? { ...fileTrack(id), durationMs: S.player.durationMs };
};
const dur = () => S.player.durationMs || cur()?.durationMs || 0;
/** 当前播放位置（ms）：上次事件的位置 + 之后流逝的时间 */
export const pos = () => {
  const p = S.player;
  return p.playing && p.trackId ? Math.min(dur(), p.positionMs + now() - S.playerAt) : p.positionMs;
};
let scrubbing: number | null = null; // 拖进度时的预览位置
let volHeld = false;

export function setPlayer(p: PlayerState) {
  const prev = S.player.trackId;
  S.player = p;
  S.playerAt = now();
  refreshNowPlaying();
  syncRows();
  if (p.trackId !== prev) {
    if (p.trackId && p.trackId === wantLyr) setLyricsOpen(true);
    else if (!p.trackId) setLyricsOpen(false);
    else if (lyrOpen) loadLyrics();
    wantLyr = undefined; // 换到的不是要看的那首（它打不开被跳过了）：意图作废
  }
  if (lyrOpen && qOn && qKey !== queueKey()) loadQueue();
  kick();
}
/** 资料库变化后：正在播放的歌的信息可能刚到或被删 */
export function refreshNowPlaying() {
  renderTransport();
  renderLCD();
  // 歌词视图按当时的 Track 画的：Track 到达、标签改了、.lrc 放进来或删掉（hasLyrics 变）都重读
  if (lyrOpen && lyrKey !== keyOf(S.player.trackId)) loadLyrics();
}
/** 把正在放的歌移到废纸篓；后台把它移出队列、接着放下一首 */
export const trashCurrent = () => void (S.player.trackId && trash(S.player.trackId));
export function togglePlay() {
  if (S.player.trackId) return void call("player_toggle");
  const ids = visibleIds();
  if (ids.length) call("player_play", { ids, index: 0 });
}

function renderTransport() {
  const p = S.player, has = !!p.trackId;
  const pb = $("#btnPlay");
  pb.innerHTML = icon(p.playing ? "pause" : "play");
  pb.title = p.playing ? "暂停" : "播放";
  $<HTMLButtonElement>("#btnPlay").disabled = !has && !S.lib.tracks.length;
  $("#btnShuffle").classList.toggle("on", p.shuffle);
  const rb = $("#btnRepeat");
  rb.classList.toggle("on", p.repeat !== "off");
  rb.innerHTML = icon(p.repeat === "one" ? "repeat1" : "repeat");
  rb.title = { off: "循环播放", all: "循环播放：全部", one: "循环播放：单曲" }[p.repeat];
  for (const id of ["#btnPrev", "#btnNext", "#lcdCover"]) $<HTMLButtonElement>(id).disabled = !has;
  $("#lcdCover").classList.toggle("on", lyrOpen);
  const vol = $<HTMLInputElement>("#volRange");
  if (!volHeld) vol.value = String(Math.round(p.volume * 100));
  vol.style.setProperty("--v", vol.value + "%");
  syncCtl();
}
/** 歌词页左栏的走带：与播放条同步 */
function syncCtl() {
  const pp = document.getElementById("lyrPlay");
  if (!pp) return;
  const p = S.player, title = p.playing ? "暂停" : "播放";
  if (pp.title !== title) (pp.innerHTML = icon(p.playing ? "pause" : "play")), (pp.title = title);
  $$<HTMLButtonElement>(".lyr-ctl button").forEach((b) => (b.disabled = !p.trackId));
  document.getElementById("lyrQueue")?.classList.toggle("lyr-paused", !p.playing);
}

let lcdSig = "";
function renderLCD() {
  const t = cur(), err = S.player.error;
  const sig = JSON.stringify([t, err]);
  if (sig === lcdSig) return void updateProgress();
  lcdSig = sig;
  const lcd = $("#lcd"), cover = $("#lcdCover");
  lcd.classList.toggle("idle", !t);
  cover.setAttribute("style", artStyle(t ? hashArt(t.title) : GREY));
  cover.innerHTML = t ? coverArt(t) + `<span class="ov">${icon("chev")}</span>` : `<div class="art"></div>`;
  $("#lcdT1").textContent = t ? t.title : err ? "无法播放" : "未在播放";
  $("#lcdT2").innerHTML = err
    ? `<span class="err">${esc(err)}</span>`
    : t ? `${esc(t.artists.join("、"))}${t.album ? `<span class="sep">—</span>${esc(t.album)}` : ""}` : "";
  $("#lcdT2").style.display = $("#lcdT2").innerHTML ? "" : "none";
  updateProgress();
}
function updateProgress() {
  if (!S.player.trackId) return;
  const d = dur(), at = scrubbing ?? pos(), p = d ? Math.min(1, at / d) : 0;
  $(".lcd .scrub i").style.setProperty("--p", (p * 100).toFixed(2) + "%");
  if (scrubbing === null) $<HTMLInputElement>("#scrub").value = String(Math.round(p * 1000));
  $("#timeL").textContent = fmtTime(at);
  $("#timeR").textContent = "-" + fmtTime(d - at);
  const fill = document.getElementById("lyrFill");
  if (!fill) return;
  fill.style.setProperty("--p", (p * 100).toFixed(2) + "%");
  if (scrubbing === null) $<HTMLInputElement>("#lyrScrub").value = String(Math.round(p * 1000));
  $("#lyrTimeL").textContent = fmtTime(at);
  $("#lyrTimeR").textContent = "-" + fmtTime(d - at);
}

// 播放中每帧推进进度与歌词
let raf = 0;
// 窗口隐藏（关窗即隐藏）时不跑帧；回来时按时间插值，进度自然对上
const kick = () => void (document.hidden || (raf ||= requestAnimationFrame(tick)));
document.addEventListener("visibilitychange", () => (document.hidden ? (cancelAnimationFrame(raf), (raf = 0)) : kick()));
function tick() {
  raf = 0;
  updateProgress();
  if (lyrOpen) syncLyrics(false);
  if (S.player.playing && S.player.trackId) kick();
}
const seek = (ms: number) => {
  S.player = { ...S.player, positionMs: ms };
  S.playerAt = now();
  call("player_seek", { ms: Math.round(ms) });
  updateProgress();
  syncLyrics(true);
};

/* ===================================================================
   歌词视图：模糊放大的封面做底、左封面右歌词；当前行显影，其余行留在潜影里
   =================================================================== */
const keyOf = (id?: string) => {
  const t = trackById(id);
  return t ? JSON.stringify([t.hasLyrics, t.title, t.artists, t.album, t.year, t.coverUrl]) : "";
};
let lyrOpen = false, lyrId: string | undefined, lyrKey = "", wantLyr: string | undefined, lines: LyricLine[] = [], lineEls: HTMLElement[] = [], curLine = -1;

export function setLyricsOpen(on: boolean) {
  wantLyr = undefined;
  lyrOpen = on && !!S.player.trackId;
  $("#lyrics").classList.toggle("on", lyrOpen);
  $("#lcdCover").classList.toggle("on", lyrOpen);
  if (lyrOpen) loadLyrics();
  if (lyrOpen && qOn) loadQueue();
}
export const lyricsOpen = () => lyrOpen;
/** 打开某首的歌词：已在放就直接开；否则 start() 去播，等播放器真换到这首再开（免得先闪出上一首的歌词） */
export function openLyricsFor(id: string, start: () => void) {
  if (S.player.trackId === id) return setLyricsOpen(true);
  start();
  wantLyr = id;
}

async function loadLyrics() {
  const id = S.player.trackId!;
  lyrId = id;
  lyrKey = keyOf(id); // 还没有 Track 时先按文件名占位，Track 到了再重载
  const L = await S.B.invoke<LyricLine[] | null>("get_lyrics", { id }).catch((e) => (console.error("get_lyrics", e), null));
  if (lyrId === id) renderLyrics(L ?? []);
}

function renderLyrics(L: LyricLine[]) {
  const t = cur(), box = $("#lyrics");
  lines = L;
  lineEls = [];
  curLine = -1;
  if (!t) return void (box.innerHTML = "");
  const art = artStyle(hashArt(t.title));
  const cover = `<div class="cover" style="${art}">${coverArt(t)}</div>`;
  const head = `<div class="lyr-bg" style="${art}">${t.coverUrl ? `<img src="${esc(t.coverUrl)}" alt="">` : ""}</div><div class="lyr-veil"></div>`;
  const ctl = `<div class="lyr-ctlbox"><div class="lyr-trk"><i id="lyrFill"></i><input type="range" min="0" max="1000" value="0" id="lyrScrub" aria-label="播放进度"></div>
    <div class="lyr-tm"><span id="lyrTimeL"></span><span id="lyrTimeR"></span></div>
    <div class="lyr-ctl"><button class="lyr-aux lyr-qbtn${qOn ? " on" : ""}" data-act="queue" title="${qOn ? "显示歌词" : "播放队列"}">${icon("queue")}</button><button class="lyr-sk" data-act="prev" title="上一首">${icon("prev")}</button><button class="lyr-pp" id="lyrPlay" data-act="play"></button><button class="lyr-sk" data-act="next" title="下一首">${icon("next")}</button><button class="lyr-aux lyr-del" data-act="trash" title="移到废纸篓">${icon("trash")}</button></div></div>`;
  if (!L.length && !qOn) {
    box.innerHTML = head + `<div class="lyr-none"><div>${cover}<h3>${esc(t.title)}</h3><p>${esc(t.artists.join("、"))}</p><p class="sub">${lyrKey || !S.lib.scanning ? "这首歌没有歌词" : "正在读取资料库…"}</p>${ctl}</div></div>`;
  } else {
    const meta = [t.album, t.year].filter(Boolean).join(" · ");
    const right = qOn
      ? `<div class="lyr-q" id="lyrQueue"></div>`
      : `<div class="lyr-lines" id="lyrLines"><div class="lyr-pad"></div>${L.map((l, i) => {
          const gap = !l.text.trim() || l.text.trim() === "…";
          return `<div class="ln${gap ? " gap" : ""}" data-i="${i}">${gap ? "• • •" : esc(l.text)}${l.trans ? `<span class="tr">${esc(l.trans)}</span>` : ""}</div>`;
        }).join("")}<div class="lyr-pad"></div></div>`;
    box.innerHTML = head + `<div class="lyr-grid"><div class="lyr-art">${cover}<div><h3>${esc(t.title)}</h3><p>${esc(t.artists.join("、"))}</p>${meta ? `<p class="meta">${esc(meta)}</p>` : ""}</div>${ctl}</div>${right}</div>`;
  }
  syncCtl();
  updateProgress();
  if (qOn) return renderQueue(true);
  lineEls = $$(".ln", box);
  layoutLyrics();
  syncLyrics(true, true);
}

/* 右半边的播放队列：实际播放顺序，当前首居中；queueRev 或当前曲变了才重新拉 */
let qOn = false, Q: QueueView = { ids: [], pos: 0 }, qKey = "";
const queueKey = () => `${S.player.queueRev}|${S.player.trackId ?? ""}`;
const EQ = `<div class="ov eq"><span class="eq"><i></i><i></i><i></i></span></div>`;
async function loadQueue() {
  const k = (qKey = queueKey());
  const v = await S.B.invoke<QueueView>("player_queue").catch((e) => (console.error("player_queue", e), null));
  if (v && k === qKey) (Q = v), renderQueue(false);
}
function renderQueue(instant: boolean) {
  const box = document.getElementById("lyrQueue");
  if (!box) return;
  box.innerHTML = `<div class="lyr-qpad"></div>${Q.ids.map((id, i) => {
    const t = fileTrack(id);
    return `${i === Q.pos + 1 ? `<h4 class="lyr-qh">接下来播放</h4>` : ""}<div class="lyr-qr${i < Q.pos ? " past" : i === Q.pos ? " cur" : ""}" data-i="${i}"><div class="cover" style="${artStyle(hashArt(t.title))}">${coverArt(t)}${i === Q.pos ? EQ : ""}</div><div><div class="lyr-qt">${esc(t.title)}</div><div class="lyr-qa">${esc(t.artists.join("、"))}</div></div><span class="lyr-qd">${t.durationMs ? fmtTime(t.durationMs) : ""}</span></div>`;
  }).join("")}<div class="lyr-qpad"></div>`;
  syncCtl();
  const c = box.querySelector<HTMLElement>(".lyr-qr.cur");
  if (c) box.scrollTo({ top: c.offsetTop - box.clientHeight / 2 + c.offsetHeight / 2, behavior: instant || reduced() ? "auto" : "smooth" });
}
function toggleQueue() {
  qOn = !qOn;
  renderLyrics(lines);
  if (qOn) loadQueue();
}
function layoutLyrics() {
  const c = document.getElementById("lyrLines");
  if (!c) return;
  const h = c.clientHeight;
  $$(".lyr-pad", c).forEach((p) => p.style.setProperty("--pad", Math.round(h * 0.42) + "px"));
}
function syncLyrics(force: boolean, instant = false) {
  if (!lineEls.length) return;
  const at = scrubbing ?? pos();
  let c = -1;
  for (let i = 0; i < lines.length; i++) if (lines[i].ms <= at) c = i;
  if (c === curLine && !force) return;
  curLine = c;
  lineEls.forEach((el, i) => {
    el.classList.toggle("cur", i === c);
    el.classList.toggle("past", i < c);
    el.style.setProperty("--bl", i === c ? "0px" : Math.min(1.1, Math.abs(i - c) * 0.4).toFixed(2) + "px");
  });
  const box = document.getElementById("lyrLines"), el = lineEls[Math.max(0, c)];
  if (box && el && lyrOpen) box.scrollTo({ top: el.offsetTop - box.clientHeight / 2 + el.offsetHeight / 2, behavior: instant || reduced() ? "auto" : "smooth" });
}

export function initPlayer() {
  $("#btnPlay").onclick = togglePlay;
  $("#btnNext").onclick = () => call("player_next");
  $("#btnPrev").onclick = () => call("player_prev");
  $("#btnShuffle").onclick = () => {
    const on = !S.player.shuffle;
    S.player = { ...S.player, shuffle: on };
    renderTransport();
    call("player_set_shuffle", { on });
  };
  $("#btnRepeat").onclick = () => {
    const mode: Repeat = ({ off: "all", all: "one", one: "off" } as const)[S.player.repeat];
    S.player = { ...S.player, repeat: mode };
    renderTransport();
    call("player_set_repeat", { mode });
  };
  $("#lcdCover").onclick = () => setLyricsOpen(!lyrOpen);

  const vol = $<HTMLInputElement>("#volRange");
  vol.addEventListener("pointerdown", () => (volHeld = true));
  for (const ev of ["pointerup", "change"]) vol.addEventListener(ev, () => (volHeld = false));
  vol.oninput = () => {
    const volume = +vol.value / 100;
    vol.style.setProperty("--v", vol.value + "%");
    S.player = { ...S.player, volume };
    call("player_set_volume", { volume });
  };

  // 拖动中只预览，松手才 seek；播放条与歌词页左栏各一条
  const scrubTo = (el: HTMLInputElement, done: boolean) => {
    const ms = (+el.value / 1000) * dur();
    if (done) return (scrubbing = null), seek(ms);
    scrubbing = ms;
    updateProgress();
    if (lyrOpen) syncLyrics(false);
  };
  const scrub = $<HTMLInputElement>("#scrub");
  scrub.oninput = () => scrubTo(scrub, false);
  scrub.onchange = () => scrubTo(scrub, true);
  for (const ev of ["input", "change"])
    $("#lyrics").addEventListener(ev, (e) => {
      const el = e.target as HTMLInputElement;
      if (el.id === "lyrScrub") scrubTo(el, ev === "change");
    });

  const acts: Record<string, () => void> = { prev: () => call("player_prev"), play: togglePlay, next: () => call("player_next"), queue: toggleQueue, trash: trashCurrent };
  $("#lyrics").addEventListener("click", (e) => {
    const el = e.target as Element, act = el.closest<HTMLElement>("[data-act]")?.dataset.act;
    if (act) return acts[act]();
    const qr = el.closest<HTMLElement>(".lyr-qr");
    if (qr) return void call("player_jump", { index: +qr.dataset.i! });
    const ln = el.closest<HTMLElement>(".ln");
    if (!ln) return;
    seek(lines[+ln.dataset.i!].ms);
    if (!S.player.playing) call("player_toggle");
  });
  window.addEventListener("resize", layoutLyrics);
  renderTransport();
  renderLCD();
}
