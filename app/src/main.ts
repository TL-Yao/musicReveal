// 调试页：显示后台快照。正式界面见下一单。
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Snapshot } from "./types";

const out = document.querySelector<HTMLPreElement>("#out")!;
const show = (s: Snapshot) => (out.textContent = JSON.stringify(s, null, 2));

invoke<Snapshot>("get_snapshot").then(show, (e) => (out.textContent = String(e)));
listen<Snapshot>("snapshot", (e) => show(e.payload));

document.querySelector("#add")!.addEventListener("click", async () => {
  const path = await invoke<string | null>("pick_folder");
  if (path) await invoke("add_watch_folder", { path });
});
