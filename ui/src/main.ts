import { invoke } from "@tauri-apps/api/core";

interface AppInfo {
  version: string;
  core_version: string;
  os: string;
  arch: string;
}

async function showBuildInfo(): Promise<void> {
  const el = document.getElementById("build-info");
  if (!el) return;
  try {
    const info = await invoke<AppInfo>("app_info");
    el.textContent = `v${info.version} · ядро ${info.core_version} · ${info.os}/${info.arch}`;
  } catch (err) {
    // Вне Tauri (vite dev в обычном браузере) IPC нет — это нормально.
    el.textContent = `нет связи с ядром: ${String(err)}`;
  }
}

void showBuildInfo();
