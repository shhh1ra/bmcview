import { Channel, invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

// ---------- типы IPC (app/src/main.rs, app/src/console.rs) ----------

interface AppInfo {
  version: string;
  core_version: string;
  os: string;
  arch: string;
}

interface Connected {
  fingerprint: string;
  newCertificate: boolean;
  unencrypted: boolean;
}

type ConnectError =
  | { kind: "badAddress" }
  | { kind: "certificateChanged"; expected: string; actual: string }
  | { kind: "failed"; message: string };

type UiEvent =
  | { type: "connected" }
  | { type: "reconnecting"; attempt: number }
  | { type: "closed"; reason: string }
  | { type: "leds"; value: number }
  | { type: "mouseMode"; absolute: boolean }
  | { type: "quality"; value: number };

type PowerAction = "forceOff" | "powerOn" | "coldReset" | "gracefulReboot" | "gracefulOff" | "usbReset";

type Input =
  | { type: "key"; code: string; down: boolean }
  | { type: "releaseKeys" }
  | { type: "ctrlAltDel" }
  | { type: "mouse"; x: number; y: number; buttons: number; wheel: number }
  | { type: "quality"; value: number }
  | { type: "power"; action: PowerAction }
  | { type: "refresh" };

// ---------- DOM ----------

function el<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`нет элемента #${id}`);
  return node as T;
}

const loginView = el("login-view");
const loginForm = el<HTMLFormElement>("login-form");
const addressInput = el<HTMLInputElement>("address");
const userInput = el<HTMLInputElement>("user");
const passwordInput = el<HTMLInputElement>("password");
const exclusiveInput = el<HTMLInputElement>("exclusive");
const connectButton = el<HTMLButtonElement>("connect");
const loginError = el("login-error");
const certChanged = el("cert-changed");

const consoleView = el("console-view");
const hostLabel = el("host");
const statusLabel = el("status");
const unencryptedBadge = el("unencrypted");
const qualitySelect = el<HTMLSelectElement>("quality");
const powerMenu = el("power-menu");
const canvas = el<HTMLCanvasElement>("screen");
const stageMessage = el("stage-message");
const ctx = canvas.getContext("2d", { alpha: false })!;

// ---------- недавние подключения (без паролей) ----------

interface Recent {
  address: string;
  user: string;
}
const RECENT_KEY = "bmcview.recent";

function loadRecent(): Recent[] {
  try {
    return JSON.parse(localStorage.getItem(RECENT_KEY) ?? "[]") as Recent[];
  } catch {
    return [];
  }
}

function saveRecent(entry: Recent): void {
  const list = [entry, ...loadRecent().filter((r) => r.address !== entry.address)].slice(0, 10);
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify(list));
  } catch {
    // хранилище недоступно — не критично
  }
}

function fillRecent(): void {
  const list = loadRecent();
  el("recent").replaceChildren(
    ...list.map((r) => {
      const option = document.createElement("option");
      option.value = r.address;
      return option;
    }),
  );
  if (list[0] && !addressInput.value) {
    addressInput.value = list[0].address;
    userInput.value = list[0].user;
  }
}

addressInput.addEventListener("change", () => {
  const match = loadRecent().find((r) => r.address === addressInput.value.trim());
  if (match) userInput.value = match.user;
});

// ---------- подключение ----------

let trustFingerprint: string | null = null;
let connected = false;

function showLoginError(text: string | null): void {
  loginError.hidden = text === null;
  loginError.textContent = text ?? "";
}

async function connect(): Promise<void> {
  showLoginError(null);
  certChanged.hidden = true;
  connectButton.disabled = true;
  connectButton.textContent = "Подключение…";

  const frames = new Channel<ArrayBuffer>();
  frames.onmessage = drawFrame;
  const events = new Channel<UiEvent>();
  events.onmessage = onEvent;

  const address = addressInput.value.trim();
  const user = userInput.value.trim();
  try {
    const result = await invoke<Connected>("connect", {
      address,
      user,
      password: passwordInput.value,
      exclusive: exclusiveInput.checked,
      trust: trustFingerprint,
      frames,
      events,
    });
    trustFingerprint = null;
    passwordInput.value = "";
    saveRecent({ address, user });
    openConsole(address, result);
  } catch (raw) {
    const err = raw as ConnectError;
    if (err.kind === "certificateChanged") {
      el("cert-expected").textContent = err.expected;
      el("cert-actual").textContent = err.actual;
      certChanged.hidden = false;
      trustFingerprint = err.actual;
    } else if (err.kind === "badAddress") {
      showLoginError("Адрес не распознан. Пример: 10.10.12.8 или bmc.local:443");
    } else {
      showLoginError(err.kind === "failed" ? err.message : String(raw));
    }
  } finally {
    connectButton.disabled = false;
    connectButton.textContent = "Подключиться";
  }
}

loginForm.addEventListener("submit", (e) => {
  e.preventDefault();
  trustFingerprint = null;
  void connect();
});

el("cert-trust").addEventListener("click", () => void connect());

function openConsole(address: string, result: Connected): void {
  connected = true;
  hostLabel.textContent = address;
  hostLabel.title = `Сертификат SHA-256: ${result.fingerprint}`;
  unencryptedBadge.hidden = !result.unencrypted;
  setStatus("подключение…");
  stageMessage.textContent = "Ожидание видео…";
  stageMessage.hidden = false;
  canvas.width = 0;
  canvas.height = 0;
  loginView.hidden = true;
  consoleView.hidden = false;
  canvas.focus();
}

function closeConsole(reason: string | null): void {
  connected = false;
  void invoke("disconnect");
  void getCurrentWindow().setFullscreen(false);
  consoleView.hidden = true;
  loginView.hidden = false;
  showLoginError(reason);
  passwordInput.focus();
}

function setStatus(text: string): void {
  statusLabel.textContent = text;
}

function send(input: Input): void {
  if (connected) void invoke("send_input", { input });
}

// ---------- события сессии ----------

function onEvent(event: UiEvent): void {
  switch (event.type) {
    case "connected":
      setStatus("на связи");
      break;
    case "reconnecting":
      setStatus(`переподключение (${event.attempt})…`);
      break;
    case "closed":
      closeConsole(`Сессия завершена: ${event.reason}`);
      break;
    case "leds":
      el("led-num").classList.toggle("on", (event.value & 1) !== 0);
      el("led-caps").classList.toggle("on", (event.value & 2) !== 0);
      el("led-scroll").classList.toggle("on", (event.value & 4) !== 0);
      break;
    case "mouseMode":
      canvas.classList.toggle("relative-mouse", !event.absolute);
      canvas.title = event.absolute ? "" : "BMC в режиме относительной мыши: курсор может не совпадать";
      break;
    case "quality":
      qualitySelect.value = String(event.value);
      break;
  }
}

// ---------- видео ----------

const BLOCK = 64;
const BLOCK_BYTES = BLOCK * BLOCK * 4;

/** Формат — `encode_frame` в app/src/console.rs. */
function drawFrame(buffer: ArrayBuffer): void {
  const view = new DataView(buffer);
  const width = view.getUint16(0, true);
  const height = view.getUint16(2, true);
  const count = view.getUint16(4, true);
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
  }
  stageMessage.hidden = true;
  let offset = 6;
  for (let i = 0; i < count; i++) {
    const x = view.getUint16(offset, true);
    const y = view.getUint16(offset + 2, true);
    offset += 4;
    const pixels = new Uint8ClampedArray(buffer, offset, BLOCK_BYTES);
    ctx.putImageData(new ImageData(pixels, BLOCK, BLOCK), x, y);
    offset += BLOCK_BYTES;
  }
}

// ---------- клавиатура ----------

function consoleHasKeyboard(): boolean {
  return connected && !consoleView.hidden && el("modal").hidden && document.activeElement === canvas;
}

window.addEventListener("keydown", (e) => {
  if (!consoleHasKeyboard()) return;
  e.preventDefault();
  if (!e.repeat) send({ type: "key", code: e.code, down: true });
});

window.addEventListener("keyup", (e) => {
  if (!consoleHasKeyboard()) return;
  e.preventDefault();
  send({ type: "key", code: e.code, down: false });
});

// Иначе клавиша, отпущенная вне окна, останется «нажатой» на сервере.
window.addEventListener("blur", () => send({ type: "releaseKeys" }));
canvas.addEventListener("blur", () => send({ type: "releaseKeys" }));

// ---------- мышь (абсолютный режим) ----------

let pointer = { x: 0, y: 0, buttons: 0 };
let pointerDirty = false;

function serverPoint(e: MouseEvent): { x: number; y: number } {
  const rect = canvas.getBoundingClientRect();
  const x = ((e.clientX - rect.left) / rect.width) * canvas.width;
  const y = ((e.clientY - rect.top) / rect.height) * canvas.height;
  return {
    x: Math.max(0, Math.min(canvas.width - 1, Math.floor(x))),
    y: Math.max(0, Math.min(canvas.height - 1, Math.floor(y))),
  };
}

function flushPointer(wheel = 0): void {
  pointerDirty = false;
  send({ type: "mouse", ...pointer, wheel });
}

function onPointer(e: MouseEvent, immediate: boolean): void {
  if (!canvas.width) return;
  // DOM `buttons`: 1 левая, 2 правая, 4 средняя — те же биты, что у BMC.
  pointer = { ...serverPoint(e), buttons: e.buttons & 7 };
  if (immediate) {
    flushPointer();
  } else if (!pointerDirty) {
    pointerDirty = true;
    requestAnimationFrame(() => {
      if (pointerDirty) flushPointer();
    });
  }
}

canvas.addEventListener("mousemove", (e) => onPointer(e, false));
canvas.addEventListener("mousedown", (e) => {
  canvas.focus();
  e.preventDefault();
  onPointer(e, true);
});
canvas.addEventListener("mouseup", (e) => onPointer(e, true));
canvas.addEventListener("contextmenu", (e) => e.preventDefault());
canvas.addEventListener(
  "wheel",
  (e) => {
    e.preventDefault();
    if (!canvas.width || e.deltaY === 0) return;
    pointer = { ...serverPoint(e), buttons: e.buttons & 7 };
    // Как в оригинале (Java `getWheelRotation`): положительное — прокрутка вниз.
    flushPointer(e.deltaY > 0 ? 1 : -1);
  },
  { passive: false },
);

// ---------- тулбар ----------

qualitySelect.addEventListener("change", () => {
  send({ type: "quality", value: Number(qualitySelect.value) });
  canvas.focus();
});

el("refresh").addEventListener("click", () => {
  send({ type: "refresh" });
  canvas.focus();
});

el("cad").addEventListener("click", () => {
  send({ type: "ctrlAltDel" });
  canvas.focus();
});

el("power").addEventListener("click", () => {
  powerMenu.hidden = !powerMenu.hidden;
});

document.addEventListener("click", (e) => {
  if (!(e.target as HTMLElement).closest(".menu")) powerMenu.hidden = true;
});

for (const item of powerMenu.querySelectorAll<HTMLButtonElement>("[data-power]")) {
  item.addEventListener("click", async () => {
    powerMenu.hidden = true;
    const action = item.dataset.power as PowerAction;
    if (await confirmDialog(`${item.textContent}: выполнить на сервере ${hostLabel.textContent}?`)) {
      send({ type: "power", action });
    }
    canvas.focus();
  });
}

el("fullscreen").addEventListener("click", async () => {
  const win = getCurrentWindow();
  await win.setFullscreen(!(await win.isFullscreen()));
  canvas.focus();
});

el("disconnect").addEventListener("click", () => closeConsole(null));

// ---------- подтверждение ----------

function confirmDialog(text: string): Promise<boolean> {
  const modal = el("modal");
  el("modal-text").textContent = text;
  modal.hidden = false;
  return new Promise((resolve) => {
    const done = (ok: boolean) => {
      modal.hidden = true;
      el("modal-ok").removeEventListener("click", yes);
      el("modal-cancel").removeEventListener("click", no);
      resolve(ok);
    };
    const yes = () => done(true);
    const no = () => done(false);
    el("modal-ok").addEventListener("click", yes);
    el("modal-cancel").addEventListener("click", no);
    el<HTMLButtonElement>("modal-cancel").focus();
  });
}

// ---------- старт ----------

async function showBuildInfo(): Promise<void> {
  const target = el("build-info");
  try {
    const info = await invoke<AppInfo>("app_info");
    target.textContent = `v${info.version} · ядро ${info.core_version} · ${info.os}/${info.arch}`;
  } catch (err) {
    // Вне Tauri (vite dev в обычном браузере) IPC нет — это нормально.
    target.textContent = `нет связи с ядром: ${String(err)}`;
  }
}

fillRecent();
(addressInput.value ? passwordInput : addressInput).focus();
void showBuildInfo();
