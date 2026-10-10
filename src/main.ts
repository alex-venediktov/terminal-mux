import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";

interface Command {
  id: string;
  title: string;
  icon: string;
  color?: string;
  program: string;
  args: string[];
  continueArgs?: string[];
  history?: string;
  shiftEnter?: string;
  elevate?: boolean;
  pasteImage?: string;
  pick?: "ssh-hosts";
}

interface SshHost {
  alias: string;
  host_name: string | null;
  user: string | null;
  port: string | null;
}

interface Config {
  fontSize: number;
  sort: "date" | "name";
  sideWidth: number;
  sideFontSize: number;
  collapsed: string[];
  commands: Command[];
}

interface Instance {
  id: number;
  cmd: Command;
  cwd: string | null;
  oscTitle: string;
  term: Terminal;
  fit: FitAddon;
  el: HTMLDivElement;
  exited: boolean;
  subtitle?: string;
}

interface ProjectInfo {
  cwd: string;
  modified: number;
  history: Record<string, number>;
  exists: boolean;
}

interface Project {
  cwd: string;
  modified: number;
  history: Map<string, number>;
}

interface TreeNode {
  key: string;
  path: string;
  name: string;
  project?: string;
  children: TreeNode[];
  modified: number;
}

const FONT_DEFAULT = 14;
const SIDE_WIDTH = 260;
const SIDE_FONT = 13;
const APP_TITLE = "terminal-mux";

const stage = document.getElementById("stage") as HTMLElement;
const side = document.getElementById("side") as HTMLElement;
const splitter = document.getElementById("splitter") as HTMLElement;
const commandBar = document.getElementById("commands") as HTMLElement;
const projectList = document.getElementById("projects") as HTMLUListElement;
const toast = document.getElementById("toast") as HTMLElement;
const appWindow = getCurrentWindow();

const instances = new Map<number, Instance>();
const pending = new Map<number, string[]>();
let order: number[] = [];
let activeId: number | null = null;
let selectedKey: string | null = null;
const projects = new Map<string, Project>();
let config: Config = {
  fontSize: FONT_DEFAULT,
  sort: "date",
  sideWidth: SIDE_WIDTH,
  sideFontSize: SIDE_FONT,
  collapsed: [],
  commands: [],
};
let collapsed = new Set<string>();
let fontSize = FONT_DEFAULT;
let saveTimer = 0;
let toastTimer = 0;
let renderTimer = 0;
const iconCache = new Map<string, string>();
const closed = new Set<number>();
const writeQueues = new Map<number, Promise<unknown>>();
let configPatch: Partial<Config> = {};

function key(cwd: string): string {
  return cwd.replace(/[\\/]+$/, "").toLowerCase();
}

function basename(p: string): string {
  return p.split(/[\\/]/).filter(Boolean).pop() ?? p;
}

function formatDate(ms: number): string {
  const d = new Date(ms);
  const now = new Date();
  const two = (n: number) => String(n).padStart(2, "0");
  if (d.toDateString() === now.toDateString()) return `сегодня ${two(d.getHours())}:${two(d.getMinutes())}`;
  return `${two(d.getDate())}.${two(d.getMonth() + 1)}.${d.getFullYear()}`;
}

function showToast(text: string, ms = 900) {
  toast.textContent = text;
  toast.classList.add("visible");
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => toast.classList.remove("visible"), ms);
}

// Сохранение измененных ключей config.json с задержкой, чтобы не писать файл на каждый шаг колеса
function saveConfig(patch: Partial<Config>) {
  configPatch = { ...configPatch, ...patch };
  clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => {
    invoke("set_config", { value: configPatch }).catch(() => {});
    configPatch = {};
  }, 500);
}

function setFont(size: number, save = true) {
  fontSize = Math.min(36, Math.max(8, size));
  for (const inst of instances.values()) inst.term.options.fontSize = fontSize;
  fitAll();
  if (save) {
    config.fontSize = fontSize;
    saveConfig({ fontSize });
    showToast(`шрифт ${fontSize}`);
  }
}

// Ширина панели проектов не меньше 160 px; верхний предел 60% окна задает max-width в стилях
function setSideWidth(width: number, save = true) {
  const w = Math.round(Math.max(160, width));
  side.style.width = `${w}px`;
  if (save) {
    config.sideWidth = w;
    saveConfig({ sideWidth: w });
  }
}

// Размер шрифта строк списка проектов, отдельный от шрифта терминала
function setSideFont(size: number, save = true) {
  config.sideFontSize = Math.min(24, Math.max(9, size));
  projectList.style.fontSize = `${config.sideFontSize}px`;
  if (save) {
    saveConfig({ sideFontSize: config.sideFontSize });
    showToast(`шрифт панели ${config.sideFontSize}`);
  }
}

// Отметка активности проекта: поднимает его наверх списка
function touch(cwd: string | null) {
  if (!cwd) return;
  const k = key(cwd);
  const p = projects.get(k) ?? { cwd, modified: 0, history: new Map() };
  p.modified = Date.now();
  projects.set(k, p);
  if (renderTimer) return;
  renderTimer = window.setTimeout(() => {
    renderTimer = 0;
    renderProjects();
  }, 1000);
}

// Клавиши, которые перехватывает мультиплексор до xterm.js
function hostKeys(cmd: Command, term: Terminal, e: KeyboardEvent): boolean {
  if (e.key === "Enter" && e.shiftKey && !e.ctrlKey && !e.altKey && cmd.shiftEnter !== undefined) {
    if (e.type === "keydown") send(idOf(term), cmd.shiftEnter);
    e.preventDefault();
    return false;
  }
  if (e.type !== "keydown") return true;
  const id = idOf(term);
  // Ctrl+Shift+W закрывает экземпляр; у завершившегося процесса - любая клавиша
  if ((e.ctrlKey && e.shiftKey && e.code === "KeyW") || (id !== null && instances.get(id)?.exited && !isModifier(e))) {
    if (id !== null) closeInstance(id);
    e.preventDefault();
    return false;
  }
  if (e.altKey && !e.ctrlKey && !e.shiftKey) {
    const step = ({ Equal: 1, NumpadAdd: 1, Minus: -1, NumpadSubtract: -1 } as Record<string, number>)[e.code];
    if (step) {
      setFont(fontSize + step);
      e.preventDefault();
      return false;
    }
    if (e.code === "Digit0" || e.code === "Numpad0") {
      setFont(FONT_DEFAULT);
      e.preventDefault();
      return false;
    }
  }
  if (e.ctrlKey && (e.key === "PageUp" || e.key === "PageDown")) {
    cycle(e.key === "PageUp" ? 1 : -1);
    e.preventDefault();
    return false;
  }
  if (e.ctrlKey && e.key.toLowerCase() === "c" && term.hasSelection()) {
    navigator.clipboard.writeText(term.getSelection());
    term.clearSelection();
    return false;
  }
  // Вставку выполняет обработчик paste в xterm.js с учетом bracketed paste
  if (e.ctrlKey && e.key.toLowerCase() === "v") return false;
  return true;
}

function isModifier(e: KeyboardEvent): boolean {
  return ["Control", "Shift", "Alt", "Meta"].includes(e.key);
}

function createTerminal(cmd: Command): { term: Terminal; fit: FitAddon; el: HTMLDivElement } {
  const el = document.createElement("div");
  el.className = "term active";
  stage.appendChild(el);
  const term = new Terminal({
    fontFamily: '"Cascadia Mono", Consolas, monospace',
    fontSize,
    cursorBlink: true,
    allowProposedApi: true,
    scrollback: 5000,
    windowsPty: { backend: "conpty", buildNumber: 26200 },
    theme: { background: "#0c0c0c" },
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(el);
  try {
    term.loadAddon(new WebglAddon());
  } catch {
    // WebGL недоступен - остается DOM-отрисовка
  }
  fit.fit();
  term.attachCustomKeyEventHandler((e) => hostKeys(cmd, term, e));
  return { term, fit, el };
}

function idOf(term: Terminal): number | null {
  for (const inst of instances.values()) if (inst.term === term) return inst.id;
  return null;
}

function send(id: number | null, data: string) {
  if (id === null) return;
  const inst = instances.get(id);
  if (!inst || inst.exited) return;
  // Записи в экземпляр идут строго по очереди: асинхронные команды Tauri выполняются параллельно
  const queue = (writeQueues.get(id) ?? Promise.resolve())
    .then(() => invoke("write", { id, data }))
    .catch((e) => console.error(e));
  writeQueues.set(id, queue);
}

async function spawn(cmd: Command, cwd: string | null, pick?: { args: string[]; subtitle: string }) {
  // Команда с повышением прав открывается в отдельном окне после диалога UAC
  if (cmd.elevate) {
    invoke("launch_elevated", { program: cmd.program, args: cmd.args, cwd })
      .then(() => showToast(`${cmd.title}: отдельное окно`))
      .catch((e) => showToast(String(e), 4000));
    return;
  }
  const project = cwd ? projects.get(key(cwd)) : undefined;
  // Продолжение последней сессии проекта, если у команды есть история и она уже есть на диске
  const cont = !!cmd.history && !!project?.history.has(cmd.history);
  const args = pick?.args ?? [...cmd.args, ...(cont ? cmd.continueArgs ?? [] : [])];
  const { term, fit, el } = createTerminal(cmd);
  const previous = activeId;
  for (const other of instances.values()) other.el.classList.remove("active");
  try {
    const res = await invoke<{ id: number }>("spawn", {
      program: cmd.program,
      args,
      cwd,
      cols: term.cols,
      rows: term.rows,
    });
    const inst: Instance = {
      id: res.id,
      cmd,
      cwd,
      oscTitle: "",
      term,
      fit,
      el,
      exited: false,
      subtitle: pick?.subtitle,
    };
    instances.set(inst.id, inst);
    order.push(inst.id);
    term.onData((d) => send(inst.id, d));
    term.onTitleChange((t) => {
      inst.oscTitle = t;
      if (inst.id === activeId) updateTitle();
    });
    for (const chunk of pending.get(inst.id) ?? []) term.write(chunk);
    pending.delete(inst.id);
    if (cmd.history) {
      touch(cwd);
      if (cwd) projects.get(key(cwd))!.history.set(cmd.history, Date.now());
    }
    activate(inst.id);
  } catch (e) {
    // Неудачный запуск не оставляет терминала: возвращается прежний активный экземпляр
    term.dispose();
    el.remove();
    if (previous !== null) activate(previous);
    showToast(`ошибка запуска ${cmd.title}: ${e}`, 4000);
  }
}

function updateTitle() {
  const inst = activeId !== null ? instances.get(activeId) : undefined;
  const title = inst
    ? [inst.cmd.title, inst.subtitle ?? (inst.cwd ? basename(inst.cwd) : "~"), inst.oscTitle]
        .filter(Boolean)
        .join(" - ")
    : APP_TITLE;
  appWindow.setTitle(title).catch(() => {});
}

function activate(id: number) {
  const inst = instances.get(id);
  if (!inst) return;
  for (const other of instances.values()) other.el.classList.toggle("active", other.id === id);
  activeId = id;
  if (inst.cwd) selectedKey = key(inst.cwd);
  fitAll();
  inst.term.focus();
  updateTitle();
  renderProjects();
}

function cycle(step: number) {
  if (order.length === 0 || activeId === null) return;
  const i = order.indexOf(activeId);
  activate(order[(i + step + order.length) % order.length]);
}

function fitAll() {
  for (const inst of instances.values()) {
    const { cols, rows } = inst.term;
    inst.fit.fit();
    if (!inst.exited && (inst.term.cols !== cols || inst.term.rows !== rows || inst.id === activeId)) {
      invoke("resize", { id: inst.id, cols: inst.term.cols, rows: inst.term.rows }).catch(() => {});
    }
  }
}

function closeInstance(id: number) {
  const inst = instances.get(id);
  if (!inst) return;
  invoke("kill", { id }).catch(() => {});
  closed.add(id);
  writeQueues.delete(id);
  inst.term.dispose();
  inst.el.remove();
  instances.delete(id);
  order = order.filter((x) => x !== id);
  if (activeId === id) {
    activeId = null;
    if (order.length) activate(order[order.length - 1]);
    else updateTitle();
  }
  renderProjects();
}

// Живые экземпляры команд с историей (claude, pi) в проекте
function liveInProject(k: string): Instance[] {
  return order
    .map((id) => instances.get(id)!)
    .filter((i) => !i.exited && !!i.cmd.history && i.cwd !== null && key(i.cwd) === k);
}

// Команда для открытия проекта: та, чья история в проекте свежее, иначе первая с историей
function defaultCommand(p: Project | undefined): Command | undefined {
  const withHistory = config.commands.filter((c) => c.history);
  if (p) {
    const recent = [...p.history].sort((a, b) => b[1] - a[1])[0]?.[0];
    const cmd = withHistory.find((c) => c.history === recent);
    if (cmd) return cmd;
  }
  return withHistory[0] ?? config.commands[0];
}

function selectProject(k: string) {
  selectedKey = k;
  const live = liveInProject(k);
  if (live.length) activate(live[live.length - 1].id);
  else renderProjects();
}

function openProject(k: string) {
  const live = liveInProject(k);
  if (live.length) {
    activate(live[live.length - 1].id);
    return;
  }
  const p = projects.get(k);
  const cmd = defaultCommand(p);
  if (p && cmd) spawn(cmd, p.cwd);
}

// Каталог для новой команды: выбранный проект, иначе домашний
function targetCwd(): string | null {
  return selectedKey ? projects.get(selectedKey)?.cwd ?? null : null;
}

// Выбор или создание каталога в системном диалоге и новый сеанс в нем
async function newInDirectory() {
  const dir = await open({ directory: true, title: "Каталог для нового сеанса", defaultPath: targetCwd() ?? undefined });
  if (typeof dir !== "string") return;
  const k = key(dir);
  if (!projects.has(k)) projects.set(k, { cwd: dir, modified: Date.now(), history: new Map() });
  selectedKey = k;
  const cmd = defaultCommand(projects.get(k));
  if (cmd) spawn(cmd, dir);
}

function setSort(sort: "date" | "name") {
  config.sort = sort;
  saveConfig({ sort });
  document.querySelectorAll<HTMLButtonElement>("#bar .sort button").forEach((b) => {
    b.classList.toggle("on", b.dataset.sort === sort);
  });
  renderProjects();
}

// Дерево проектов по сегментам пути без учета регистра
function buildTree(): TreeNode[] {
  const root: TreeNode = { key: "", path: "", name: "", children: [], modified: 0 };
  const index = new Map<string, TreeNode>();
  for (const [k, p] of projects) {
    let node = root;
    for (const part of p.cwd.split(/[\\/]/).filter(Boolean)) {
      const path = node.key ? `${node.path}\\${part}` : part;
      const nodeKey = node.key ? `${node.key}\\${part.toLowerCase()}` : part.toLowerCase();
      let next = index.get(nodeKey);
      if (!next) {
        next = { key: nodeKey, path, name: part, children: [], modified: 0 };
        index.set(nodeKey, next);
        node.children.push(next);
      }
      node = next;
    }
    node.project = k;
  }
  return compact(root).children;
}

// Дата узла по самому свежему проекту внутри и сортировка уровня; каталог без проекта с одним потомком склеивается с ним
function compact(node: TreeNode): TreeNode {
  node.children = node.children.map(compact);
  node.modified = node.project ? projects.get(node.project)!.modified : 0;
  for (const c of node.children) node.modified = Math.max(node.modified, c.modified);
  if (node.key && !node.project && node.children.length === 1) {
    const child = node.children[0];
    return { ...child, name: `${node.name}\\${child.name}` };
  }
  node.children.sort((a, b) =>
    config.sort === "name" ? a.name.localeCompare(b.name, "ru", { sensitivity: "base" }) : b.modified - a.modified,
  );
  return node;
}

// Ключи проектов в поддереве узла, включая сам узел
function projectsIn(node: TreeNode): string[] {
  return [...(node.project ? [node.project] : []), ...node.children.flatMap(projectsIn)];
}

function toggleNode(k: string) {
  if (!collapsed.delete(k)) collapsed.add(k);
  config.collapsed = [...collapsed];
  saveConfig({ collapsed: config.collapsed });
  renderProjects();
}

function renderProjects() {
  const items: HTMLLIElement[] = [];
  const walk = (nodes: TreeNode[], depth: number) => {
    for (const node of nodes) {
      const folded = node.children.length > 0 && collapsed.has(node.key);
      const li = document.createElement("li");
      li.style.paddingLeft = `calc(6px + ${depth * 1.1}em)`;
      const twist = document.createElement("span");
      twist.className = "twist";
      if (node.children.length) twist.textContent = folded ? "▸" : "▾";
      li.appendChild(twist);
      const inner = folded ? projectsIn(node) : node.project ? [node.project] : [];
      if (node.project) {
        const k = node.project;
        const p = projects.get(k)!;
        li.className = k === selectedKey ? "selected" : "";
        li.title = `${p.cwd}\nдвойной клик - открыть`;
        // Тип проекта: иконки команд, чья история есть в проекте, свежая первой
        const types = document.createElement("span");
        types.className = "ptype";
        for (const [kind] of [...p.history].sort((a, b) => b[1] - a[1])) {
          const cmd = config.commands.find((c) => c.history === kind);
          if (cmd) types.appendChild(iconNode(cmd));
        }
        li.appendChild(types);
        li.onclick = () => selectProject(k);
        li.ondblclick = () => openProject(k);
        if (node.children.length) {
          twist.onclick = (e) => {
            e.stopPropagation();
            toggleNode(node.key);
          };
          twist.ondblclick = (e) => e.stopPropagation();
        }
      } else {
        li.className = "dir";
        li.title = node.path;
        li.onclick = () => toggleNode(node.key);
      }
      // Свернутый узел подсвечивается, если внутри выбранный проект
      if (folded && selectedKey && selectedKey !== node.project && inner.includes(selectedKey)) {
        li.classList.add("contains");
      }
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = node.name;
      li.appendChild(name);
      const live = inner.reduce((n, k) => n + liveInProject(k).length, 0);
      if (live) {
        const dot = document.createElement("span");
        dot.className = "live";
        dot.textContent = live > 1 ? `● ${live}` : "●";
        li.appendChild(dot);
      }
      const date = document.createElement("span");
      date.className = "date";
      date.textContent = formatDate(node.project ? projects.get(node.project)!.modified : node.modified);
      li.appendChild(date);
      items.push(li);
      if (!folded) walk(node.children, depth + 1);
    }
  };
  walk(buildTree(), 0);
  projectList.replaceChildren(...items);
}

// Иконка команды: картинка из файла (data URL из кеша) либо символ своим цветом
function iconNode(cmd: Command): HTMLElement {
  const url = iconCache.get(cmd.id);
  if (url) {
    const img = document.createElement("img");
    img.src = url;
    return img;
  }
  const span = document.createElement("span");
  span.textContent = cmd.icon || cmd.id.slice(0, 2);
  if (cmd.color) span.style.color = cmd.color;
  return span;
}

async function renderCommands() {
  for (const cmd of config.commands) {
    if (/\.(png|svg|ico|jpe?g)$/i.test(cmd.icon) && !iconCache.has(cmd.id)) {
      const url = await invoke<string>("read_icon", { path: cmd.icon }).catch(() => "");
      if (url) iconCache.set(cmd.id, url);
    }
  }
  commandBar.replaceChildren(
    ...config.commands.map((cmd) => {
      const b = document.createElement("button");
      b.title = cmd.title;
      b.appendChild(iconNode(cmd));
      b.onclick = () => (cmd.pick === "ssh-hosts" ? openSshPicker(cmd, b) : spawn(cmd, targetCwd()));
      return b;
    }),
  );
}

// Выпадающий список хостов из ~/.ssh/config под кнопкой команды; выбор подставляет хост вместо {host}
async function openSshPicker(cmd: Command, button: HTMLElement) {
  document.getElementById("picker")?.remove();
  const hosts = await invoke<SshHost[]>("list_ssh_hosts").catch(() => []);
  if (!hosts.length) {
    showToast("в ~/.ssh/config нет хостов");
    return;
  }
  const menu = document.createElement("ul");
  menu.id = "picker";
  const r = button.getBoundingClientRect();
  menu.style.left = `${r.left}px`;
  menu.style.top = `${r.bottom + 4}px`;
  for (const h of hosts) {
    const li = document.createElement("li");
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = h.alias;
    const meta = document.createElement("span");
    meta.className = "date";
    meta.textContent = [h.user ? `${h.user}@` : "", h.host_name ?? "", h.port ? `:${h.port}` : ""].join("");
    li.append(name, meta);
    li.onclick = () => {
      close();
      spawn(cmd, null, { args: cmd.args.map((a) => a.split("{host}").join(h.alias)), subtitle: h.alias });
    };
    menu.appendChild(li);
  }
  const close = () => {
    menu.remove();
    document.removeEventListener("mousedown", outside, true);
    document.removeEventListener("keydown", escape, true);
  };
  const outside = (e: MouseEvent) => {
    if (!menu.contains(e.target as Node)) close();
  };
  const escape = (e: KeyboardEvent) => {
    if (e.key === "Escape") close();
  };
  document.addEventListener("mousedown", outside, true);
  document.addEventListener("keydown", escape, true);
  document.body.appendChild(menu);
}

// Проекты из хранилищ сессий; удаленные с диска каталоги не показываются
async function loadProjects() {
  const list = await invoke<ProjectInfo[]>("list_projects");
  for (const info of list) {
    if (!info.exists) continue;
    const history = new Map(Object.entries(info.history).map(([kind, t]) => [kind, t * 1000]));
    projects.set(key(info.cwd), { cwd: info.cwd, modified: info.modified * 1000, history });
  }
  renderProjects();
}

listen<{ id: number; data: string }>("pty-output", (e) => {
  const inst = instances.get(e.payload.id);
  if (closed.has(e.payload.id)) return;
  if (!inst) {
    pending.set(e.payload.id, [...(pending.get(e.payload.id) ?? []), e.payload.data]);
    return;
  }
  inst.term.write(e.payload.data);
  if (inst.cmd.history) touch(inst.cwd);
});

// Завершение с кодом 0 закрывает экземпляр, иначе он остается с сообщением об ошибке
listen<{ id: number; code: number | null }>("pty-exit", (e) => {
  const inst = instances.get(e.payload.id);
  if (!inst) return;
  if (e.payload.code === 0) {
    closeInstance(inst.id);
    return;
  }
  inst.exited = true;
  inst.term.write(`\r\n\x1b[90m[процесс завершен, код ${e.payload.code ?? "?"}; любая клавиша - закрыть]\x1b[0m\r\n`);
  renderProjects();
});

document.getElementById("new-dir")!.onclick = newInDirectory;
document.querySelectorAll<HTMLButtonElement>("#bar .sort button").forEach((b) => {
  b.onclick = () => setSort(b.dataset.sort as "date" | "name");
});

// Alt + колесо мыши меняет размер шрифта, событие не доходит до терминала
stage.addEventListener(
  "wheel",
  (e) => {
    if (!e.altKey) return;
    e.preventDefault();
    e.stopPropagation();
    setFont(fontSize + (e.deltaY < 0 ? 1 : -1));
  },
  { capture: true, passive: false },
);

// Alt + колесо мыши над панелью меняет размер шрифта списка проектов
side.addEventListener(
  "wheel",
  (e) => {
    if (!e.altKey) return;
    e.preventDefault();
    setSideFont(config.sideFontSize + (e.deltaY < 0 ? 1 : -1));
  },
  { passive: false },
);

// Перетаскивание разделителя меняет ширину панели, ширина сохраняется при отпускании
splitter.addEventListener("pointerdown", (e) => {
  if (e.button !== 0) return;
  e.preventDefault();
  splitter.setPointerCapture(e.pointerId);
  splitter.classList.add("drag");
  const x0 = e.clientX;
  const w0 = side.offsetWidth;
  splitter.onpointermove = (m) => setSideWidth(w0 + m.clientX - x0, false);
  splitter.onlostpointercapture = () => {
    splitter.onpointermove = null;
    splitter.onlostpointercapture = null;
    splitter.classList.remove("drag");
    setSideWidth(side.offsetWidth);
  };
});
splitter.addEventListener("dblclick", () => setSideWidth(SIDE_WIDTH));

// Картинка в буфере без текста: приложению уходит его сочетание вставки картинки, файл оно читает само
stage.addEventListener(
  "paste",
  (e) => {
    const inst = activeId !== null ? instances.get(activeId) : undefined;
    const data = e.clipboardData;
    if (!inst?.cmd.pasteImage || !data || data.getData("text/plain")) return;
    if (![...data.items].some((i) => i.kind === "file" && i.type.startsWith("image/"))) return;
    e.preventDefault();
    e.stopPropagation();
    send(inst.id, inst.cmd.pasteImage);
  },
  { capture: true },
);

// Контекстное меню WebView в окне терминала не нужно
document.addEventListener("contextmenu", (e) => e.preventDefault());

let resizeTimer = 0;
new ResizeObserver(() => {
  clearTimeout(resizeTimer);
  resizeTimer = window.setTimeout(fitAll, 60);
}).observe(stage);

invoke<Config>("get_config")
  .then((c) => (config = c))
  .catch(() => {})
  .finally(() => {
    setFont(config.fontSize || FONT_DEFAULT, false);
    setSideWidth(config.sideWidth || SIDE_WIDTH, false);
    setSideFont(config.sideFontSize || SIDE_FONT, false);
    collapsed = new Set(Array.isArray(config.collapsed) ? config.collapsed : []);
    document.querySelectorAll<HTMLButtonElement>("#bar .sort button").forEach((b) => {
      b.classList.toggle("on", b.dataset.sort === (config.sort ?? "date"));
    });
    updateTitle();
    renderCommands().finally(loadProjects);
  });
