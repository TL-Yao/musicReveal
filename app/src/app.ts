// 界面外壳：侧栏、通知卡、引导三步（在内容面板里原位呈现）、拖放面、键盘，以及独立的设置窗口视图。
import type { Backend } from "./backend";
import type { Library, PlayerState, Snapshot } from "./types";
import { $, $$, baseName, esc, icon, springLinear, tilde } from "./dom";
import { S, call } from "./store";
import { closeMenu, initLibrary, renderVoid, setLibrary } from "./library";
import { initPlayer, lyricsOpen, refreshNowPlaying, setLyricsOpen, setPlayer, togglePlay } from "./player";
import { initTray, renderTray } from "./tray";

type Format = Snapshot["format"];
type FolderView = { path: string; name: string; accessible: boolean; reason?: string };

let firstTasks = true;

// 引导的选择只暂存在前端，第 3 步才提交
const ob = {
  step: 1,
  revealed: 0,
  extra: [] as { path: string; name: string }[],
  unchecked: new Set<string>(),
  outputDir: null as string | null,
  format: null as Format | null,
};
const onboarding = () => !S.snap!.onboarded;
const obFolders = () => ob.extra.map((f) => ({ ...f, checked: !ob.unchecked.has(f.path) }));
const outDir = () => (onboarding() ? (ob.outputDir ?? S.snap!.outputDir) : S.snap!.outputDir);
const format = () => (onboarding() ? (ob.format ?? S.snap!.format) : S.snap!.format);
const pickFolder = () => S.B.invoke<string | null>("pick_folder").catch(() => null);

function boot(backend: Backend) {
  S.B = backend;
  if ("__TAURI_INTERNALS__" in window) document.documentElement.classList.add("native");
  document.documentElement.style.setProperty("--spring", springLinear(210, 24)); // 轻微过冲
  document.documentElement.style.setProperty("--spring-soft", springLinear(170, 26)); // 临界阻尼
}
/** 先订阅事件，再取一次初值；事件先到就不用初值 */
function load<T>(on: (cb: (v: T) => void) => void, cmd: string, apply: (v: T) => void) {
  let got = false;
  on((v) => ((got = true), apply(v)));
  S.B.invoke<T>(cmd).then((v) => got || apply(v), (e) => console.error(cmd, e));
}

export function start(backend: Backend) {
  boot(backend);
  initLibrary();
  initPlayer();
  initTray();
  wire();
  load<Snapshot>(S.B.onSnapshot, "get_snapshot", (s) => ((S.snap = s), render()));
  load<Library>(S.B.onLibrary, "get_library", (l) => (setLibrary(l), refreshNowPlaying()));
  load<PlayerState>(S.B.onPlayer, "get_player", setPlayer);
  S.B.onDragDrop(onDrag);
}

function render() {
  if (!S.snap) return;
  const onb = onboarding();
  $("#win").classList.toggle("onboarding", onb);
  // 引导中压暗的部分只是看起来不能点：inert 让键盘也进不去
  for (const g of $$<HTMLElement>("[data-group]")) g.inert = g.classList.toggle("dormant", onb && (g.dataset.group === "lib" || ob.revealed < 1));
  for (const el of $$<HTMLElement>(".toolbar, .page-head, #scroll")) el.inert = onb;
  renderFolders();
  renderStatus();
  renderQQ();
  renderOutput();
  renderNotices();
  renderTray(onb ? [] : S.snap.jobs, firstTasks);
  firstTasks = false;
  renderStage();
  renderVoid(); // 空资料库的文案随监听文件夹数变
}

/* —— 侧栏 —— */
let folderSig = "";
let knownFolders: Set<string> | null = null;
function renderFolders() {
  const list: FolderView[] = onboarding()
    ? obFolders().filter((f) => f.checked).map((f) => ({ path: f.path, name: f.name, accessible: true }))
    : S.snap!.folders;
  const s = JSON.stringify(list);
  if (s === folderSig) return;
  folderSig = s;
  $("#folderList").innerHTML = list
    .map((f) => {
      const p = esc(f.path), fresh = knownFolders && !knownFolders.has(f.path);
      return `<div class="srow ${f.accessible ? "" : "bad"} ${fresh ? "added" : ""}" title="${p}">
      <span class="ico">${icon(f.accessible ? "folder" : "folder-warn")}</span>
      <span><div class="name">${esc(f.name)}</div><div class="path">${f.accessible ? esc(tilde(f.path)) : esc(f.reason ?? "无法访问")}</div></span>
      <span><button class="icon-btn rm" data-rm="${p}" title="不再监听">${icon("minus")}</button></span>
      ${f.accessible ? "" : `<span class="fix"><button class="btn" data-fix="${p}">重新选择…</button></span>`}
    </div>`;
    })
    .join("");
  knownFolders = new Set(list.map((f) => f.path));
}

function renderStatus() {
  const s = S.snap!;
  let html = "初始设置";
  if (!onboarding()) {
    const n = s.folders.filter((f) => f.accessible).length, bad = s.folders.length - n;
    html = n ? `<span class="live"></span>监听 ${n} 个文件夹` : "未监听文件夹";
    if (bad) html += ` · ${bad} 个无法访问`;
    const busy = s.jobs.filter((j) => j.state === "decrypt" || j.state === "transcode" || j.state === "metadata").length;
    if (busy) html += ` · ${busy} 首转换中`;
  }
  const el = $("#sbStatus");
  if (el.innerHTML !== html) el.innerHTML = html;
}

/* —— QQ 音乐：侧栏只在异常时出卡；引导第 3 步与设置窗口给完整状态 —— */
const QQ_KEY = { loggedIn: "ok", notInstalled: "notInstalled", loggedOut: "loggedOut", needsAuth: "needAuth", expired: "expired" } as const;
const QQ_CMD = { "打开 QQ 音乐": "open_qq_music", "授权…": "open_auth_settings" } as Record<string, string>;
const actBtn = (t?: string) => (t ? `<div class="acts"><button class="btn primary" data-cmd="${QQ_CMD[t]}">${t}</button></div>` : "");
function qqCardHTML() {
  const q = S.snap!.qq.state;
  if (q === "loggedIn" || onboarding()) return "";
  const m = {
    loggedOut: { name: "QQ 音乐未登录", desc: "登录后，待登录的歌曲会自动继续转换。", act: "打开 QQ 音乐" },
    expired: { name: "QQ 音乐登录已过期", desc: "重新登录后，待登录的歌曲会自动继续转换。", act: "打开 QQ 音乐" },
    notInstalled: { name: "未安装 QQ 音乐", desc: "解密 QQ 音乐下载的文件需要安装并登录 QQ 音乐；网易云音乐文件不受影响。" },
    needsAuth: { name: "QQ 音乐需要授权", desc: "读取 QQ 音乐登录信息需要授权。", act: "授权…", hint: "升级 QQ 音乐到最新版本可免授权。" },
  }[q] as { name: string; desc: string; act?: string; hint?: string };
  return `<div class="qqcard" data-qq="${QQ_KEY[q]}"><span class="dot"><i></i></span><span><div class="name">${m.name}</div><div class="desc">${m.desc}</div>${actBtn(m.act)}${m.hint ? `<div class="hint">${m.hint}</div>` : ""}</span></div>`;
}
function qqLineHTML(q: Snapshot["qq"]) {
  const m = {
    loggedIn: { name: "已登录", desc: "" },
    notInstalled: { name: "未安装", desc: "解密 QQ 音乐下载的文件需要安装并登录 QQ 音乐。" },
    loggedOut: { name: "未登录", desc: "登录 QQ 音乐后，待登录的歌曲自动继续。", act: "打开 QQ 音乐" },
    needsAuth: { name: "需要授权", desc: "读取 QQ 音乐登录信息需要授权。", act: "授权…", hint: "升级 QQ 音乐到最新版本可免授权。" },
    expired: { name: "登录已过期", desc: "重新登录 QQ 音乐后，待登录的歌曲自动继续。", act: "打开 QQ 音乐" },
  }[q.state] as { name: string; desc: string; act?: string; hint?: string };
  return `<span class="dot"><i></i></span><span>
    <div class="name">${m.name}${q.state === "loggedIn" && q.account ? ` <span class="masked">· QQ ${esc(q.account)}</span>` : ""}</div>
    <div class="desc">${m.desc || "用于解密 QQ 音乐下载的文件。网易云音乐文件无需登录。"}</div>
    ${actBtn(m.act)}${m.hint ? `<div class="hint">${m.hint}</div>` : ""}</span>`;
}
function renderQQ() {
  const card = qqCardHTML(), box = document.getElementById("qqAlert");
  if (box && box.dataset.sig !== card) (box.dataset.sig = card), (box.innerHTML = card);
  const html = qqLineHTML(S.snap!.qq), key = QQ_KEY[S.snap!.qq.state];
  for (const el of $$(".qq, .qqline")) {
    if (el.dataset.sig === html) continue;
    el.dataset.qq = key;
    el.dataset.sig = html;
    el.innerHTML = html;
  }
}

function renderOutput() {
  const dir = outDir(), on = format() === "mp3" ? "1" : "0";
  for (const box of $$(".ob-out, .pathbox")) {
    $(".name", box).textContent = baseName(dir);
    $(".path", box).textContent = tilde(dir);
  }
  for (const seg of $$(".seg")) {
    seg.dataset.on = on;
    $$("button", seg).forEach((b, i) => b.classList.toggle("on", String(i) === on));
  }
  $$<HTMLInputElement>(".radio input").forEach((r) => (r.checked = r.value === format()));
}

/* —— 通知卡：新文件夹里的旧歌 / 补转完成 —— */
function renderNotices() {
  const box = $("#notices"), list = onboarding() ? [] : S.snap!.notices;
  const ids = new Set(list.map((n) => n.id));
  for (const el of $$(".notice", box)) {
    if (ids.has(el.dataset.id!) || el.classList.contains("gone")) continue;
    el.classList.add("gone");
    el.addEventListener("animationend", () => el.remove(), { once: true });
  }
  for (const n of list) {
    const ask = n.kind === "existingSongs";
    const folder = n.folder ?? "";
    const fname = S.snap!.folders.find((f) => f.path === folder)?.name ?? baseName(folder);
    const title = ask ? `「${esc(fname)}」里有 ${n.count} 首还没转换的歌曲` : `已加入 ${n.count} 首关闭期间下载的歌曲`;
    const text = ask ? "转换后会加入资料库；忽略则只处理之后新下载的。" : "";
    let el = box.querySelector<HTMLElement>(`.notice[data-id="${CSS.escape(n.id)}"]:not(.gone)`);
    if (!el) {
      el = document.createElement("div");
      el.className = "notice" + (ask ? "" : " quiet");
      el.dataset.id = n.id;
      el.innerHTML = `<span class="ico">${icon(ask ? "folder" : "clock")}</span><span><h4></h4><p></p></span><span class="acts">${
        ask
          ? `<button class="btn primary" data-cmd="convert_existing">全部转换</button><button class="btn" data-cmd="ignore_existing">忽略</button>`
          : `<button class="btn" data-cmd="dismiss_notice">好</button>`
      }</span>`;
      el.addEventListener("click", (e) => {
        const cmd = (e.target as Element).closest<HTMLElement>("[data-cmd]")?.dataset.cmd;
        if (cmd) call(cmd, cmd === "dismiss_notice" ? { id: n.id } : { path: folder });
      });
      box.appendChild(el);
    }
    const h = $("h4", el);
    if (h.innerHTML !== title) h.innerHTML = title;
    $("p", el).textContent = text;
  }
}

/* —— 引导三步：在内容面板里原位变化 —— */
const dots = () => [1, 2, 3].map((i) => `<i class="${i === ob.step ? "on" : i < ob.step ? "done" : ""}"></i>`).join("");
function cardHTML(k: string) {
  if (k === "ob1")
    return `<div class="stage-card ob" data-k="ob1"><div class="steps">${dots()}</div>
      <h2>选择监听文件夹</h2><p class="lead">文件夹中新下载的歌曲将自动转换。也可稍后在侧栏添加。</p>
      <div class="box"></div>
      <div class="cta"><span></span><button class="btn primary lg" id="obNext">继续</button></div></div>`;
  if (k === "ob2")
    return `<div class="stage-card ob" data-k="ob2"><div class="steps">${dots()}</div>
      <h2>输出设置</h2><p class="lead">按「歌手 - 歌名」命名，写入封面、标签与 .lrc 歌词。源文件保持不变。</p>
      <div class="box">
        <div class="srow ob-out" role="button" title="选择输出文件夹…"><span class="ico">${icon("folder")}</span><span><div class="name"></div><div class="path"></div></span><span><button class="btn" id="obPick">选择…</button></span></div>
        <div class="ob-seg"><div class="lab">输出格式</div><div class="seg" id="obSeg" data-on="0"><div class="thumb"></div><button data-v="original">原格式</button><button data-v="mp3">MP3</button></div></div>
      </div>
      <p class="rule">原格式：保留无损音质。MP3：320 kbps，兼容性更好。</p>
      <div class="cta"><button class="btn quiet" id="obBack">上一步</button><button class="btn primary lg" id="obNext">继续</button></div></div>`;
  return `<div class="stage-card ob" data-k="ob3"><div class="steps">${dots()}</div>
      <h2>QQ 音乐账号</h2><p class="lead">用于解密 QQ 音乐新格式文件。网易云音乐文件无需登录。</p>
      <div class="box"><div class="qq"></div></div>
      <div class="cta"><button class="btn quiet" id="obBack">上一步</button><button class="btn primary lg" id="obNext">开始监听</button></div></div>`;
}
let boxSig = "";
function renderStage() {
  const st = $("#stage"), onb = onboarding();
  st.classList.toggle("off", !onb);
  st.inert = !onb; // 退场动画期间旧卡片还在，不能再被 Tab / Enter 触发
  if (!onb) return;
  const k = `ob${ob.step}`;
  if (st.querySelector<HTMLElement>(".stage-card")?.dataset.k !== k) {
    st.innerHTML = cardHTML(k);
    boxSig = "";
    renderOutput();
    renderQQ();
  }
  if (k === "ob1") {
    const fs = obFolders(), s = JSON.stringify(fs);
    if (s !== boxSig) {
      boxSig = s;
      $(".box", st).innerHTML =
        fs.map((f) => `<div class="srow" role="button" data-toggle="${esc(f.path)}"><span class="cb ${f.checked ? "" : "off"}">${icon("check")}</span><span><div class="name">${esc(f.name)}</div><div class="path">${esc(tilde(f.path))}</div></span><span></span></div>`).join("") +
        `<div class="srow" id="obAdd" role="button"><span class="ico" style="color:var(--text-2)">${icon("plus")}</span><span><div class="name" style="color:var(--text-2)">添加文件夹…</div></span><span></span></div>`;
    }
  }
}
function setObStep(n: number) {
  ob.step = Math.max(1, Math.min(3, n));
  ob.revealed = Math.max(ob.revealed, ob.step - 1);
  render();
}

/* —— 交互 —— */
async function addFolder() {
  const p = await pickFolder();
  if (!p) return;
  if (!onboarding()) return void call("add_watch_folder", { path: p });
  ob.unchecked.delete(p);
  if (!obFolders().some((f) => f.path === p)) ob.extra.push({ path: p, name: baseName(p) });
  render();
}
async function pickOutDir() {
  const p = await pickFolder();
  if (!p) return;
  if (onboarding()) (ob.outputDir = p), render();
  else call("set_output_dir", { path: p });
}
function setFormat(v: Format) {
  if (onboarding()) (ob.format = v), render();
  else call("set_format", { format: v });
}
async function finishOnboarding() {
  if (!onboarding()) return; // complete_onboarding 会重设监听文件夹，引导之外绝不能再发
  ob.revealed = 3;
  render();
  await call("complete_onboarding", {
    folders: obFolders().filter((f) => f.checked).map((f) => f.path),
    outputDir: outDir(),
    format: format(),
  });
}

function wire() {
  $("#addFolder").onclick = addFolder;
  $("#folderList").onclick = async (e) => {
    const t = e.target as Element;
    const rm = t.closest<HTMLElement>("[data-rm]")?.dataset.rm;
    const fix = t.closest<HTMLElement>("[data-fix]")?.dataset.fix;
    if (rm && onboarding()) ob.unchecked.add(rm), render();
    else if (rm) call("remove_watch_folder", { path: rm });
    if (fix) {
      const p = await pickFolder();
      if (!p) return;
      await call("remove_watch_folder", { path: fix });
      await call("add_watch_folder", { path: p });
    }
  };
  // 分段控件、QQ 卡片、引导舞台上的按钮都走委托
  document.addEventListener("click", (e) => {
    const t = e.target as Element;
    const seg = t.closest<HTMLElement>(".seg button");
    if (seg) return setFormat(seg.dataset.v as Format);
    const cmd = t.closest<HTMLElement>("#qqAlert [data-cmd], .qq [data-cmd]")?.dataset.cmd;
    if (cmd) return void call(cmd);
    if (!t.closest("#stage") || !onboarding()) return;
    if (t.closest("#obNext")) return ob.step < 3 ? setObStep(ob.step + 1) : void finishOnboarding();
    if (t.closest("#obBack")) return setObStep(ob.step - 1);
    if (t.closest("#obAdd")) return void addFolder();
    if (t.closest(".ob-out")) return void pickOutDir();
    const tog = t.closest<HTMLElement>("[data-toggle]")?.dataset.toggle;
    if (tog) {
      if (ob.unchecked.has(tog)) ob.unchecked.delete(tog);
      else ob.unchecked.add(tog);
      render();
    }
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      if ($("#menu").classList.contains("on")) return closeMenu();
      if (lyricsOpen()) setLyricsOpen(false);
    }
    if (e.metaKey && e.key === ",") return e.preventDefault(), void call("open_settings");
    if (S.snap && onboarding()) return;
    if ((e.target as Element).matches("input:not([type=range]), textarea")) return;
    // 焦点在按钮 / 单选上时空格归控件自己（原生激活）
    if (e.key === " " && !e.metaKey && !(e.target as Element).closest("button, a[href], input[type=radio], #menu")) {
      e.preventDefault();
      togglePlay();
    }
    if (e.metaKey && e.key === "ArrowRight") call("player_next");
    if (e.metaKey && e.key === "ArrowLeft") call("player_prev");
  });
}

/* —— 拖放：只说「N 个文件」，不列文件名 —— */
function onDrag(e: { type: string; paths?: string[] }) {
  const drop = $("#drop");
  if (!S.snap || onboarding()) return;
  if (e.type === "enter" && e.paths?.length) {
    $("#dropTitle").textContent = `松开以转换 ${e.paths.length} 个文件`;
    drop.classList.add("on");
  } else if (e.type === "drop") {
    drop.classList.remove("on");
    if (e.paths?.length) call("add_files", { paths: e.paths });
  } else if (e.type === "leave") drop.classList.remove("on");
}

/* ===================================================================
   设置窗口（index.html?view=settings）：输出文件夹、输出格式、QQ 音乐
   =================================================================== */
export function startSettings(backend: Backend) {
  boot(backend);
  $("#win").remove();
  const root = $("#settings");
  root.hidden = false;
  root.innerHTML = `<div class="form">
    <div class="frow"><label>输出文件夹</label><div><div class="field"><div class="pathbox"><span class="ico">${icon("folder")}</span><span><div class="name"></div><div class="path"></div></span></div><button class="btn" id="stPick">选择…</button></div><p class="help">转换好的歌曲按「歌手 - 歌名」命名写到这里；这个文件夹里的歌就是资料库。</p></div></div>
    <div class="frow"><label>输出格式</label><div>
      <label class="radio"><input type="radio" name="format" value="original"><i></i><span><div class="lab">原格式</div><div class="desc">保留无损音质，文件较大。</div></span></label>
      <label class="radio"><input type="radio" name="format" value="mp3"><i></i><span><div class="lab">MP3</div><div class="desc">320 kbps，兼容性更好。只影响之后转换的歌曲。</div></span></label></div></div>
    <div class="frow"><label>QQ 音乐</label><div class="qqline"></div></div>
  </div>`;
  root.addEventListener("click", async (e) => {
    const t = e.target as Element;
    if (t.closest("#stPick")) {
      const p = await pickFolder();
      if (p) call("set_output_dir", { path: p });
    }
    const cmd = t.closest<HTMLElement>("[data-cmd]")?.dataset.cmd;
    if (cmd) call(cmd);
  });
  root.addEventListener("change", (e) => {
    const r = e.target as HTMLInputElement;
    if (r.name === "format") call("set_format", { format: r.value });
  });
  load<Snapshot>(S.B.onSnapshot, "get_snapshot", (s) => {
    S.snap = { ...s, onboarded: true };
    renderOutput();
    renderQQ();
  });
}
