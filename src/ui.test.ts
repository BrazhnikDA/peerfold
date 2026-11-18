import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { beforeEach, describe, expect, it, vi } from "vitest";

const css = readFileSync(resolve("src/styles.css"), "utf8");

const invoke = vi.fn();
const open = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (...args: unknown[]) => open(...args),
}));

type Snapshot = {
  deviceId: string;
  deviceName: string;
  paused: boolean;
  port: number;
  peers: Array<{
    deviceId: string;
    name: string;
    online: boolean;
    trusted: boolean;
    mutual: boolean;
    pairingCode: string;
    waiting: boolean;
  }>;
  shares: Array<{
    id: string;
    name: string;
    path: string;
    fileCount: number;
    status: string;
  }>;
  incoming: Array<{
    deviceId: string;
    deviceName: string;
    shareId: string;
    name: string;
  }>;
  activity: Array<{ at: number; text: string }>;
};

const liveState: Snapshot = {
  deviceId: "local",
  deviceName: "Ноутбук",
  paused: false,
  port: 47332,
  peers: [
    {
      deviceId: "desk",
      name: "Кабинет",
      online: true,
      trusted: true,
      mutual: true,
      pairingCode: "482913",
      waiting: false,
    },
    {
      deviceId: "away",
      name: "Мастерская",
      online: false,
      trusted: true,
      mutual: false,
      pairingCode: "105774",
      waiting: false,
    },
  ],
  shares: [
    {
      id: "notes",
      name: "Заметки",
      path: "/home/user/Документы/заметки",
      fileCount: 18,
      status: "idle",
    },
  ],
  incoming: [],
  activity: [{ at: 1_700_000_000, text: "Папка на связи" }],
};

function peerCard(name: string): HTMLElement {
  const card = [...document.querySelectorAll(".peer")].find((item) =>
    item.querySelector(".item-title")?.textContent === name,
  );
  if (!(card instanceof HTMLElement)) throw new Error(`нет компьютера «${name}»`);
  return card;
}

function normalizeColor(value: string): string {
  const hex = value.trim().toLowerCase();
  if (hex.startsWith("#") && hex.length === 7) return hex;
  const match = value.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);
  if (!match) return hex;
  const channel = (part: string) => Number(part).toString(16).padStart(2, "0");
  return `#${channel(match[1])}${channel(match[2])}${channel(match[3])}`;
}

function button(label: string): HTMLButtonElement {
  const found = [...document.querySelectorAll("button")].find(
    (item) => item.textContent === label,
  );
  if (!(found instanceof HTMLButtonElement)) {
    throw new Error(`нет кнопки «${label}»`);
  }
  return found;
}

function buttons(label: string): HTMLButtonElement[] {
  return [...document.querySelectorAll("button")].filter(
    (item): item is HTMLButtonElement =>
      item instanceof HTMLButtonElement && item.textContent === label,
  );
}

async function boot(mode: "preview" | "app"): Promise<void> {
  vi.resetModules();
  document.body.innerHTML = '<div id="app"></div>';
  if (mode === "app") window.__TAURI_INTERNALS__ = {};
  else delete window.__TAURI_INTERNALS__;
  await import("./main.ts");
  if (mode === "app") {
    await vi.waitFor(() => {
      const input = document.querySelector("#device-name");
      if (!(input instanceof HTMLInputElement) || input.disabled) {
        throw new Error("имя ещё не готово");
      }
    });
  }
}

function setUserAgent(value: string): void {
  Object.defineProperty(navigator, "userAgent", {
    configurable: true,
    get: () => value,
  });
}

beforeEach(() => {
  setUserAgent("Mozilla/5.0 (X11; Linux x86_64)");
  invoke.mockReset();
  open.mockReset();
  invoke.mockImplementation(async (command: string) => {
    if (command === "state") return structuredClone(liveState);
    if (command === "versions") {
      return [
        {
          relPath: "readme.md",
          hash: "abc123",
          size: 18432,
          mtimeNs: 1,
          deviceId: "desk",
          createdAt: 1_700_000_100,
        },
      ];
    }
    return null;
  });
  open.mockResolvedValue("/tmp/проекты/агент");
});

describe("списки", () => {
  it("показывает папки, входящее предложение и события", async () => {
    await boot("preview");
    expect(document.querySelector("h1")?.textContent).toBe("Папки");
    expect(document.body.textContent).toContain("Заметки");
    expect(document.body.textContent).toContain("/home/user/Документы/заметки");
    expect(document.body.textContent).toContain("18\u00A0файлов");
    expect(document.body.textContent).toContain("Мастерская делится папкой Чертежи");
    expect(document.body.textContent).toContain("Кабинет обновил файлы");
  });

  it("показывает компьютеры и размер индикатора сети", async () => {
    await boot("preview");
    button("Устройства").click();
    expect(document.querySelector("h1")?.textContent).toBe("Устройства");
    expect(document.body.textContent).toContain("Кабинет");
    expect(document.body.textContent).toContain("Ноутбук");
    expect(document.body.textContent).toContain("Гостевой");
    const online = document.querySelector(".dot.is-online");
    const offline = document.querySelector(".dot.is-offline");
    expect(online).toBeTruthy();
    expect(offline).toBeTruthy();
    const style = document.createElement("style");
    style.textContent = css;
    document.head.append(style);
    expect(getComputedStyle(online as Element).width).toBe("14px");
    expect(getComputedStyle(online as Element).height).toBe("14px");
    expect(normalizeColor(getComputedStyle(online as Element).backgroundColor)).toBe("#16d35a");
    expect(normalizeColor(getComputedStyle(offline as Element).backgroundColor)).toBe("#ff2d2d");
    expect(document.body.textContent).toContain("в сети");
    expect(document.body.textContent).toContain("не в сети");
  });

  it("показывает список версий выбранной папки", async () => {
    await boot("preview");
    button("История").click();
    expect(document.body.textContent).toContain("readme.md");
    expect(document.body.textContent).toContain("главы/первая.txt");
    expect(document.body.textContent).toContain("images/cover.png");
    expect(buttons("Вернуть эту версию")).toHaveLength(3);
  });
});

describe("кнопки и поле имени", () => {
  it("пауза, доверие, отказ и забывание меняют списки", async () => {
    await boot("preview");
    button("Пауза").click();
    await vi.waitFor(() => {
      expect(button("Пауза").getAttribute("aria-pressed")).toBe("true");
    });

    button("Устройства").click();
    button("Доверять").click();
    await vi.waitFor(() => {
      const guest = peerCard("Гостевой");
      expect(guest.textContent).not.toContain("Доверять");
      expect(guest.textContent).toContain("Ждём подтверждение на другом компьютере");
    });

    buttons("Забыть")[0].click();
    await vi.waitFor(() => {
      expect(document.querySelectorAll(".peer")).toHaveLength(2);
    });
  });

  it("добавляет, подключает, обновляет и отключает папки", async () => {
    await boot("preview");
    button("Выбрать папку").click();
    await vi.waitFor(() => {
      expect(document.body.textContent).toContain("агент");
    });
    button("Подключить к папке").click();
    await vi.waitFor(() => {
      expect(document.body.textContent).toContain("Чертежи");
      expect(document.body.textContent).not.toContain("делится папкой");
    });
    const refresh = buttons("Обновить")[0];
    refresh.click();
    await Promise.resolve();
    expect(document.body.textContent).toContain("синхронизация");
    buttons("Отключить")[0].click();
    await Promise.resolve();
  });

  it("сохраняет имя ключом name, без обёртки args", async () => {
    await boot("app");
    const input = document.querySelector("#device-name");
    if (!(input instanceof HTMLInputElement)) throw new Error("нет поля имени");
    expect(input.value).toBe("Ноутбук");
    input.value = "  Кухня  ";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("blur"));
    await vi.waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("set_name", { name: "Кухня" });
    });
    expect(invoke).not.toHaveBeenCalledWith("set_name", expect.objectContaining({ args: expect.anything() }));
    expect(document.querySelector(".error")).toBeNull();
  });

  it("показывает ошибку команды и не затирает введённое имя", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "state") return structuredClone(liveState);
      if (command === "set_name") throw new Error("missing required key name");
      return null;
    });
    await boot("app");
    const input = document.querySelector("#device-name");
    if (!(input instanceof HTMLInputElement)) throw new Error("нет поля имени");
    input.value = "Кухня";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("blur"));
    await vi.waitFor(() => {
      expect(document.body.textContent).toContain("missing required key name");
    });
    const again = document.querySelector("#device-name");
    if (!(again instanceof HTMLInputElement)) throw new Error("поле пропало");
    expect(again.value).toBe("Кухня");
  });

  it("пока открыт выбор папки, кнопка показывает что программа отвечает", async () => {
    let releasePicker: (path: string) => void = () => {};
    open.mockImplementation(
      () =>
        new Promise<string>((resolvePicker) => {
          releasePicker = resolvePicker;
        }),
    );
    await boot("preview");
    button("Выбрать папку").click();
    await vi.waitFor(() => {
      expect(document.body.textContent).toContain("Открываю…");
    });
    releasePicker("/tmp/проекты/агент");
    await vi.waitFor(() => {
      expect(document.body.textContent).toContain("агент");
      expect(document.body.textContent).not.toContain("Открываю…");
    });
  });

  it("на Windows выбирает папку отдельной командой и не вешает окно", async () => {
    setUserAgent("Mozilla/5.0 (Windows NT 10.0; Win64; x64)");
    invoke.mockImplementation(async (command: string) => {
      if (command === "choose_folder") return "C:\\Users\\Kate\\Documents\\проект";
      if (command === "state") return structuredClone(liveState);
      return null;
    });
    await boot("preview");
    button("Выбрать папку").click();
    await vi.waitFor(() => {
      expect(document.body.textContent).toContain("проект");
    });
    expect(invoke).toHaveBeenCalledWith("choose_folder");
    expect(open).not.toHaveBeenCalled();
  });

  it("показывает пустые списки и ошибку недоступной папки", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "state") {
        return {
          ...structuredClone(liveState),
          peers: [],
          shares: [],
          incoming: [],
          activity: [],
        };
      }
      if (command === "create_share") throw new Error("папка не найдена");
      return null;
    });
    await boot("app");
    expect(document.body.textContent).toContain("Пока нет общих папок");
    button("Устройства").click();
    expect(document.body.textContent).toContain("Пока нет других компьютеров");
    button("История").click();
    expect(document.body.textContent).toContain("Сначала выберите папку");
    button("Папки").click();
    button("Выбрать папку").click();
    await vi.waitFor(() => {
      expect(document.body.textContent).toContain("папка не найдена");
    });
  });

  it("возвращает версию из списка", async () => {
    await boot("app");
    button("История").click();
    await vi.waitFor(() => {
      expect(document.body.textContent).toContain("readme.md");
    });
    button("Вернуть эту версию").click();
    await vi.waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("restore", {
        shareId: "notes",
        relPath: "readme.md",
        hash: "abc123",
      });
    });
  });
});
