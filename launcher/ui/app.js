// DisasterZone Launcher window. Talks to the Rust side (src-tauri/src/lib.rs) through Tauri commands.
// Opened outside Tauri (a plain browser), it runs on made-up demo data so the page can be previewed: nothing is
// installed or sent.
"use strict";

const T = window.__TAURI__;
const DEMO = !T;
const api = DEMO ? demoApi() : {
  invoke: (cmd, args) => T.core.invoke(cmd, args),
  listen: (ev, fn) => T.event.listen(ev, fn),
  pickFolder: () => T.dialog.open({ directory: true, title: "Your World of Warcraft folder" }),
  minimize: () => T.window.getCurrentWindow().minimize(),
  close: () => T.window.getCurrentWindow().close(),
};

const FLAVOR_NAMES = { _anniversary_: "Anniversary", _classic_era_: "Classic Era", _classic_: "Classic", _retail_: "Retail" };
const RECHECK_MS = 30 * 60 * 1000;

let status = null;     // status command
let check = null;      // check command (newest release vs installed)
let checkErr = null;
let checking = false;
let working = false;

const $ = (id) => document.getElementById(id);
function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}
function fmtBytes(n) {
  if (n < 1024) return n + " B";
  if (n < 1024 * 1024) return Math.round(n / 1024) + " KB";
  return (n / 1048576).toFixed(1) + " MB";
}
function fmtDate(iso) {
  if (!iso) return "";
  const d = new Date(iso);
  return isNaN(d) ? iso : d.toLocaleDateString(undefined, { day: "numeric", month: "long", year: "numeric" });
}
function plural(n, one, many) { return n + " " + (n === 1 ? one : many); }

// ---------------------------------------------------------------------------------------------------------------
// Toast + modal
// ---------------------------------------------------------------------------------------------------------------

let toastTimer = null;
function toast(text, good) {
  const t = $("toast");
  t.textContent = String(text);
  t.className = "toast" + (good ? " good" : "");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.add("hidden"), good ? 4000 : 8000);
}
function confirmBox(title, body, okText, danger) {
  return new Promise((resolve) => {
    $("m-title").textContent = title;
    $("m-body").textContent = body;
    const ok = $("m-ok");
    ok.textContent = okText;
    ok.className = "btn " + (danger ? "danger" : "primary");
    $("modal").classList.remove("hidden");
    const done = (v) => { $("modal").classList.add("hidden"); ok.onclick = null; $("m-cancel").onclick = null; resolve(v); };
    ok.onclick = () => done(true);
    $("m-cancel").onclick = () => done(false);
  });
}

// ---------------------------------------------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------------------------------------------

async function loadStatus() {
  status = await api.invoke("status");
  renderChrome();
  renderSettings();
}

async function runCheck() {
  if (!status || !status.flavorDir || working) { renderHero(); return; }
  checking = true;
  $("btn-check").classList.add("spin");
  renderHero();
  try {
    check = await api.invoke("check");
    checkErr = null;
  } catch (e) {
    check = null;
    checkErr = String(e);
  }
  checking = false;
  $("btn-check").classList.remove("spin");
  renderHero();
  renderNews();
  renderChangelog();
}

async function refreshAll() {
  await loadStatus();
  await runCheck();
  renderBackups();
}

// ---------------------------------------------------------------------------------------------------------------
// Header + footer
// ---------------------------------------------------------------------------------------------------------------

function renderChrome() {
  const s = status.settings;
  const parts = ["Launcher", FLAVOR_NAMES[s.flavor] || s.flavor];
  if (s.channel !== "stable") parts.push(s.channel);
  if (DEMO) parts.push("demo");
  $("kicker").textContent = parts.join(" · ");
  $("foot-path").textContent = status.flavorDir || "No game folder: pick it in Settings";
  $("foot-ver").textContent = "Launcher " + status.launcherVersion;
}

// ---------------------------------------------------------------------------------------------------------------
// Home
// ---------------------------------------------------------------------------------------------------------------

function setHero(chip, chipCls, ver, next, detail) {
  const c = $("state-chip");
  c.textContent = chip;
  c.className = "chip" + (chipCls ? " " + chipCls : "");
  $("ver-main").textContent = ver || "—";
  $("ver-next").textContent = next ? "→ " + next : "";
  $("detail").textContent = detail || "";
}

function setNotice(text, buttonText, onClick) {
  const n = $("notice");
  n.textContent = "";
  if (!text) { n.classList.add("hidden"); return; }
  n.append(text);
  if (buttonText) {
    const b = el("button", "link", buttonText);
    b.onclick = onClick;
    n.append(b);
  }
  n.classList.remove("hidden");
}

function renderHero() {
  if (!status || working) return;
  const btn = $("btn-update");
  const inst = status.installed;
  btn.disabled = true;
  btn.onclick = null;
  btn.classList.remove("hidden");
  $("btn-play").className = "btn big";
  setNotice(null);

  if (!status.flavorDir) {
    setHero("No game folder", "warn", "—", null, "Pick your World of Warcraft folder to install DisasterZone.");
    btn.textContent = "Choose folder";
    btn.disabled = false;
    btn.onclick = pickFolder;
    return;
  }
  if (inst && (inst.dev || inst.linked)) {
    setHero("Development copy", "", inst.version, null, "This is a development copy of the addon: the launcher leaves it alone.");
    btn.textContent = "Update";
    return;
  }
  if (checking && !check) {
    setHero("Checking", "", inst && inst.version, null, "Looking for the newest release…");
    btn.textContent = inst && inst.present ? "Update" : "Install";
    return;
  }
  if (checkErr) {
    setHero("Can't check", "warn", inst && inst.version, null, checkErr);
    btn.textContent = "Try again";
    btn.disabled = false;
    btn.onclick = runCheck;
    return;
  }
  if (!check) return;

  const size = check.downloadBytes ? fmtBytes(check.downloadBytes) + " to download" : "nothing to download";
  if (!inst || !inst.present) {
    setHero("Not installed", "gold", check.version, null, plural(check.files, "file", "files") + " · " + size);
    btn.textContent = "Install";
    btn.disabled = false;
    btn.onclick = runUpdate;
  } else if (check.upToDate) {
    setHero("Up to date", "ok", inst.version, null, "Released " + fmtDate(check.published));
    btn.classList.add("hidden");                 // (nothing to do: Play is the main action)
    $("btn-play").className = "btn big primary";
  } else if (check.hold) {
    setHero("Restored", "warn", inst.version, check.version, plural(check.changed, "file", "files") + " to change · " + size);
    setNotice("You went back to an older version, so updates are paused.");
    btn.textContent = "Update to " + check.version;
    btn.disabled = false;
    btn.onclick = runUpdate;
  } else if (inst.version === check.version && inst.build === check.build) {
    setHero("Needs repair", "warn", inst.version, null,
      plural(check.changed + check.removed, "file differs", "files differ") + " from the release · " + size);
    btn.textContent = "Repair";
    btn.disabled = false;
    btn.onclick = runUpdate;
  } else {
    setHero("Update ready", "gold", inst.version || "?", check.version,
      plural(check.changed, "file", "files") + " to change · " + size);
    btn.textContent = "Update";
    btn.disabled = false;
    btn.onclick = runUpdate;
  }
  if (check.older) setNotice("The release list looks older than one already installed from. Try again in a few minutes.");
  else if (status.wowRunning && !check.upToDate) setNotice("World of Warcraft is running. Restart the game after updating so it loads the new files.");
}

function sectionRow(s, withLines) {
  const row = el("div", "sec");
  const kind = ["new", "changed", "fixed"].includes(s.kind) ? s.kind : "other";
  row.append(el("span", "kind " + kind, s.kind || "note"), el("div", "h", s.heading));
  if (s.lead) row.append(el("div", "lead", s.lead));
  if (withLines && s.lines && s.lines.length) {
    const ul = el("ul");
    for (const line of s.lines) {
      const li = el("li", null, typeof line === "string" ? line : line.text);
      if (typeof line !== "string" && line.for) li.append(el("span", "role", line.for.split(" ").join(", ")));
      ul.append(li);
    }
    row.append(ul);
  }
  return row;
}

function changelog() {
  return (check && check.notes && check.notes.changelog) || [];
}

function renderNews() {
  const box = $("news");
  box.textContent = "";
  const rel = changelog()[0];
  if (!rel) { box.append(el("div", "muted", "Nothing yet.")); return; }
  box.append(el("div", "rel-title", rel.title || "DisasterZone " + rel.version));
  box.append(el("div", "rel-date", rel.version + " · " + rel.date));
  for (const s of rel.sections.slice(0, 6)) box.append(sectionRow(s, false));
  if (rel.sections.length > 6) {
    const more = el("button", "link", plural(rel.sections.length - 6, "more change", "more changes"));
    more.onclick = () => showTab("changelog");
    box.append(more);
  }
}

// ---------------------------------------------------------------------------------------------------------------
// Update / restore
// ---------------------------------------------------------------------------------------------------------------

const STEP_TEXT = { download: "Downloading", stage: "Preparing the new files", swap: "Putting the new version in place", done: "Done" };

function showProgress(p) {
  $("progress").classList.remove("hidden");
  const pct = p.total ? Math.min(100, (p.done / p.total) * 100) : 0;
  const weight = { download: [0, 70], stage: [70, 25], swap: [95, 5], done: [100, 0] }[p.step] || [0, 100];
  $("bar").style.width = (weight[0] + (weight[1] * pct) / 100) + "%";
  $("ptext").textContent = p.step === "download"
    ? STEP_TEXT.download + " " + fmtBytes(p.done) + " of " + fmtBytes(p.total)
    : STEP_TEXT[p.step] || p.step;
}

async function runUpdate() {
  if (working) return;
  working = true;
  const btn = $("btn-update");
  btn.disabled = true;
  btn.textContent = "Updating…";
  $("btn-check").disabled = true;
  setNotice(null);
  showProgress({ step: "download", done: 0, total: 0 });
  try {
    const out = await api.invoke("update");
    showProgress({ step: "done", done: 1, total: 1 });
    toast("DisasterZone " + out.version + " is installed." + (status.wowRunning ? " Restart World of Warcraft to load it." : ""), true);
  } catch (e) {
    toast(e);
  }
  working = false;
  $("btn-check").disabled = false;
  setTimeout(() => $("progress").classList.add("hidden"), 1200);
  await refreshAll();
}

async function restoreBackup(b) {
  const ok = await confirmBox("Restore " + (b.version || "this backup") + "?",
    "The addon goes back to version " + (b.version || "?") + ". The version you have now is backed up first, " +
    "and updates pause until you update again. Your saved variables are not touched.", "Restore", false);
  if (!ok) return;
  try {
    await api.invoke("restore", { id: b.id });
    toast("Restored " + (b.version || "the backup") + ".", true);
  } catch (e) {
    toast(e);
  }
  await refreshAll();
}

async function play() {
  try {
    await api.invoke("play");
  } catch (e) {
    toast(e);
  }
}

// ---------------------------------------------------------------------------------------------------------------
// Changelog + backups
// ---------------------------------------------------------------------------------------------------------------

function renderChangelog() {
  const box = $("changelog");
  const list = changelog();
  if (!list.length) return;
  box.textContent = "";
  for (const rel of list) {
    const r = el("div", "release");
    const top = el("div", "top");
    top.append(el("span", "v", rel.version), el("span", "t", rel.title), el("span", "d", rel.date));
    r.append(top);
    for (const s of rel.sections) r.append(sectionRow(s, true));
    box.append(r);
  }
}

async function renderBackups() {
  const box = $("backups");
  box.textContent = "";
  let list = [];
  try {
    list = status && status.flavorDir ? await api.invoke("backups") : [];
  } catch (e) {
    box.append(el("div", "muted", String(e)));
    return;
  }
  if (!list.length) { box.append(el("div", "muted", "No backups yet. One is made before every update.")); return; }
  for (const b of list) {
    const row = el("div", "brow");
    row.append(el("div", "bv", b.version || "?"));
    const info = el("div", "bi");
    const when = b.id.slice(0, 15).replace(/^(\d{4})(\d{2})(\d{2})-(\d{2})(\d{2})\d{2}$/, "$1-$2-$3T$4:$5:00Z");
    const d = new Date(when);
    const strong = el("b", null, isNaN(d) ? b.id : d.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" }));
    info.append(strong, " · " + (b.reason === "restore" ? "before a restore" : "before an update"));
    if (b.savedVariables) info.append(" · " + plural(b.savedVariables, "saved variables file", "saved variables files"));
    row.append(info);
    const btn = el("button", "btn", "Restore");
    btn.onclick = () => restoreBackup(b);
    row.append(btn);
    box.append(row);
  }
}

// ---------------------------------------------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------------------------------------------

function segment(box, items, current, onPick) {
  box.textContent = "";
  for (const [value, label] of items) {
    const b = el("button", "btn" + (value === current ? " chosen" : ""), label);
    b.onclick = () => { if (value !== current) onPick(value); };
    box.append(b);
  }
}

async function saveSettings(patch, recheck) {
  try {
    await api.invoke("set_settings", patch);
  } catch (e) {
    toast(e);
  }
  await loadStatus();
  if (recheck) { check = null; await runCheck(); }
}

async function setPath(path, flavor) {
  try {
    await api.invoke("set_wow_path", { path, flavor: flavor || null });
  } catch (e) {
    toast(e);
    return;
  }
  check = null;
  await refreshAll();
}

async function pickFolder() {
  const path = await api.pickFolder();
  if (path) await setPath(path);
}

function renderSettings() {
  const s = status.settings;
  $("set-path").textContent = s.wowRoot || "Not set";
  const installs = status.installs || [];
  $("row-installs").classList.toggle("hidden", installs.length < 2);
  segment($("set-installs"), installs.map((w) => [w.root, w.root]), s.wowRoot, (root) => setPath(root));
  const here = installs.find((w) => s.wowRoot && w.root.toLowerCase() === s.wowRoot.toLowerCase());
  const flavors = here ? here.flavors : [s.flavor];
  segment($("set-flavor"), flavors.map((f) => [f, FLAVOR_NAMES[f] || f]), s.flavor, (f) => setPath(s.wowRoot, f));
  for (const b of $("set-channel").children) {
    b.className = "btn" + (b.dataset.v === s.channel ? " chosen" : "");
    b.onclick = () => { if (b.dataset.v !== s.channel) saveSettings({ channel: b.dataset.v }, true); };
  }
  $("keep").textContent = s.keepBackups;
  $("set-check").setAttribute("aria-checked", String(!!s.checkOnStart));
  for (const [id, key] of [["set-code", "launchCode"], ["set-cmd", "playCommand"], ["set-source", "source"]]) {
    const f = $(id);
    if (document.activeElement !== f) f.value = s[key] || "";
  }
  $("set-source").placeholder = status.source;
  $("about").textContent = "DisasterZone Launcher " + status.launcherVersion + " · releases from " + status.source;
}

function wireSettings() {
  $("btn-pick").onclick = pickFolder;
  $("foot-path").onclick = () => (status && status.flavorDir ? api.invoke("open_folder", { kind: "addons" }).catch(toast) : showTab("settings"));
  $("keep-minus").onclick = () => saveSettings({ keepBackups: Math.max(1, status.settings.keepBackups - 1) });
  $("keep-plus").onclick = () => saveSettings({ keepBackups: Math.min(10, status.settings.keepBackups + 1) });
  $("set-check").onclick = () => saveSettings({ checkOnStart: !status.settings.checkOnStart });
  const field = (id, key, recheck) => {
    const f = $(id);
    const save = () => { if ((status.settings[key] || "") !== f.value.trim()) saveSettings({ [key]: f.value }, recheck); };
    f.addEventListener("change", save);
    f.addEventListener("keydown", (e) => { if (e.key === "Enter") f.blur(); });
  };
  field("set-code", "launchCode");
  field("set-cmd", "playCommand");
  field("set-source", "source", true);
  $("btn-open-backups").onclick = () => api.invoke("open_folder", { kind: "backups" }).catch(toast);
}

// ---------------------------------------------------------------------------------------------------------------
// Tabs + start
// ---------------------------------------------------------------------------------------------------------------

function showTab(name) {
  for (const t of document.querySelectorAll(".tab")) t.classList.toggle("active", t.dataset.tab === name);
  for (const p of document.querySelectorAll(".page")) p.classList.toggle("active", p.id === "page-" + name);
  if (name === "backups") renderBackups();
}

// The launcher's own update (tauri-plugin-updater; the feed is the release "launcher-latest").
async function checkLauncherUpdate() {
  if (DEMO || !T.updater) return;
  let upd = null;
  try {
    upd = await T.updater.check();
  } catch (e) {
    return;                       // (offline, or no feed yet: the addon check reports connection problems)
  }
  if (!upd) return;
  $("lbar-text").textContent = "A new launcher is ready: " + upd.version + ".";
  $("lbar").classList.remove("hidden");
  $("lbar-btn").onclick = async () => {
    if (working) { toast("Wait for the addon update to finish first."); return; }
    $("lbar-btn").disabled = true;
    $("lbar-btn").textContent = "Downloading…";
    try {
      await upd.downloadAndInstall();
      await T.process.relaunch();
    } catch (e) {
      toast(e);
      $("lbar-btn").disabled = false;
      $("lbar-btn").textContent = "Install and restart";
    }
  };
}

async function start() {
  for (const t of document.querySelectorAll(".tab")) t.onclick = () => showTab(t.dataset.tab);
  for (const l of document.querySelectorAll("[data-goto]")) l.onclick = () => showTab(l.dataset.goto);
  $("wmin").onclick = () => api.minimize();
  $("wclose").onclick = () => api.close();
  $("btn-check").onclick = runCheck;
  $("btn-play").onclick = play;
  wireSettings();
  await api.listen("progress", (e) => { if (working) showProgress(e.payload); });
  try {
    await loadStatus();
  } catch (e) {
    toast(e);
    return;
  }
  if (status.settings.checkOnStart || !status.installed || !status.installed.present) await runCheck();
  else renderHero();
  renderBackups();
  checkLauncherUpdate();
  setInterval(() => { if (!working && !checking) runCheck(); }, RECHECK_MS);
}

start();

// ---------------------------------------------------------------------------------------------------------------
// Demo data (a plain browser, no Tauri)
// ---------------------------------------------------------------------------------------------------------------

function demoApi() {
  const settings = { wowRoot: "C:\\World of Warcraft", flavor: "_anniversary_", channel: "stable", seenSeq: {}, hold: false,
    keepBackups: 3, checkOnStart: true, source: null, launchCode: "", playCommand: "" };
  let installed = { present: true, version: "1.3.5", build: "7b7ab65a16911ed4", assetPack: 7, dev: false, linked: false, rooms: true };
  const backups = [{ id: "20261009-201544-1.3.3", version: "1.3.3", build: "f60f", time: "", reason: "update", savedVariables: 3 }];
  const notes = { changelog: [
    { version: "1.3.6", date: "14 October 2026", title: "The launcher, and quicker raid sign-ups", sections: [
      { heading: "DisasterZone Launcher", kind: "new", area: "other", lead: "Install and update the addon with one click.", lines: [
        "A desktop launcher for Windows, macOS and Linux keeps the addon up to date.",
        "It downloads only the files that changed, and keeps backups you can go back to.",
        { text: "Officers: beta testers can switch to the Beta channel in Settings.", for: "officers" }] },
      { heading: "Raids: sign up from the reminder", kind: "changed", area: "raids", lead: "Accept or decline straight from the pop-up.", lines: ["The raid reminder has Accept and Decline buttons."] },
      { heading: "Loot rolls", kind: "fixed", area: "loot", lead: "Rolls no longer close early when the master looter reloads.", lines: ["Fixed a roll window closing after a /reload."] },
    ] },
    { version: "1.3.5", date: "11 October 2026", title: "Raid nights, loot rules for everyone and a Welcome tour", sections: [
      { heading: "Raids: raid nights", kind: "new", area: "raids", lead: "See a team's raid nights before you join it.", lines: ["Teams can say which nights they raid."] },
      { heading: "Welcome tour", kind: "new", area: "other", lead: "A short tour of the addon, made for the roles you hold.", lines: ["New: a Welcome tour on the real window."] },
    ] },
  ] };
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  let progressFn = null;
  return {
    async invoke(cmd, args) {
      await wait(250);
      switch (cmd) {
        case "status": return { settings: { ...settings }, installs: [{ root: settings.wowRoot, flavors: ["_anniversary_", "_classic_era_", "_retail_"] }],
          installed: { ...installed }, flavorDir: settings.wowRoot + "\\" + settings.flavor, wowRunning: false, launcherVersion: "0.1.0",
          source: "https://raw.githubusercontent.com/OWNER/DisasterZone-releases/main/" };
        case "check": {
          const up = installed.version === "1.3.6";
          return { version: "1.3.6", build: "9a1c", seq: 2, channel: settings.channel, assetPack: 7, published: "2026-10-14T18:00:00Z",
            notes, files: 473, changed: up ? 0 : 23, removed: up ? 0 : 1, downloadBytes: up ? 0 : 412000, upToDate: up, older: false,
            hold: settings.hold, zipUrl: null };
        }
        case "update": {
          for (let i = 0; i <= 10; i++) { progressFn && progressFn({ payload: { step: "download", done: i * 41200, total: 412000 } }); await wait(120); }
          for (let i = 0; i <= 4; i++) { progressFn && progressFn({ payload: { step: "stage", done: i * 118, total: 473 } }); await wait(120); }
          progressFn && progressFn({ payload: { step: "swap", done: 0, total: 1 } });
          await wait(300);
          backups.unshift({ id: "20261014-190000-" + installed.version, version: installed.version, reason: "update", savedVariables: 3 });
          installed = { ...installed, version: "1.3.6", build: "9a1c" };
          settings.hold = false;
          return { version: "1.3.6", downloaded: 412000, changed: 23, removed: 1, backup: backups[0].id };
        }
        case "backups": return backups.slice();
        case "restore": { const b = backups.find((x) => x.id === args.id); installed = { ...installed, version: b.version, build: b.build }; settings.hold = true; return null; }
        case "set_settings": for (const k in args) if (args[k] != null) settings[k] = args[k]; return { ...settings };
        case "set_wow_path": if (args.flavor) settings.flavor = args.flavor; return { ...settings };
        case "play": throw "Demo: Play would start Battle.net here.";
        case "open_folder": throw "Demo: this would open the folder.";
      }
      throw "unknown command " + cmd;
    },
    async listen(ev, fn) { if (ev === "progress") progressFn = fn; },
    async pickFolder() { return null; },
    minimize() {},
    close() {},
  };
}
