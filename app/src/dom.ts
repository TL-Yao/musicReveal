export const $ = <T extends Element = HTMLElement>(s: string, el: ParentNode = document) => el.querySelector<T>(s)!;
export const $$ = <T extends Element = HTMLElement>(s: string, el: ParentNode = document) => [...el.querySelectorAll<T>(s)];
export const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]!);
export const icon = (n: string) => `<svg><use href="#i-${n}"/></svg>`;
export const reduced = () => matchMedia("(prefers-reduced-motion: reduce)").matches;
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
