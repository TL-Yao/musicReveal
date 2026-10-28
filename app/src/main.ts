import "./styles.css";
import { tauriBackend } from "./backend";
import { start, startSettings } from "./app";

// ?view=settings：后台为 ⌘, 开的独立设置窗口，只渲染设置视图
const q = new URLSearchParams(location.search);
const run = q.get("view") === "settings" ? startSettings : start;
// 开发期浏览器预览：vite dev 且带 ?mock 时换成假后台；生产构建里这一支整段被裁掉
if (import.meta.env.DEV && q.has("mock")) import("./mock").then((m) => run(m.mockBackend));
else run(tauriBackend);
