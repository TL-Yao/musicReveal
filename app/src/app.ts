// 界面：侧栏三组、标题副行、舞台（引导 / 空状态）、通知卡、拖放面。数据全部来自后台快照。
import type { Backend } from "./backend";
import type { Snapshot } from "./types";
import { $, $$, baseName, esc, icon, springLinear, tilde } from "./dom";
import { flashError, initRows, renderTasks } from "./rows";

type Format = Snapshot["format"];
type FolderView = { path: string; name: string; accessible: boolean; reason?: string };

let B: Backend;
let snap: Snapshot | undefined;
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
const onboarding = () => !snap!.onboarded;
const obFolders = () =>
  [...snap!.suggestions.map((f) => ({ ...f, found: true })), ...ob.extra.map((f) => ({ ...f, found: false }))].map((f) => ({
    ...f,
    checked: !ob.unchecked.has(f.path),
  }));
const outDir = () => (onboarding() ? (ob.outputDir ?? snap!.outputDir) : snap!.outputDir);
const format = () => (onboarding() ? (ob.format ?? snap!.format) : snap!.format);

const call = (cmd: string, args?: Record<string, unknown>) => B.invoke(cmd, args).catch((e) => console.error(cmd, e));
const pickFolder = () => B.invoke<string | null>("pick_folder").catch(() => null);

export function start(backend: Backend) {
  B = backend;
  if ("__TAURI_INTERNALS__" in window) document.documentElement.classList.add("native");
  document.documentElement.style.setProperty("--spring", springLinear(210, 24)); // 轻微过冲
  document.documentElement.style.setProperty("--spring-soft", springLinear(170, 26)); // 临界阻尼
  initRows($("#tasks"), onRowAct);
  wire();
  let got = false;
  B.onSnapshot((s) => ((got = true), apply(s)));
  B.invoke<Snapshot>("get_snapshot").then((s) => got || apply(s), (e) => console.error("get_snapshot", e));
  B.onDragDrop(onDrag);
}

function apply(s: Snapshot) {
  snap = s;
  render();
}

function render() {
  if (!snap) return;
  const onb = onboarding();
  $("#win").classList.toggle("onboarding", onb);
  for (const g of $$(".group")) {
    const order = { folders: 1, output: 2, qq: 3 }[g.dataset.group as "folders"];
    g.classList.toggle("latent", onb && ob.revealed < order);
  }
  renderFolders();
  renderQQ();
  renderOutput();
  renderSubtitle();
  renderNotices();
  renderTasks(onb ? [] : snap.jobs, firstTasks);
  firstTasks = false;
  renderStage();
}

/* —— 侧栏 —— */
let folderSig = "";
let knownFolders: Set<string> | null = null;
function renderFolders() {
  const list: FolderView[] = onboarding()
    ? obFolders().filter((f) => f.checked).map((f) => ({ path: f.path, name: f.name, accessible: true }))
    : snap!.folders;
  const s = JSON.stringify(list);
  if (s === folderSig) return;
  folderSig = s;
  $("#folderList").innerHTML = list
    .map((f) => {
      const p = esc(f.path), fresh = knownFolders && !knownFolders.has(f.path);
      return `<div class="srow ${f.accessible ? "" : "bad"} ${fresh ? "added" : ""}" title="${p}">
      <span class="ico">${icon(f.accessible ? "folder" : "folder-warn")}</span>
      <span><div class="name">${esc(f.name)}</div><div class="path">${f.accessible ? esc(tilde(f.path)) : esc(f.reason ?? "无法访问")}</div></span>
      <span>${f.accessible ? `<button class="icon-btn rm" data-rm="${p}" title="移除">${icon("minus")}</button>` : ""}</span>
      ${f.accessible ? "" : `<span class="fix"><button class="btn" data-fix="${p}">重新选择…</button></span>`}
    </div>`;
    })
    .join("");
  knownFolders = new Set(list.map((f) => f.path));
}

const QQ_KEY = { loggedIn: "ok", notInstalled: "notInstalled", loggedOut: "loggedOut", needsAuth: "needAuth", expired: "expired" } as const;
function qqHTML() {
  const q = snap!.qq;
  const m = {
    loggedIn: { name: "已登录", desc: "", acts: [] as [string, string][], hint: "" },
    notInstalled: { name: "未安装", desc: "解密 QQ 音乐新格式文件需要安装并登录 QQ 音乐。", acts: [], hint: "" },
    loggedOut: { name: "未登录", desc: "登录 QQ 音乐后，待登录的歌曲将自动继续。", acts: [["打开 QQ 音乐", "open_qq_music"]], hint: "" },
    needsAuth: { name: "需要授权", desc: "读取 QQ 音乐登录信息需要授权。", acts: [["授权…", "open_auth_settings"]], hint: "升级 QQ 音乐至最新版本可免授权。" },
    expired: { name: "登录已过期", desc: "重新登录 QQ 音乐后，待登录的歌曲将自动继续。", acts: [["打开 QQ 音乐", "open_qq_music"]], hint: "" },
  }[q.state];
  return `<span class="dot"><i></i></span><span>
    <div class="name">${m.name}${q.state === "loggedIn" && q.account ? ` <span class="masked">· ${esc(q.account)}</span>` : ""}</div>
    ${m.desc ? `<div class="desc">${m.desc}</div>` : ""}
    ${m.acts.length ? `<div class="acts">${m.acts.map(([t, cmd]) => `<button class="btn primary" data-cmd="${cmd}">${t}</button>`).join("")}</div>` : ""}
    ${m.hint ? `<div class="hint">${m.hint}</div>` : ""}
  </span>`;
}
function renderQQ() {
  const html = qqHTML(), key = QQ_KEY[snap!.qq.state];
  for (const el of $$(".qq")) {
    if (el.dataset.qq === key && el.dataset.sig === html) continue;
    el.dataset.qq = key;
    el.dataset.sig = html;
    el.innerHTML = html;
  }
}

function renderOutput() {
  const dir = outDir(), on = format() === "mp3" ? "1" : "0";
  for (const [name, path] of [[$("#outName"), $("#outPath")], [$("#stage .ob-out .name"), $("#stage .ob-out .path")]]) {
    if (!name) continue;
    name.textContent = baseName(dir);
    path.textContent = tilde(dir);
  }
  for (const seg of $$(".seg")) {
    seg.dataset.on = on;
    $$("button", seg).forEach((b, i) => b.classList.toggle("on", String(i) === on));
  }
}

function renderSubtitle() {
  const s = snap!, p = $("#subtitle");
  let html = "初始设置";
  if (!onboarding()) {
    const n = s.folders.filter((f) => f.accessible).length, bad = s.folders.length - n;
    html = `<span class="live"></span>监听 ${n} 个文件夹`;
    if (bad) html += ` · ${bad} 个无法访问`;
    if (s.busy) html += ` · ${s.busy} 首处理中`;
    if (s.waitingLogin) html += ` · ${s.waitingLogin} 首待登录`;
  }
  if (p.innerHTML !== html) p.innerHTML = html;
}

/* —— 通知卡 —— */
function renderNotices() {
  const box = $("#notices"), list = onboarding() ? [] : snap!.notices;
  const ids = new Set(list.map((n) => n.id));
  for (const el of $$(".notice", box)) {
    if (ids.has(el.dataset.id!) || el.classList.contains("gone")) continue;
    el.classList.add("gone");
    el.addEventListener("animationend", () => el.remove(), { once: true });
  }
  for (const n of list) {
    const ask = n.kind === "existingSongs";
    const folder = n.folder ?? "";
    const fname = snap!.folders.find((f) => f.path === folder)?.name ?? baseName(folder);
    const title = ask ? `「${esc(fname)}」中有 ${n.count} 首未转换的歌曲` : `已加入 ${n.count} 首关闭期间下载的歌曲`;
    let el = box.querySelector<HTMLElement>(`.notice[data-id="${CSS.escape(n.id)}"]:not(.gone)`);
    if (!el) {
      el = document.createElement("div");
      el.className = "notice" + (ask ? "" : " quiet");
      el.dataset.id = n.id;
      el.innerHTML = `<span class="ico">${icon(ask ? "folder" : "clock")}</span><span><h4></h4></span><span class="acts">${
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
  }
}

/* —— 舞台：引导三步 / 空状态，在同一块版面里原位变化 —— */
const dots = () => [1, 2, 3].map((i) => `<i class="${i === ob.step ? "on" : i < ob.step ? "done" : ""}"></i>`).join("");
function cardHTML(k: string) {
  if (k === "ob1")
    return `<div class="stage-card" data-k="ob1"><div class="steps">${dots()}</div>
      <h2>选择监听文件夹</h2><p class="lead">文件夹中新下载的歌曲将自动转换。</p>
      <div class="box"></div>
      <div class="cta"><span></span><button class="btn primary lg" id="obNext">继续</button></div></div>`;
  if (k === "ob2")
    return `<div class="stage-card" data-k="ob2"><div class="steps">${dots()}</div>
      <h2>输出设置</h2><p class="lead">按「歌手 - 歌名」命名，写入封面、标签与 .lrc 歌词。源文件保持不变。</p>
      <div class="box">
        <div class="srow ob-out" role="button" title="选择输出文件夹…"><span class="ico">${icon("folder")}</span><span><div class="name"></div><div class="path"></div></span><span><button class="btn" id="obPick">选择…</button></span></div>
        <div class="ob-seg"><div class="lab">输出格式</div><div class="seg" id="obSeg" data-on="0"><div class="thumb"></div><button data-v="original">原格式</button><button data-v="mp3">MP3</button></div></div>
      </div>
      <p class="rule">原格式：保留无损音质。MP3：320 kbps，兼容性更好。</p>
      <div class="cta"><button class="btn quiet" id="obBack">上一步</button><button class="btn primary lg" id="obNext">继续</button></div></div>`;
  if (k === "ob3")
    return `<div class="stage-card" data-k="ob3"><div class="steps">${dots()}</div>
      <h2>QQ 音乐账号</h2><p class="lead">用于解密 QQ 音乐新格式文件。网易云音乐文件无需登录。</p>
      <div class="box"><div class="qq"></div></div>
      <div class="cta"><button class="btn quiet" id="obBack">上一步</button><button class="btn primary lg" id="obNext">开始监听</button></div></div>`;
  return `<div class="stage-card empty" data-k="empty">
      <div class="tile" style="--art-a:#55585F;--art-b:#1E2024;--art-c:#9AA0AA;--art-angle:150deg;--dev:0"><div class="art latent"></div><div class="art clear"></div><div class="dash"></div></div>
      <h2></h2><p>新下载的歌曲将自动转换，也可将文件拖到此处。</p></div>`;
}
let boxSig = "";
function renderStage() {
  const st = $("#stage"), onb = onboarding();
  const show = onb || (snap!.jobs.length === 0 && snap!.notices.length === 0);
  st.classList.toggle("off", !show);
  if (!show) return;
  const k = onb ? `ob${ob.step}` : "empty";
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
        fs
          .map(
            (f) => `<div class="srow" role="button" data-toggle="${esc(f.path)}"><span class="cb ${f.checked ? "" : "off"}">${icon("check")}</span><span><div class="name">${esc(f.name)}</div><div class="path">${esc(tilde(f.path))}</div></span><span class="found">${f.found ? "已检测到" : ""}</span></div>`,
          )
          .join("") +
        `<div class="srow" id="obAdd" role="button"><span class="ico" style="color:var(--text-2)">${icon("plus")}</span><span><div class="name" style="color:var(--text-2)">添加文件夹…</div></span><span></span></div>`;
    }
    $<HTMLButtonElement>("#obNext").disabled = !fs.some((f) => f.checked);
  }
  if (k === "empty") $("h2", st).textContent = `正在监听 ${snap!.folders.filter((f) => f.accessible).length} 个文件夹`;
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
  ob.revealed = 3;
  render();
  await call("complete_onboarding", {
    folders: obFolders().filter((f) => f.checked).map((f) => f.path),
    outputDir: outDir(),
    format: format(),
  });
}

function onRowAct(j: Snapshot["jobs"][number], a: string) {
  if (a === "play") {
    if (j.playing) return void call("stop_preview");
    B.invoke("play_preview", { id: j.id }).catch((e) => flashError(j.id, typeof e === "string" ? e : "无法试听"));
  } else if (a === "finder") call("reveal_in_finder", { id: j.id });
  else if (a === "retry") call("retry_job", { id: j.id });
  else if (a === "remove") call("remove_job", { id: j.id });
}

function wire() {
  $("#addFolder").onclick = addFolder;
  $("#outDir").onclick = pickOutDir;
  $("#addFiles").onclick = async () => {
    const paths = await B.pickFiles().catch(() => null);
    if (paths) call("add_files", { paths });
  };
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
  // 两处分段控件、两处 QQ 卡片、舞台上的按钮都走委托
  document.addEventListener("click", (e) => {
    const t = e.target as Element;
    const seg = t.closest<HTMLElement>(".seg button");
    if (seg) return setFormat(seg.dataset.v as Format);
    const cmd = t.closest<HTMLElement>(".qq [data-cmd]")?.dataset.cmd;
    if (cmd) return void call(cmd);
    if (!t.closest("#stage")) return;
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
}

/* —— 拖放 —— */
function onDrag(e: { type: string; paths?: string[] }) {
  const drop = $("#drop");
  if (!snap || onboarding()) return;
  if (e.type === "enter" && e.paths?.length) {
    $("#dropTitle").textContent = `松开以转换 ${e.paths.length} 个文件`;
    const names = e.paths.map(baseName);
    $("#dropFiles").innerHTML = names.slice(0, 4).map((n) => `<span>${esc(n)}</span>`).join("") + (names.length > 4 ? "<span>…</span>" : "");
    drop.classList.add("on");
  } else if (e.type === "drop") {
    drop.classList.remove("on");
    if (e.paths?.length) call("add_files", { paths: e.paths });
  } else if (e.type === "leave") drop.classList.remove("on");
}
