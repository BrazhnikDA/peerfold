import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

type Section = "folders" | "devices" | "history";
type ShareStatus = "idle" | "syncing" | "paused";

type Peer = {
  deviceId: string;
  name: string;
  online: boolean;
  trusted: boolean;
  mutual: boolean;
  pairingCode: string;
  waiting: boolean;
};

type Share = {
  id: string;
  name: string;
  path: string;
  fileCount: number;
  status: ShareStatus;
};

type Incoming = {
  deviceId: string;
  deviceName: string;
  shareId: string;
  name: string;
};

type Activity = {
  at: number;
  text: string;
};

type Snapshot = {
  deviceId: string;
  deviceName: string;
  paused: boolean;
  port: number;
  peers: Peer[];
  shares: Share[];
  incoming: Incoming[];
  activity: Activity[];
};

type VersionInfo = {
  relPath: string;
  hash: string;
  size: number;
  mtimeNs: number;
  deviceId: string;
  createdAt: number;
};

const NAV: { id: Section; label: string }[] = [
  { id: "folders", label: "Папки" },
  { id: "devices", label: "Устройства" },
  { id: "history", label: "История" },
];

const previewMode = window.__TAURI_INTERNALS__ == null;
const timeFormat = new Intl.DateTimeFormat("ru-RU", {
  day: "numeric",
  month: "short",
  year: "numeric",
  hour: "2-digit",
  minute: "2-digit",
});

const nowSeconds = Math.floor(Date.now() / 1000);
const fixtureShareId = "share-notes";

const fixtureVersions: VersionInfo[] = [
  {
    relPath: "readme.md",
    hash: "a31f0c8b2e44",
    size: 18432,
    mtimeNs: (nowSeconds - 1200) * 1_000_000,
    deviceId: "b7e21a90c3d4",
    createdAt: nowSeconds - 1200,
  },
  {
    relPath: "главы/первая.txt",
    hash: "09cd44e1aa77",
    size: 2_400_000,
    mtimeNs: (nowSeconds - 10_000) * 1_000_000,
    deviceId: "91ab34dc7701",
    createdAt: nowSeconds - 10_000,
  },
  {
    relPath: "images/cover.png",
    hash: "77b01de4c902",
    size: 512_000,
    mtimeNs: (nowSeconds - 86_400) * 1_000_000,
    deviceId: "4f8a1c2b9d10",
    createdAt: nowSeconds - 86_400,
  },
];

function emptySnapshot(): Snapshot {
  return {
    deviceId: "",
    deviceName: "",
    paused: false,
    port: 0,
    peers: [],
    shares: [],
    incoming: [],
    activity: [],
  };
}

function fixtureSnapshot(): Snapshot {
  return {
    deviceId: "4f8a1c2b9d10",
    deviceName: "Домашний",
    paused: false,
    port: 53317,
    peers: [
      {
        deviceId: "b7e21a90c3d4",
        name: "Кабинет",
        online: true,
        trusted: true,
        mutual: true,
        pairingCode: "482913",
        waiting: false,
      },
      {
        deviceId: "91ab34dc7701",
        name: "Ноутбук",
        online: false,
        trusted: true,
        mutual: false,
        pairingCode: "105774",
        waiting: true,
      },
      {
        deviceId: "aa11bb22cc33",
        name: "Гостевой",
        online: true,
        trusted: false,
        mutual: false,
        pairingCode: "330198",
        waiting: false,
      },
    ],
    shares: [
      {
        id: fixtureShareId,
        name: "Заметки",
        path: "/home/user/Документы/заметки",
        fileCount: 18,
        status: "idle",
      },
    ],
    incoming: [
      {
        deviceId: "de44c0a91827",
        deviceName: "Мастерская",
        shareId: "share-drawings",
        name: "Чертежи",
      },
    ],
    activity: [
      { at: nowSeconds - 120, text: "Кабинет обновил файлы в\u00A0папке «Заметки»" },
      { at: nowSeconds - 2400, text: "Ноутбук появился в\u00A0сети" },
      { at: nowSeconds - 86_400, text: "Папка «Заметки» на связи" },
    ],
  };
}

let snapshot: Snapshot = previewMode ? fixtureSnapshot() : emptySnapshot();
let versions: VersionInfo[] = previewMode ? fixtureVersions.map((item) => ({ ...item })) : [];
let versionsShareId: string | null = previewMode ? fixtureShareId : null;
let section: Section = "folders";
let selectedShareId: string | null = previewMode ? fixtureShareId : null;
let error = "";
let busy = false;
let booted = previewMode;
let nameDraft: string | null = null;
let nameQueued = false;
let committingName = false;
let seen = 0;

function pickerError(err: unknown): string {
  const text = errorText(err);
  if (text.includes("__TAURI_INTERNALS__") || text.includes("reading 'invoke'")) {
    return "Не удалось открыть выбор папки.";
  }
  return text;
}

function errorText(err: unknown): string {
  if (typeof err === "string" && err.trim()) return err.trim();
  if (err instanceof Error && err.message.trim()) return err.message.trim();
  if (err && typeof err === "object" && "message" in err) {
    const message = err.message;
    if (typeof message === "string" && message.trim()) return message.trim();
  }
  return "Не удалось выполнить команду.";
}

function toUnixSeconds(value: number): number {
  if (!Number.isFinite(value)) return 0;
  if (value > 1e14) return Math.floor(value / 1e9);
  if (value > 1e11) return Math.floor(value / 1e3);
  return Math.floor(value);
}

function formatTime(value: number): string {
  return timeFormat.format(new Date(toUnixSeconds(value) * 1000));
}

function formatSize(bytes: number): string {
  const safe = Number.isFinite(bytes) ? Math.max(0, bytes) : 0;
  const mb = safe / (1024 * 1024);
  const value = mb >= 1 ? mb : safe / 1024;
  const unit = mb >= 1 ? "МБ" : "КБ";
  const digits = value >= 10 || Number.isInteger(value) ? 0 : 1;
  const formatted = new Intl.NumberFormat("ru-RU", {
    minimumFractionDigits: 0,
    maximumFractionDigits: digits,
  }).format(value);
  return `${formatted}\u00A0${unit}`;
}

function formatFileCount(count: number): string {
  const n = Math.abs(Math.trunc(count));
  const mod10 = n % 10;
  const mod100 = n % 100;
  let word = "файлов";
  if (mod100 < 11 || mod100 > 14) {
    if (mod10 === 1) word = "файл";
    else if (mod10 >= 2 && mod10 <= 4) word = "файла";
  }
  return `${new Intl.NumberFormat("ru-RU").format(n)}\u00A0${word}`;
}

function formatCode(code: string): string {
  const digits = code.replace(/\D/g, "");
  if (digits.length === 6) return `${digits.slice(0, 3)}\u202F${digits.slice(3)}`;
  return code;
}

function shortId(deviceId: string): string {
  const compact = deviceId.replace(/[^a-zA-Z0-9]/g, "");
  return (compact || deviceId).slice(0, 6);
}

function statusLabel(status: string): string {
  if (status === "syncing") return "синхронизация";
  if (status === "paused") return "пауза";
  return "спокойно";
}

function folderName(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

function selectedShare(): Share | null {
  if (snapshot.shares.length === 0) return null;
  return snapshot.shares.find((share) => share.id === selectedShareId) ?? snapshot.shares[0];
}

function orderedPeers(): Peer[] {
  return snapshot.peers
    .map((peer, index) => ({ peer, index }))
    .sort((a, b) => {
      if (a.peer.waiting !== b.peer.waiting) return a.peer.waiting ? -1 : 1;
      return a.index - b.index;
    })
    .map((item) => item.peer);
}

function orderedActivity(): Activity[] {
  return [...snapshot.activity].sort((a, b) => b.at - a.at);
}

function orderedVersions(): VersionInfo[] {
  return [...versions].sort((a, b) => toUnixSeconds(b.createdAt) - toUnixSeconds(a.createdAt));
}

function isWindows(): boolean {
  return navigator.userAgent.includes("Windows");
}

async function pickFolder(): Promise<string | null> {
  const picked = isWindows()
    ? await invoke<string | null>("choose_folder")
    : await open({ directory: true, multiple: false });
  if (typeof picked !== "string") return null;
  const path = picked.trim();
  return path.length > 0 ? path : null;
}

async function pull(): Promise<void> {
  const id = ++seen;
  const next = await invoke<Snapshot>("state");
  if (id !== seen) return;
  snapshot = next;
  booted = true;
  if (section === "history") await loadVersions(id);
}

async function loadVersions(epoch = ++seen): Promise<void> {
  const share = selectedShare();
  if (!share) {
    if (epoch !== seen) return;
    versions = [];
    versionsShareId = null;
    return;
  }
  const list = previewMode
    ? share.id === fixtureShareId
      ? fixtureVersions.map((item) => ({ ...item }))
      : []
    : await invoke<VersionInfo[]>("versions", { shareId: share.id });
  if (epoch !== seen) return;
  versions = list;
  versionsShareId = share.id;
}

async function refresh(): Promise<void> {
  try {
    if (!previewMode) await pull();
  } catch (err) {
    error = errorText(err);
    booted = true;
  }
  render();
}

async function run(task: () => Promise<void>): Promise<boolean> {
  if (busy) return false;
  busy = true;
  render();
  try {
    await task();
    if (!previewMode) await pull();
    error = "";
    return true;
  } catch (err) {
    error = errorText(err);
    return false;
  } finally {
    busy = false;
    render();
    if (nameQueued && !committingName) {
      nameQueued = false;
      void commitName();
    }
  }
}

function noteActivity(text: string): void {
  snapshot = {
    ...snapshot,
    activity: [{ at: Math.floor(Date.now() / 1000), text }, ...snapshot.activity].slice(0, 12),
  };
}

async function commitName(): Promise<void> {
  if (!booted || committingName) return;
  const next = (nameDraft ?? snapshot.deviceName).trim();
  if (!next || next === snapshot.deviceName) {
    if (nameDraft !== null && !busy) {
      nameDraft = null;
      render();
    }
    return;
  }
  if (busy) {
    nameQueued = true;
    return;
  }
  committingName = true;
  try {
    await run(async () => {
      if (previewMode) snapshot = { ...snapshot, deviceName: next };
      else await invoke("set_name", { name: next });
      if ((nameDraft ?? next).trim() === next) nameDraft = null;
    });
  } finally {
    committingName = false;
  }
  if (nameQueued) {
    nameQueued = false;
    void commitName();
  }
}

async function togglePaused(): Promise<void> {
  const paused = !snapshot.paused;
  await run(async () => {
    if (previewMode) {
      snapshot = { ...snapshot, paused };
      return;
    }
    await invoke("set_paused", { paused });
  });
}

async function trustPeer(deviceId: string): Promise<void> {
  await run(async () => {
    if (previewMode) {
      snapshot = {
        ...snapshot,
        peers: snapshot.peers.map((peer) =>
          peer.deviceId === deviceId ? { ...peer, trusted: true, waiting: false } : peer,
        ),
      };
      return;
    }
    await invoke("trust", { deviceId });
  });
}

async function rejectPeer(deviceId: string): Promise<void> {
  await run(async () => {
    if (previewMode) {
      snapshot = {
        ...snapshot,
        peers: snapshot.peers.filter((peer) => peer.deviceId !== deviceId),
      };
      return;
    }
    await invoke("reject", { deviceId });
  });
}

async function forgetPeer(deviceId: string): Promise<void> {
  await run(async () => {
    if (previewMode) {
      snapshot = {
        ...snapshot,
        peers: snapshot.peers.filter((peer) => peer.deviceId !== deviceId),
      };
      return;
    }
    await invoke("forget", { deviceId });
  });
}

async function chooseAndCreateShare(): Promise<void> {
  let path: string | null = null;
  try {
    path = await pickFolder();
  } catch (err) {
    error = pickerError(err);
    render();
    return;
  }
  if (!path) return;
  const chosen = path;
  await run(async () => {
    if (previewMode) {
      snapshot = {
        ...snapshot,
        shares: [
          ...snapshot.shares,
          {
            id: `share-${Date.now()}`,
            name: folderName(chosen),
            path: chosen,
            fileCount: 0,
            status: "idle",
          },
        ],
      };
      noteActivity(`Папка «${folderName(chosen)}» добавлена`);
      return;
    }
    await invoke("create_share", { path: chosen });
  });
}

async function attachIncoming(offer: Incoming): Promise<void> {
  let path: string | null = null;
  try {
    path = await pickFolder();
  } catch (err) {
    error = pickerError(err);
    render();
    return;
  }
  if (!path) return;
  const chosen = path;
  await run(async () => {
    if (previewMode) {
      snapshot = {
        ...snapshot,
        incoming: snapshot.incoming.filter((item) => item.shareId !== offer.shareId),
        shares: [
          ...snapshot.shares,
          {
            id: offer.shareId,
            name: offer.name,
            path: chosen,
            fileCount: 0,
            status: "idle",
          },
        ],
      };
      noteActivity(`${offer.deviceName} подключил папку «${offer.name}»`);
      return;
    }
    await invoke("attach_share", { shareId: offer.shareId, path: chosen });
  });
}

async function removeShare(share: Share): Promise<void> {
  await run(async () => {
    if (previewMode) {
      snapshot = {
        ...snapshot,
        shares: snapshot.shares.filter((item) => item.id !== share.id),
      };
      if (selectedShareId === share.id) selectedShareId = snapshot.shares[0]?.id ?? null;
      return;
    }
    await invoke("remove_share", { shareId: share.id });
  });
}

async function rescanShare(share: Share): Promise<void> {
  await run(async () => {
    if (previewMode) {
      snapshot = {
        ...snapshot,
        shares: snapshot.shares.map((item) =>
          item.id === share.id ? { ...item, status: "syncing" } : item,
        ),
      };
      noteActivity(`Проверка папки «${share.name}»`);
      return;
    }
    await invoke("rescan", { shareId: share.id });
  });
}

async function restoreVersion(shareId: string, version: VersionInfo): Promise<void> {
  await run(async () => {
    if (previewMode) {
      noteActivity(`Возвращена версия ${version.relPath}`);
      return;
    }
    await invoke("restore", {
      shareId,
      relPath: version.relPath,
      hash: version.hash,
    });
  });
}

function selectSection(next: Section): void {
  if (section === next) return;
  section = next;
  render();
  if (next !== "history") return;
  const epoch = ++seen;
  void loadVersions(epoch)
    .then(() => {
      if (epoch === seen) render();
    })
    .catch((err: unknown) => {
      if (epoch !== seen) return;
      error = errorText(err);
      render();
    });
}

function selectHistoryShare(shareId: string): void {
  if (selectedShareId === shareId && versionsShareId === shareId) return;
  selectedShareId = shareId;
  if (previewMode) {
    versions =
      shareId === fixtureShareId ? fixtureVersions.map((item) => ({ ...item })) : [];
    versionsShareId = shareId;
    render();
    return;
  }
  versions = [];
  versionsShareId = null;
  render();
  const epoch = ++seen;
  void loadVersions(epoch)
    .then(() => {
      if (epoch === seen) render();
    })
    .catch((err: unknown) => {
      if (epoch !== seen) return;
      error = errorText(err);
      render();
    });
}

function actionButton(
  label: string,
  className: string,
  onClick: () => void,
  ariaLabel?: string,
  focusKey?: string,
): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = className;
  button.textContent = label;
  if (ariaLabel) button.setAttribute("aria-label", ariaLabel);
  if (focusKey) button.dataset.focus = focusKey;
  button.disabled = busy;
  button.addEventListener("click", () => {
    onClick();
  });
  return button;
}

function renderApp(): HTMLElement {
  const app = document.createElement("div");
  app.className = "app";
  app.append(renderSidebar(), renderMain());
  return app;
}

function renderSidebar(): HTMLElement {
  const aside = document.createElement("aside");
  aside.className = "sidebar";

  const brandBlock = document.createElement("div");
  brandBlock.className = "brand-block";
  const brand = document.createElement("p");
  brand.className = "brand";
  brand.textContent = "PeerFold";
  brandBlock.append(brand);

  const nav = document.createElement("nav");
  nav.className = "nav";
  nav.setAttribute("aria-label", "Разделы");
  for (const item of NAV) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "nav-btn";
    button.id = `nav-${item.id}`;
    button.textContent = item.label;
    if (section === item.id) button.setAttribute("aria-current", "page");
    button.addEventListener("click", () => {
      selectSection(item.id);
    });
    nav.append(button);
  }

  const device = document.createElement("div");
  device.className = "device-block";
  const field = document.createElement("div");
  field.className = "field";
  const label = document.createElement("label");
  label.className = "field-label";
  label.htmlFor = "device-name";
  label.textContent = "Имя устройства";
  const input = document.createElement("input");
  input.id = "device-name";
  input.className = "name-input";
  input.type = "text";
  input.autocomplete = "off";
  input.spellcheck = false;
  input.value = nameDraft ?? snapshot.deviceName;
  input.disabled = !booted;
  input.addEventListener("input", () => {
    nameDraft = input.value;
  });
  input.addEventListener("blur", () => {
    window.setTimeout(() => {
      void commitName();
    }, 0);
  });
  input.addEventListener("keydown", (event) => {
    if (event.key !== "Enter") return;
    event.preventDefault();
    input.blur();
  });
  field.append(label, input);

  const pause = actionButton("Пауза", "btn", () => {
    void togglePaused();
  }, undefined, "pause");
  pause.setAttribute("aria-pressed", snapshot.paused ? "true" : "false");
  device.append(field, pause);

  aside.append(brandBlock, nav, device);
  return aside;
}

function renderMain(): HTMLElement {
  const main = document.createElement("main");
  main.className = "main";
  main.setAttribute("aria-labelledby", "page-title");

  if (!booted) {
    const loading = document.createElement("p");
    loading.className = "page-note";
    loading.textContent = "Загрузка…";
    main.append(loading);
    return main;
  }

  const page = document.createElement("div");
  page.className = "page";
  if (section === "folders") page.append(renderFolders());
  else if (section === "devices") page.append(renderDevices());
  else page.append(renderHistory());
  main.append(page);
  return main;
}

function renderPageHead(title: string, extra?: HTMLElement): HTMLElement {
  const head = document.createElement("header");
  head.className = "page-head";
  const heading = document.createElement("h1");
  heading.id = "page-title";
  heading.className = "page-title";
  heading.textContent = title;
  head.append(heading);
  if (extra) head.append(extra);
  return head;
}

function renderError(): HTMLElement | null {
  if (!error) return null;
  const line = document.createElement("p");
  line.className = "error";
  line.setAttribute("role", "status");
  line.textContent = error;
  return line;
}

function renderFolders(): DocumentFragment {
  const fragment = document.createDocumentFragment();
  const addButton =
    snapshot.shares.length > 0
      ? actionButton(
          "Выбрать папку",
          "btn btn-primary",
          () => {
            void chooseAndCreateShare();
          },
          undefined,
          "create-share",
        )
      : undefined;
  fragment.append(renderPageHead("Папки", addButton));
  const errorLine = renderError();
  if (errorLine) fragment.append(errorLine);

  const body = document.createElement("div");
  body.className = "stack";
  for (const offer of snapshot.incoming) {
    body.append(renderIncoming(offer));
  }
  if (snapshot.shares.length === 0) {
    body.append(renderEmptyFolders());
  } else {
    for (const share of snapshot.shares) body.append(renderShare(share));
  }
  fragment.append(body, renderActivity());
  return fragment;
}

function renderEmptyFolders(): HTMLElement {
  const panel = document.createElement("section");
  panel.className = "card empty";
  const copy = document.createElement("p");
  copy.textContent =
    "Пока нет общих папок. Выберите папку проекта — она появится на других ваших компьютерах в этой сети.";
  panel.append(
    copy,
    actionButton("Выбрать папку", "btn btn-primary", () => {
      void chooseAndCreateShare();
    }, undefined, "create-share"),
  );
  return panel;
}

function renderIncoming(offer: Incoming): HTMLElement {
  const card = document.createElement("article");
  card.className = "card incoming";
  const text = document.createElement("p");
  text.textContent = `${offer.deviceName} делится папкой ${offer.name}`;
  card.append(
    text,
    actionButton(
      "Подключить к папке",
      "btn btn-primary",
      () => {
        void attachIncoming(offer);
      },
      `Подключить папку ${offer.name}`,
      `attach-${offer.shareId}`,
    ),
  );
  return card;
}

function renderShare(share: Share): HTMLElement {
  const card = document.createElement("article");
  card.className = "card share";
  const head = document.createElement("div");
  head.className = "card-head";
  const titles = document.createElement("div");
  titles.className = "stack";
  const name = document.createElement("h2");
  name.className = "item-title truncate";
  name.textContent = share.name;
  const path = document.createElement("p");
  path.className = "path";
  path.title = share.path;
  path.textContent = share.path;
  titles.append(name, path);
  const status = document.createElement("p");
  status.className = share.status === "syncing" ? "status status-syncing" : "status";
  status.textContent = statusLabel(share.status);
  head.append(titles, status);

  const foot = document.createElement("div");
  foot.className = "card-foot";
  const count = document.createElement("p");
  count.className = "muted";
  count.textContent = formatFileCount(share.fileCount);
  const actions = document.createElement("div");
  actions.className = "actions";
  actions.append(
    actionButton("Обновить", "btn", () => {
      void rescanShare(share);
    }, `Обновить папку ${share.name}`, `rescan-${share.id}`),
    actionButton("Отключить", "btn", () => {
      void removeShare(share);
    }, `Отключить папку ${share.name}`, `remove-${share.id}`),
  );
  foot.append(count, actions);
  card.append(head, foot);
  return card;
}

function renderActivity(): HTMLElement {
  const sectionEl = document.createElement("section");
  sectionEl.className = "activity";
  sectionEl.setAttribute("aria-labelledby", "activity-title");
  const heading = document.createElement("h2");
  heading.id = "activity-title";
  heading.className = "item-title page-note";
  heading.textContent = "Последние события";
  sectionEl.append(heading);

  const events = orderedActivity();
  if (events.length === 0) {
    const empty = document.createElement("p");
    empty.className = "muted";
    empty.textContent = "Пока нет событий.";
    sectionEl.append(empty);
    return sectionEl;
  }

  const list = document.createElement("ol");
  list.className = "activity-list";
  for (const event of events) {
    const item = document.createElement("li");
    const time = document.createElement("time");
    const seconds = toUnixSeconds(event.at);
    time.dateTime = new Date(seconds * 1000).toISOString();
    time.textContent = formatTime(event.at);
    const text = document.createElement("span");
    text.textContent = event.text;
    item.append(time, text);
    list.append(item);
  }
  sectionEl.append(list);
  return sectionEl;
}

function renderDevices(): DocumentFragment {
  const fragment = document.createDocumentFragment();
  fragment.append(renderPageHead("Устройства"));
  const errorLine = renderError();
  if (errorLine) fragment.append(errorLine);

  const peers = orderedPeers();
  if (peers.length === 0) {
    const empty = document.createElement("p");
    empty.className = "card muted";
    empty.textContent = "Пока нет других компьютеров в\u00A0этой сети.";
    fragment.append(empty);
    return fragment;
  }

  const list = document.createElement("div");
  list.className = "stack";
  for (const peer of peers) list.append(renderPeer(peer));
  fragment.append(list);
  return fragment;
}

function renderPeer(peer: Peer): HTMLElement {
  const card = document.createElement("article");
  card.className = "card peer";

  const dot = document.createElement("span");
  dot.className = peer.online ? "dot is-online" : "dot is-offline";
  dot.setAttribute("aria-hidden", "true");

  const main = document.createElement("div");
  main.className = "peer-main";
  const name = document.createElement("h2");
  name.className = "item-title truncate";
  name.textContent = peer.name;
  const presence = document.createElement("span");
  presence.className = "sr-only";
  presence.textContent = peer.online ? "в сети" : "не в сети";
  const code = document.createElement("p");
  code.className = "code";
  code.textContent = formatCode(peer.pairingCode);
  main.append(name, presence, code);

  if (peer.trusted && peer.mutual) {
    const status = document.createElement("p");
    status.className = "peer-status is-live";
    status.textContent = "На связи";
    main.append(status);
  } else if (peer.trusted) {
    const status = document.createElement("p");
    status.className = "peer-status";
    status.textContent = "Ждём подтверждение на другом компьютере";
    main.append(status);
  }

  const actions = document.createElement("div");
  actions.className = "actions";
  if (!peer.trusted) {
    actions.append(
      actionButton(
        "Доверять",
        "btn btn-primary",
        () => {
          void trustPeer(peer.deviceId);
        },
        `Доверять устройству ${peer.name}`,
        `trust-${peer.deviceId}`,
      ),
    );
  }
  actions.append(
    actionButton("Отклонить", "btn", () => {
      void rejectPeer(peer.deviceId);
    }, `Отклонить устройство ${peer.name}`, `reject-${peer.deviceId}`),
    actionButton("Забыть", "btn", () => {
      void forgetPeer(peer.deviceId);
    }, `Забыть устройство ${peer.name}`, `forget-${peer.deviceId}`),
  );

  card.append(dot, main, actions);
  return card;
}

function renderHistory(): DocumentFragment {
  const fragment = document.createDocumentFragment();
  fragment.append(renderPageHead("История"));
  const errorLine = renderError();
  if (errorLine) fragment.append(errorLine);

  const share = selectedShare();
  if (!share) {
    const empty = document.createElement("p");
    empty.className = "card muted";
    empty.textContent = "Сначала выберите папку\u00A0— здесь появятся её версии.";
    fragment.append(empty);
    return fragment;
  }

  if (snapshot.shares.length > 1) {
    const group = document.createElement("div");
    group.className = "actions";
    group.setAttribute("role", "group");
    group.setAttribute("aria-label", "Папка");
    for (const item of snapshot.shares) {
      const button = actionButton(item.name, "btn", () => {
        selectHistoryShare(item.id);
      }, undefined, `history-${item.id}`);
      button.setAttribute("aria-pressed", item.id === share.id ? "true" : "false");
      group.append(button);
    }
    fragment.append(group);
  } else {
    const note = document.createElement("p");
    note.className = "page-note";
    note.textContent = `Папка «${share.name}»`;
    fragment.append(note);
  }

  const current = versionsShareId === share.id ? orderedVersions() : [];
  if (current.length === 0) {
    const empty = document.createElement("p");
    empty.className = "muted";
    empty.textContent = "Версий пока нет.";
    fragment.append(empty);
    return fragment;
  }

  const list = document.createElement("div");
  list.className = "stack";
  for (const version of current) list.append(renderVersion(share.id, version));
  fragment.append(list);
  return fragment;
}

function renderVersion(shareId: string, version: VersionInfo): HTMLElement {
  const card = document.createElement("article");
  card.className = "card version";
  const main = document.createElement("div");
  main.className = "version-main";
  const path = document.createElement("p");
  path.className = "item-title truncate";
  path.title = version.relPath;
  path.textContent = version.relPath;
  const meta = document.createElement("p");
  meta.className = "meta";
  const time = document.createElement("time");
  const seconds = toUnixSeconds(version.createdAt);
  time.dateTime = new Date(seconds * 1000).toISOString();
  time.textContent = formatTime(version.createdAt);
  const id = document.createElement("span");
  id.textContent = shortId(version.deviceId);
  const size = document.createElement("span");
  size.textContent = formatSize(version.size);
  meta.append(time, document.createTextNode(" · "), id, document.createTextNode(" · "), size);
  main.append(path, meta);
  card.append(
    main,
    actionButton(
      "Вернуть эту версию",
      "btn btn-primary",
      () => {
        void restoreVersion(shareId, version);
      },
      `Вернуть эту версию ${version.relPath}`,
      `restore-${shareId}-${version.hash}`,
    ),
  );
  return card;
}

function render(): void {
  const root = document.querySelector("#app");
  if (!(root instanceof HTMLElement)) return;
  const active = document.activeElement;
  const activeId = active instanceof HTMLElement ? active.id : "";
  const activeFocus = active instanceof HTMLElement ? (active.dataset.focus ?? "") : "";
  const selection =
    active instanceof HTMLInputElement
      ? { start: active.selectionStart, end: active.selectionEnd }
      : null;

  root.replaceChildren(renderApp());

  const next = activeId
    ? document.getElementById(activeId)
    : activeFocus
      ? document.querySelector(`[data-focus="${CSS.escape(activeFocus)}"]`)
      : null;
  if (!(next instanceof HTMLElement)) return;
  next.focus();
  if (next instanceof HTMLInputElement && selection) {
    const start = selection.start ?? next.value.length;
    const end = selection.end ?? start;
    next.setSelectionRange(start, end);
  }
}

async function init(): Promise<void> {
  render();
  if (previewMode) return;
  try {
    const unlisten = await listen("state", () => {
      void refresh();
    });
    window.addEventListener("pagehide", () => {
      void unlisten();
    });
    await refresh();
  } catch (err) {
    error = errorText(err);
    booted = true;
    render();
  }
}

void init();
