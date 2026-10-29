// 设置窗口「输出格式」一行：Apple Music 兼容开关 → preview_format → 有歌要转就先确认（sheet）→ set_format；
// 批次进行中原位显示进度并锁住开关，结束后给出结果。
import type { FormatPreview, Reformat, Snapshot } from "./types";
import { $, esc, icon, minutes, switchHTML } from "./dom";
import { S } from "./store";
import appIcon from "../src-tauri/icons/icon.svg?url";

type Format = Snapshot["format"];
const VERB = { apple: "转为 Apple 无损", original: "转回 FLAC" };

export const FORMAT_ROW = `<div class="frow"><label>输出格式</label><div>
  <div class="fmt-head"><span class="fmt-name" id="fmtName">Apple Music 兼容</span>${switchHTML("fmtSwitch", "fmtName", "fmtLead")}</div>
  <p class="fmt-lead" id="fmtLead">存为「音乐」App 可以直接导入的格式。关闭时保留原格式。</p>
  <dl class="fmt-map"><dt>FLAC、WAV、APE</dt><dd>Apple 无损（ALAC），音质完全一样</dd><dt>OGG、Opus</dt><dd>AAC 256 kbps，有损转换，略有损失</dd><dt>MP3、M4A</dt><dd>保持不变</dd></dl>
  <div class="fmt-status" id="fmtStatus" aria-live="polite"></div></div></div>`;
const SHEET = `<div class="sheet-wrap"><div class="sheet" role="alertdialog" aria-modal="true" aria-labelledby="shTitle" aria-describedby="shText">
  <img class="icon" src="${appIcon}" alt=""><h3 id="shTitle"></h3><p id="shText"></p><p class="dim" id="shDim"></p>
  <div class="acts"><button class="btn lg" id="shCancel">取消</button><button class="btn primary lg" id="shOk"></button></div></div></div>`;

/** 已拨过去、还没在快照里落定的值（确认中 / 等后台） */
let want: Format | null = null;
let sheet: (FormatPreview & { to: Format }) | null = null;
let err = "";
/** 拨到另一边时要转换多少首：随当前格式与批次状态重取 */
let preview: FormatPreview | null = null;
let previewKey = "";
let statusKind = "";
let root: HTMLElement;

const running = () => !!S.snap?.reformat && !S.snap.reformat.finished;
const message = (e: unknown) => (typeof e === "string" ? e : (e as Error)?.message) || "无法更改输出格式";

export function initFormat(el: HTMLElement) {
  root = el;
  root.insertAdjacentHTML("beforeend", SHEET);
  $("#fmtSwitch").onclick = toggle;
  $("#shCancel").onclick = () => closeSheet(false);
  $("#shOk").onclick = () => closeSheet(true);
  document.addEventListener("keydown", (e) => {
    if (!sheet) return;
    if (e.key === "Escape") e.preventDefault(), closeSheet(false);
    else if (e.key === "Enter") e.preventDefault(), closeSheet(true);
    else if (e.key === "Tab") {
      // 焦点只在 sheet 的两个按钮之间转
      e.preventDefault();
      (document.activeElement === $("#shOk") ? $("#shCancel") : $("#shOk")).focus();
    }
  });
}

async function toggle() {
  if (running() || want || sheet) return;
  const to: Format = S.snap!.format === "apple" ? "original" : "apple";
  want = to;
  err = "";
  renderFormat();
  try {
    const p = await S.B.invoke<FormatPreview>("preview_format", { format: to });
    if (p.count > 0) return openSheet({ ...p, to });
  } catch (e) {
    return fail(e);
  }
  commit(to);
}
async function commit(to: Format) {
  try {
    await S.B.invoke("set_format", { format: to });
  } catch (e) {
    fail(e);
  }
}
function fail(e: unknown) {
  want = null;
  err = message(e);
  renderFormat();
}

function openSheet(s: FormatPreview & { to: Format }) {
  sheet = s;
  const on = s.to === "apple";
  $("#shTitle").textContent = on ? `将 ${s.count} 首无损歌曲转为 Apple 无损？` : `将 ${s.count} 首歌曲转回 FLAC？`;
  $("#shText").textContent = `资料库中的 ${on ? "FLAC、WAV 歌曲将转为 ALAC" : "Apple 无损歌曲将转回 FLAC"}，音质完全一样。约需 ${minutes(s.etaSecs)} 分钟，期间可照常播放。`;
  $("#shDim").textContent = on ? "已有的 OGG、Opus 歌曲保持不变。" : "由 OGG、Opus 转成的 AAC 歌曲保持不变。";
  $("#shOk").textContent = on ? "转换" : "转回 FLAC";
  root.classList.add("sheeted");
  $(".form", root).inert = true;
  $("#shOk").focus();
}
function closeSheet(ok: boolean) {
  const s = sheet!;
  sheet = null;
  root.classList.remove("sheeted");
  $(".form", root).inert = false;
  $("#fmtSwitch").focus();
  if (ok) return void commit(s.to);
  want = null; // 取消：开关弹回
  renderFormat();
}

export function renderFormat() {
  const s = S.snap!;
  if (want === s.format) want = null;
  const sw = $<HTMLButtonElement>("#fmtSwitch");
  sw.setAttribute("aria-checked", String((want ?? s.format) === "apple"));
  sw.disabled = running();
  const key = `${s.format}|${s.reformat?.finished ?? "-"}`;
  if (key !== previewKey) {
    previewKey = key;
    preview = null;
    S.B.invoke<FormatPreview>("preview_format", { format: s.format === "apple" ? "original" : "apple" }).then(
      (p) => previewKey === key && ((preview = p), renderStatus()),
      (e) => console.error("preview_format", e),
    );
  }
  renderStatus();
}

const pct = (rf: Reformat) => `${((rf.done / Math.max(1, rf.total)) * 100).toFixed(1)}%`;
const pos = (rf: Reformat) => `${Math.min(rf.done + 1, rf.total)} / ${rf.total}`;
const eta = (rf: Reformat) => (rf.etaSecs == null ? "" : `剩余约 ${minutes(rf.etaSecs)} 分钟`);

function progHTML(rf: Reformat) {
  return `<div class="prog"><div class="prog-top"><span>正在${VERB[rf.target]}…</span><span class="n">${pos(rf)}</span></div>
    <div class="bar" role="progressbar" aria-label="转换进度" aria-valuemin="0" aria-valuemax="${rf.total}" aria-valuenow="${rf.done}"><i style="--p:${pct(rf)}"></i></div>
    <div class="prog-sub"><span class="cur">${esc(rf.current ?? "")}</span><span class="eta">${eta(rf)}</span></div>
    <p class="prog-help">期间可照常播放，关闭此窗口不影响转换。完成前不能更改此设置。</p></div>`;
}
function resultHTML(rf: Reformat) {
  const n = rf.failed.length;
  if (!n)
    return `<div class="result"><span class="mk">${icon("check")}</span><span><div class="h">${rf.total} 首歌曲已${VERB[rf.target]}。</div><div class="sub">音质与原文件完全一样。</div></span></div>`;
  const was = rf.target === "apple" ? "仍为 FLAC" : "仍为 Apple 无损";
  return `<div class="result warn"><span class="mk">${icon("warn")}</span><span><div class="h">${rf.total - n} 首已${VERB[rf.target]}，${n} 首未转换。</div>
    <ul class="fails">${rf.failed.map((f) => `<li><span class="t">${esc(f.title)}</span><span class="k">${was}</span><span class="why">${esc(f.reason)}</span></li>`).join("")}</ul></span></div>`;
}
function noteHTML(f: Format, count: number) {
  const t = f === "apple"
    ? count ? "资料库里的无损歌曲已是 Apple 无损。关闭后会转回 FLAC，音质不变。" : "资料库里没有需要转回的歌曲，关闭后只影响之后转换的歌曲。"
    : count ? `打开后，资料库里已有的 ${count} 首无损歌曲也会转为 Apple 无损。OGG、Opus 歌曲不受影响。` : "资料库里没有需要转换的歌曲，打开后只影响之后转换的歌曲。";
  return `<p class="note">${t}</p>`;
}

function renderStatus() {
  const s = S.snap!, rf = s.reformat, box = $("#fmtStatus");
  let kind = "", html = "";
  if (err) (kind = "err"), (html = `<p class="note err">${esc(err)}</p>`);
  else if (rf && !rf.finished) (kind = "prog-" + rf.target), (html = progHTML(rf));
  else if (rf) (kind = "result"), (html = resultHTML(rf));
  else if (preview) (kind = "note"), (html = noteHTML(s.format, preview.count));
  // 进度只改数字、宽度与歌名，不重放入场
  if (rf && kind === statusKind && kind.startsWith("prog")) {
    $(".n", box).textContent = pos(rf);
    $(".bar", box).setAttribute("aria-valuenow", String(rf.done));
    $(".bar i", box).style.setProperty("--p", pct(rf));
    $(".cur", box).textContent = rf.current ?? "";
    $(".eta", box).textContent = eta(rf);
    return;
  }
  statusKind = kind;
  if (box.dataset.sig !== html) (box.dataset.sig = html), (box.innerHTML = html);
}
