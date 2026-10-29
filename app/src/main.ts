import "./styles.css";
import { tauriBackend } from "./backend";
import { start, startSettings } from "./app";

// 被省略号截断的文字：悬停时用 title 显示全文，同 macOS 表格的截断提示。祖先已有自己的 title（如侧栏文件夹路径）时让给它
document.addEventListener("mouseover", (e) => {
  if ((e.target as Element).closest("[title]:not([data-tip])")) return;
  for (let el = e.target as HTMLElement; el && el !== document.body; el = el.parentElement!) {
    const s = getComputedStyle(el);
    if (s.textOverflow !== "ellipsis" && s.webkitLineClamp === "none") continue;
    if (el.scrollWidth > el.clientWidth || el.scrollHeight > el.clientHeight) (el.title = el.textContent!.trim()), (el.dataset.tip = "");
    else if ("tip" in el.dataset) el.removeAttribute("title"), delete el.dataset.tip;
    return;
  }
});

// ?view=settings：后台为 ⌘, 开的独立设置窗口，只渲染设置视图
const q = new URLSearchParams(location.search);
const run = q.get("view") === "settings" ? startSettings : start;
// 开发期浏览器预览：vite dev 且带 ?mock 时换成假后台；生产构建里这一支整段被裁掉
if (import.meta.env.DEV && q.has("mock")) import("./mock").then((m) => run(m.mockBackend));
else run(tauriBackend);
