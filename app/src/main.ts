import "./styles.css";
import { tauriBackend } from "./backend";
import { start } from "./app";

// 开发期浏览器预览：vite dev 且带 ?mock 时换成假后台；生产构建里这一支整段被裁掉
if (import.meta.env.DEV && new URLSearchParams(location.search).has("mock")) import("./mock").then((m) => start(m.mockBackend));
else start(tauriBackend);
