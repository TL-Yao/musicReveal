export const $ = <T extends Element = HTMLElement>(s: string, el: ParentNode = document) => el.querySelector<T>(s)!;
export const $$ = <T extends Element = HTMLElement>(s: string, el: ParentNode = document) => [...el.querySelectorAll<T>(s)];
export const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]!);
export const icon = (n: string) => `<svg><use href="#i-${n}"/></svg>`;
export const reduced = () => matchMedia("(prefers-reduced-motion: reduce)").matches;
/** el 自己的退场动画 / 过渡结束后执行一次 fn。减少动态、窗口隐藏时事件可能永远不来：ms 后兜底 */
export function afterMotion(el: Element, ms: number, fn: () => void) {
  let done = false;
  const go = (e?: Event) => {
    if (done || (e && e.target !== el)) return;
    done = true;
    fn();
  };
  el.addEventListener("animationend", go);
  el.addEventListener("transitionend", go);
  setTimeout(go, ms);
}
/** 展示用：/Users/xx/Music → ~/Music */
export const tilde = (p: string) => p.replace(/^\/Users\/[^/]+(?=\/|$)/, "~");
export const baseName = (p: string) => p.replace(/\/+$/, "").split("/").pop() || p;

/** 设计稿的弹簧 → CSS linear() */
export function springLinear(stiffness: number, damping: number, mass = 1, steps = 48, dur = 1) {
  const w0 = Math.sqrt(stiffness / mass), zeta = damping / (2 * Math.sqrt(stiffness * mass));
  const pts = [];
  for (let i = 0; i <= steps; i++) {
    const t = (dur * i) / steps;
    let v;
    if (zeta < 1) {
      const wd = w0 * Math.sqrt(1 - zeta * zeta);
      v = 1 - Math.exp(-zeta * w0 * t) * (Math.cos(wd * t) + ((zeta * w0) / wd) * Math.sin(wd * t));
    } else v = 1 - Math.exp(-w0 * t) * (1 + w0 * t);
    pts.push(`${v.toFixed(4)} ${((100 * i) / steps).toFixed(1)}%`);
  }
  return `linear(${pts.join(", ")})`;
}
export const fmtTime = (ms: number) => {
  const s = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
};
export const frames = (n = 2) => new Promise<void>((r) => { const f = () => (--n ? requestAnimationFrame(f) : r()); requestAnimationFrame(f); });

// 设计稿 ART 的 9 套配色：没有封面时按名字哈希稳定取一套作底
export const ART = [
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
export const GREY = ART[5];
export function hashArt(s: string) {
  let h = 0;
  for (const ch of s) h = (Math.imul(h, 31) + ch.codePointAt(0)!) | 0;
  return ART[Math.abs(h) % ART.length];
}
export const glyphOf = (s: string) => s.match(/[\p{L}\p{N}]/u)?.[0] ?? "?";
export const glyphHTML = (s: string) => {
  const g = glyphOf(s);
  return `<span class="glyph${/^[A-Za-z]/.test(g) ? " latin" : ""}">${esc(g)}</span>`;
};
export const artStyle = (a: (typeof ART)[number]) => `--art-a:${a.a};--art-b:${a.b};--art-c:${a.c};--art-angle:${a.angle}`;

/**
 * 界面不显示文件名：没有歌名时用文件名解析出的歌名。
 * 去扩展名；含 " - " 取第一个之后；否则含 "-" 取第一个之后；去空白；空则用去扩展名原名。
 */
export function titleFromFile(fileName: string) {
  const stem = fileName.replace(/\.[^.]*$/, "");
  const i = stem.indexOf(" - "), j = stem.indexOf("-");
  const t = (i >= 0 ? stem.slice(i + 3) : j >= 0 ? stem.slice(j + 1) : stem).trim();
  return t || stem.trim();
}
