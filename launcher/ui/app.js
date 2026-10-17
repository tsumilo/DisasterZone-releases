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
let updating = false;   // (working on an update: the progress bars show)

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
function joinNames(a) { return a.length < 2 ? a.join("") : a.slice(0, -1).join(", ") + " and " + a[a.length - 1]; }

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
  renderGames();
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
  renderGames();
  // the first release with game packs: ask once per start which games to keep (Update asks again if dismissed)
  if (check && check.needsPick && !check.upToDate && !pickerAsked && !working && !devCopy()) {
    pickerAsked = true;
    showTab("games");
    openPicker({});
  }
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
  if (DEMO && !location.search.includes("shot")) parts.push("demo");   // (?shot: README screenshots)
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

  renderHomePacks();
  const size = check.downloadBytes ? fmtBytes(check.downloadBytes) + " to download" : "nothing to download";
  const ch = changing();
  const go = (text) => { btn.textContent = text; btn.disabled = false; btn.onclick = runUpdate; };
  if (!inst || !inst.present) {
    const files = packs().filter((p) => p.wanted).reduce((n, p) => n + p.files, 0) || check.files;
    setHero("Not installed", "gold", check.version, null, plural(files, "file", "files") + " · " + size);
    go("Install");
  } else if (check.upToDate) {
    const n = games().filter((p) => p.installed).length;
    setHero("Up to date", "ok", inst.version, null,
      (games().length ? "DisasterZone and " + plural(n, "game", "games") + " installed · released " : "Released ") + fmtDate(check.published));
    btn.classList.add("hidden");                 // (nothing to do: Play is the main action)
    $("btn-play").className = "btn big primary";
  } else if (check.hold) {
    setHero("Restored", "warn", inst.version, check.version, describeChanges(ch) + " · " + size);
    setNotice("You went back to an older version, so updates are paused.");
    go("Update to " + check.version);
  } else if (inst.version === check.version && inst.build === check.build) {
    if (ch.some((p) => p.action === "update")) {
      setHero("Needs repair", "warn", inst.version, null,
        plural(check.changed + check.removed, "file differs", "files differ") + " from the release · " + size);
      go("Repair");
    } else {
      // (only packs coming or going: a game ticked or unticked, the 3D rooms switched)
      setHero("Changes waiting", "gold", inst.version, null, describeChanges(ch) + " · " + size);
      go("Apply");
    }
  } else {
    setHero("Update ready", "gold", inst.version || "?", check.version, describeChanges(ch) + " · " + size);
    go("Update");
  }
  if (check.older) setNotice("The release list looks older than one already installed from. Try again in a few minutes.");
  else if (check.needsPick && !check.upToDate) {
    setNotice("This version gives every game its own download.", "Pick your games", () => openPicker({ then: "update" }));
  } else if (status.wowRunning && !check.upToDate) setNotice("World of Warcraft is running. Restart the game after updating so it loads the new files.");
}

// What Update does, in words: "DisasterZone and Azeromon change · Chess to install".
function describeChanges(list) {
  const named = (action) => list.filter((p) => p.action === action).map((p) => p.title);
  const parts = [];
  const upd = named("update"), add = named("install"), drop = named("remove");
  if (upd.length) parts.push(joinNames(upd) + (upd.length === 1 ? " changes" : " change"));
  if (add.length) parts.push(joinNames(add) + " to install");
  if (drop.length) parts.push(joinNames(drop) + " to remove");
  return parts.join(" · ") || plural(check.changed, "file", "files") + " to change";
}

const PACK_STATE = { same: "up to date", update: "changes", install: "to install", remove: "to remove" };

// Home: one row per folder this PC has or gets.
function renderHomePacks() {
  const box = $("h-packs");
  box.textContent = "";
  const list = packs().filter((p) => p.required || p.installed || p.wanted);
  box.classList.toggle("hidden", list.length < 2);
  for (const p of list) {
    const row = el("div");
    row.append(el("span", null, p.title), el("span", p.action !== "same" ? "chg" : null, PACK_STATE[p.action] || ""));
    box.append(row);
  }
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

let lastProgress = { step: "download", done: 0, total: 0 };

// The bars of a running update: the hero's, the Games tab's strip and the cards being installed (class pbar-live).
function showProgress(p) {
  lastProgress = p;
  $("progress").classList.remove("hidden");
  const pct = p.total ? Math.min(100, (p.done / p.total) * 100) : 0;
  const weight = { download: [0, 70], stage: [70, 25], swap: [95, 5], done: [100, 0] }[p.step] || [0, 100];
  const width = (weight[0] + (weight[1] * pct) / 100) + "%";
  const text = p.step === "download"
    ? STEP_TEXT.download + " " + fmtBytes(p.done) + " of " + fmtBytes(p.total)
    : STEP_TEXT[p.step] || p.step;
  for (const b of document.querySelectorAll(".pbar-live")) b.style.width = width;
  for (const t of document.querySelectorAll(".ptext-live")) t.textContent = text;
}

// Update (Home, the Games strip, Install on a card): the core and every kept pack move to the release together.
// The first release with games asks which to keep first.
async function runUpdate() {
  if (working) return;
  if (check && check.needsPick && !check.upToDate) { openPicker({ then: "update" }); return; }
  await doUpdate();
}

async function doUpdate() {
  if (working) return;
  working = updating = true;
  const btn = $("btn-update");
  btn.disabled = true;
  btn.textContent = "Updating…";
  $("btn-check").disabled = true;
  setNotice(null);
  renderGames();
  showProgress({ step: "download", done: 0, total: 0 });
  try {
    const out = await api.invoke("update");
    showProgress({ step: "done", done: 1, total: 1 });
    const added = (out.folders || []).filter((f) => f.action === "install" && f.folder !== "DisasterZone").map((f) => titleOf(f.folder));
    const what = added.length ? "DisasterZone " + out.version + " and " + joinNames(added) + " are installed."
      : "DisasterZone " + out.version + " is installed.";
    toast(what + (status.wowRunning ? " Restart World of Warcraft to load it." : ""), true);
  } catch (e) {
    toast(e);
  }
  working = updating = false;
  installing = null;
  $("btn-check").disabled = false;
  setTimeout(() => $("progress").classList.add("hidden"), 1200);
  await refreshAll();
}

async function restoreBackup(b, newest) {
  const names = joinNames((b.folders || []).map(titleOf));
  const ok = b.reason === "remove"
    ? await confirmBox("Put " + names + " back?",
      names + " goes back in as it was when you removed it" + (newest ? "" : ", and what changed after that is undone first") +
      ". What you have now is backed up first, and updates pause until you update again.", "Put back", false)
    : await confirmBox("Restore " + (b.version || "this backup") + "?",
      "The addon goes back to version " + (b.version || "?") + ", with the games as they were then. What you have now is " +
      "backed up first, and updates pause until you update again. Your saved variables are not touched.", "Restore", false);
  if (!ok) return;
  try {
    await api.invoke("restore", { id: b.id });
    toast(b.reason === "remove" ? names + ((b.folders || []).length > 1 ? " are" : " is") + " back." : "Restored " + (b.version || "the backup") + ".", true);
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
  list.forEach((b, i) => {
    const row = el("div", "brow");
    row.append(el("div", "bv", b.version || "?"));
    const info = el("div", "bi");
    const when = b.id.slice(0, 15).replace(/^(\d{4})(\d{2})(\d{2})-(\d{2})(\d{2})\d{2}$/, "$1-$2-$3T$4:$5:00Z");
    const d = new Date(when);
    const strong = el("b", null, isNaN(d) ? b.id : d.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" }));
    const folders = (b.folders || []).map(titleOf);
    const first = (b.added || []).includes("DisasterZone");
    const why = first ? "before the first install"
      : b.reason === "remove" ? joinNames(folders) + " removed"
      : b.reason === "restore" ? "before a restore" : "before an update";
    info.append(strong, " · " + why);
    if (folders.length && b.reason !== "remove") info.append(" · " + joinNames(folders));
    if (b.bytes) info.append(el("span", "sz", " · " + fmtBytes(b.bytes)));
    if (b.savedVariables) info.append(" · " + plural(b.savedVariables, "saved variables file", "saved variables files"));
    row.append(info);
    if (!first) {      // (a backup from before the first install holds saved variables only: nothing to go back to)
      const btn = el("button", "btn", b.reason === "remove" ? "Put back" : "Restore");
      btn.onclick = () => restoreBackup(b, i === 0);
      row.append(btn);
    }
    box.append(row);
  });
}

// ---------------------------------------------------------------------------------------------------------------
// Games: a card per game pack, the 3D rooms under Extras, the one-time picker. The look of each card (art in
// games/) is keyed by the id the game registers with the Games hub; a game this launcher doesn't know yet gets the
// generic card. Sizes, taglines and what's installed come from the release list (check.packs).
// ---------------------------------------------------------------------------------------------------------------

const GAME_LOOK = {
  azeromon: { kicker: "Adventure", accent: "134, 216, 107", pix: true, tagline: "Tame and battle the beasts of Elwynn." },
  holdtheline: { kicker: "Survivors", accent: "224, 160, 64", tagline: "Fifteen minutes. One class. The whole Scourge." },
  illidan: { kicker: "Roguelike", accent: "169, 133, 240", tagline: "Five floors of the Black Temple, new every run." },
  bingo: { kicker: "Raid nights", accent: "242, 196, 64", tagline: "Squares fill in as the raid happens." },
  chess: { kicker: "The Gamesman's Hall", accent: "184, 176, 224", rooms: true, tagline: "Play Medivh's echo, or a guildmate." },
  trainer: { kicker: "Practice", accent: "79, 211, 224", rooms: true, tagline: "Practise each boss in 3D before the pull." },
};
const GAME_ORDER = Object.keys(GAME_LOOK);
const GENERIC_LOOK = { kicker: "Game", accent: "201, 163, 38", generic: true, tagline: "" };
const EXTRA_TEXT = {
  rooms: "Strategy boards in 3D, the chess hall and Raid Trainer's arenas. Without it, boards and chess fall back to 2D and the trainer's arenas stay flat.",
};
const CHECK_SVG = '<svg viewBox="0 0 12 12"><path d="M2 6.5l2.6 2.6L10 3.5"/></svg>';
const DOTS_SVG = '<svg viewBox="0 0 16 16"><circle cx="3.5" cy="8" r="1.4"/><circle cx="8" cy="8" r="1.4"/><circle cx="12.5" cy="8" r="1.4"/></svg>';

let menuOpen = null;      // pack id whose ··· menu is open
let menuAt = 0;           // (when it opened: the scroll that layout makes right then doesn't close it)
let installing = null;    // pack id an Install click is installing
let pickerAsked = false;  // the picker opened by itself once this start
let picker = null;        // { pick: {id: keep}, then: "update" | undefined } while the picker is open

function packs() { return (check && check.packs) || []; }
function games() {
  const rank = (p) => { const i = GAME_ORDER.indexOf(p.id); return i < 0 ? 99 : i; };
  return packs().filter((p) => p.kind === "game").sort((a, b) => rank(a) - rank(b) || a.title.localeCompare(b.title));
}
function changing() { return packs().filter((p) => p.action === "install" || p.action === "update" || p.action === "remove"); }
function lookOf(p) { return GAME_LOOK[p.id] || GENERIC_LOOK; }
function titleOf(folder) {
  const p = packs().find((x) => x.folder === folder);
  if (p) return p.title;
  if (folder === "DisasterZone_Rooms") return "3D rooms";
  return folder === "DisasterZone" ? folder : folder.replace(/^DisasterZone_/, "");
}
function diskOf(p) { return p.installed ? p.diskBytes || p.bytes : p.bytes; }
function devCopy() { return !!(status && status.installed && (status.installed.dev || status.installed.linked)); }
function busyCard(p) { return updating && (installing === p.id || (p.wanted && (p.action === "install" || p.action === "update"))); }
function playedText(t) {
  if (t == null) return null;
  if (!t) return "You've played it";
  const days = Math.floor((Date.now() / 1000 - t) / 86400);
  return days < 1 ? "Played today" : days < 2 ? "Played yesterday" : days < 14 ? "Played " + days + " days ago"
    : "Last played " + new Date(t * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short" });
}

function progressBox(cls) {
  const box = el("div", cls);
  const bar = el("div", "bar");
  const fill = el("i", "pbar-live");
  bar.append(fill);
  box.append(cls === "upd" ? el("span", "ptext ptext-live") : bar, cls === "upd" ? bar : el("div", "ptext ptext-live"));
  return box;
}

function renderGames() {
  const grid = $("g-grid");
  if (!grid || !status) return;
  grid.textContent = "";
  $("g-extras").textContent = "";
  const list = games();
  const left = $("g-left");
  left.textContent = "";

  // the head: the update strip, or a plain line
  if (updating) left.append(progressBox("upd"));
  else if (working) left.append(el("span", "muted", "Working…"));
  else if (devCopy()) left.append(el("span", "muted", "This is a development copy of the addon: the launcher leaves its folders alone."));
  else if (check && !check.upToDate) {
    const strip = el("div", "upd");
    const names = changing().map((p) => p.title);
    const size = check.downloadBytes ? " · " + fmtBytes(check.downloadBytes) : "";
    const b = el("button", "btn primary sm");
    if (check.needsPick) {
      strip.append(el("span", null, check.version + " is ready · pick your games first"));
      b.textContent = "Pick games";
      b.onclick = () => openPicker({ then: "update" });
    } else {
      const inst = status.installed;
      const newer = inst && inst.present && inst.version !== check.version;
      strip.append(el("span", null, (newer ? check.version + " is ready for " : "Waiting: ") + joinNames(names) + size));
      b.textContent = check.hold ? "Update to " + check.version : newer ? "Update" : "Apply";
      b.onclick = runUpdate;
    }
    strip.append(b);
    left.append(strip);
  } else left.append(el("span", "muted", "Pick what lives on this computer. Removing a game keeps your progress."));

  // the disk meter: the core and Extras in gold, each game in its colour
  let used = 0, max = 0;
  const segs = [];
  for (const p of packs()) {
    max += Math.max(p.bytes, p.diskBytes || 0);
    if (!p.installed) continue;
    used += diskOf(p);
    segs.push({ v: diskOf(p), c: p.kind === "game" ? "rgb(" + lookOf(p).accent + ")" : "var(--gold)" });
  }
  $("g-disk").textContent = check ? fmtBytes(used) : "—";
  const bar = $("g-segs");
  bar.textContent = "";
  for (const s of segs) {
    const i = el("i");
    i.style.width = (max ? (s.v / max) * 100 : 0) + "%";
    i.style.background = s.c;
    bar.append(i);
  }

  if (!list.length) {
    const why = !status.flavorDir ? "Pick your World of Warcraft folder in Settings first."
      : checkErr ? "The games show once the release list can be read: " + checkErr
      : !check ? "Looking for the newest release…"
      : "In this version every game is still part of DisasterZone. Once games come as downloads of their own, you pick yours here.";
    grid.append(el("div", "muted gempty", why));
  }
  for (const p of list) grid.append(gameCard(p));
  for (const p of packs().filter((x) => x.kind === "extra")) $("g-extras").append(extraRow(p));
  if (updating) showProgress(lastProgress);
  placeMenu();
}

function gameCard(p) {
  const look = lookOf(p);
  const card = el("article", "game" + (p.installed ? " inst" : ""));
  card.style.setProperty("--acc", "rgb(" + look.accent + ")");
  card.style.setProperty("--acc-rgb", look.accent);

  const art = el("div", "gart" + (look.generic ? " generic" : ""));
  if (!look.generic) {
    const pic = el("img", "pic");
    pic.src = "games/" + p.id + ".jpg";
    pic.alt = "";
    pic.draggable = false;
    art.append(pic);
  }
  art.append(el("div", "shade"));
  if (look.generic) art.append(el("div", "ltext", p.title));
  else {
    const logo = el("img", "logo" + (look.pix ? " pix" : ""));
    logo.src = "games/" + p.id + "-logo.png";
    logo.alt = p.title;
    logo.draggable = false;
    art.append(logo);
  }
  const kick = el("span", "gkick");
  kick.append(el("i"), look.kicker);
  art.append(kick);
  card.append(art);

  const body = el("div", "gbody");
  body.append(el("div", "gtag", p.tagline || look.tagline || p.title));
  const meta = el("div", "gmeta");
  const m1 = el("span"), m2 = el("span");
  m1.append(el("b", null, fmtBytes(diskOf(p))), " on disk");
  m2.append(el("b", null, fmtBytes(p.gz)), " download");
  meta.append(m1, m2);
  body.append(meta);

  const rooms = packs().find((x) => x.id === "rooms");
  const played = playedText(p.played);
  let stat;
  if (p.action === "remove") stat = el("div", "gstat idle", "Leaves with the next update");
  else if (!p.installed) stat = el("div", "gstat" + (p.wanted ? " chg" : ""),
    p.wanted ? "Installs with the next update" : "Not installed" + (played ? " · your progress is kept" : ""));
  else if (look.rooms && rooms && !rooms.installed) stat = el("div", "gstat idle", "Plays flat while 3D rooms are off");
  else if (played) stat = el("div", "gstat played", played);
  else stat = el("div", "gstat idle", "Never opened. Remove it to free " + fmtBytes(diskOf(p)) + ".");
  body.append(stat);

  const locked = working || devCopy() || !check;
  if (busyCard(p)) body.append(progressBox("gprog"));
  else if (p.action === "remove") {
    const act = el("div", "gact");
    const keep = el("button", "btn", "Keep it");
    keep.disabled = locked;
    keep.onclick = () => choose(p, true);
    act.append(keep);
    body.append(act);
  } else if (!p.installed) {
    const act = el("div", "gact");
    const b = el("button", "btn primary", "Install · " + fmtBytes(p.gz));
    b.disabled = locked;
    b.onclick = () => installPack(p);
    act.append(b);
    body.append(act);
  } else {
    const act = el("div", "gact");
    const ok = el("span", "ginst");
    ok.innerHTML = CHECK_SVG;
    ok.append("Installed");
    act.append(ok);
    if (p.action === "update") act.append(el("span", "gstat chg", "Changes in " + check.version));
    const dots = el("button", "ibtn dots");
    dots.innerHTML = DOTS_SVG;
    dots.title = "More";
    dots.setAttribute("aria-label", "More for " + p.title);
    dots.dataset.menu = p.id;
    dots.onclick = (e) => { e.stopPropagation(); menuOpen = menuOpen === p.id ? null : p.id; menuAt = performance.now(); placeMenu(); };
    act.append(dots);
    body.append(act);
  }
  card.append(body);
  return card;
}

// A card's ··· menu floats over the window (inside the scrolling grid it would be cut off), above its button when
// there is room.
function placeMenu() {
  for (const old of document.querySelectorAll(".gmenu")) old.remove();
  const p = menuOpen && packs().find((x) => x.id === menuOpen);
  const dots = p && document.querySelector('[data-menu="' + p.id + '"]');
  if (!dots) { menuOpen = null; return; }
  const menu = el("div", "gmenu");
  const open = el("button", null, "Open folder");
  open.onclick = () => { menuOpen = null; placeMenu(); api.invoke("open_folder", { kind: "pack", folder: p.folder }).catch(toast); };
  const rm = el("button", "danger", "Remove · frees " + fmtBytes(diskOf(p)));
  rm.disabled = working || devCopy();
  rm.onclick = () => removePack(p);
  menu.append(open, rm);
  document.body.append(menu);
  const r = dots.getBoundingClientRect();
  const h = menu.offsetHeight;
  menu.style.right = Math.max(8, window.innerWidth - r.right) + "px";
  menu.style.top = (r.top - h - 6 > 110 ? r.top - h - 6 : r.bottom + 6) + "px";
}

function extraRow(p) {
  const row = el("div", "xrow");
  const l = el("div", "rl");
  const t = el("div", "rt", p.title);
  t.append(el("span", "xtag", "Extra"));
  l.append(t, el("div", "rd", EXTRA_TEXT[p.id] || p.tagline || ""));
  row.append(l, el("span", "sz", busyCard(p) ? "installing…" : fmtBytes(diskOf(p))));
  const tg = el("button", "toggle");
  tg.setAttribute("role", "switch");
  tg.setAttribute("aria-label", p.title);
  tg.setAttribute("aria-checked", String(p.wanted && p.action !== "remove"));
  tg.append(el("i"));
  tg.disabled = working || devCopy() || !check;
  tg.onclick = () => (p.wanted && p.action !== "remove" ? turnOff(p) : installPack(p));
  row.append(tg);
  return row;
}

// What a choice does to the cards until the next check: they show progress while the update runs.
function assume(id, on) {
  const p = packs().find((x) => x.id === id);
  if (!p) return;
  p.wanted = on;
  p.chosen = on;
  p.action = on ? (p.installed ? (p.action === "remove" ? "same" : p.action) : "install") : (p.installed ? "remove" : "");
}

async function choose(p, on) {
  try { await api.invoke("set_packs", { packs: { [p.id]: on } }); } catch (e) { toast(e); }
  await refreshAll();
}

// Install on a card (and an Extra switched on): the pack goes in with the rest of the release, so a waiting update
// comes along.
async function installPack(p) {
  if (working) return;
  if (check && check.needsPick && p.kind === "game") { openPicker({ then: "update", tick: p.id }); return; }
  try { await api.invoke("set_packs", { packs: { [p.id]: true } }); } catch (e) { toast(e); return; }
  assume(p.id, true);
  installing = p.id;
  if (check && check.needsPick) { openPicker({ then: "update" }); return; }
  await doUpdate();
}

async function removePack(p) {
  menuOpen = null;
  renderGames();
  const size = fmtBytes(diskOf(p));
  const ok = await confirmBox("Remove " + p.title + "?", "Frees " + size + ". Your progress stays in DisasterZone's saved " +
    "variables, so installing it again picks up where you left off.", "Remove", true);
  if (ok) await takeOut(p, p.title + " removed · " + size + " freed.");
}

async function turnOff(p) {
  if (p.installed) await takeOut(p, p.title + " removed · " + fmtBytes(diskOf(p)) + " freed.");
  else await choose(p, false);
}

async function takeOut(p, done) {
  if (working) return;
  working = true;
  renderGames();
  try {
    await api.invoke("remove_pack", { id: p.id, folder: p.folder });
    toast(done + (status.wowRunning ? " It leaves the game at your next /reload." : ""), true);
  } catch (e) {
    toast(e);
  }
  working = false;
  await refreshAll();
}

// The one-time picker: a switch per game, the ones played (or installed) on.
function openPicker(opts) {
  const list = games();
  if (!list.length || working) return;
  const pick = {};
  for (const p of list) pick[p.id] = p.chosen != null ? p.chosen : p.played != null || p.installed;
  if (opts.tick) pick[opts.tick] = true;
  picker = { pick, then: opts.then };
  $("p-ver").textContent = check.version;
  renderPicker();
  $("picker").classList.remove("hidden");
}

function closePicker() {
  picker = null;
  $("picker").classList.add("hidden");
}

function renderPicker() {
  const box = $("p-rows");
  box.textContent = "";
  let keep = 0, free = 0;
  for (const p of games()) {
    const on = !!picker.pick[p.id];
    if (on) keep += p.bytes; else free += p.bytes;
    const look = lookOf(p);
    const row = el("div", "prow");
    const logo = el("div", "plogo");
    if (look.generic) logo.append(el("span", null, p.title));
    else {
      const img = el("img", look.pix ? "pix" : null);
      img.src = "games/" + p.id + "-logo.png";
      img.alt = "";
      logo.append(img);
    }
    const l = el("div", "pl");
    l.append(el("b", null, p.title), el("span", p.played != null ? "played" : null, p.played != null ? "You've played it" : "Never opened"));
    const tg = el("button", "toggle");
    tg.setAttribute("role", "switch");
    tg.setAttribute("aria-checked", String(on));
    tg.setAttribute("aria-label", "Keep " + p.title);
    tg.append(el("i"));
    tg.onclick = () => { picker.pick[p.id] = !on; renderPicker(); };
    row.append(logo, l, el("span", "sz", fmtBytes(p.bytes)), tg);
    box.append(row);
  }
  $("p-sum").textContent = "Keeps " + fmtBytes(keep) + " · frees " + fmtBytes(free);
}

async function savePicker(all) {
  if (!picker) return;
  const choice = {};
  for (const p of games()) choice[p.id] = all || !!picker.pick[p.id];
  const then = picker.then;
  closePicker();
  try {
    await api.invoke("set_packs", { packs: choice, picked: true });
  } catch (e) {
    toast(e);
    return;
  }
  if (check) check.needsPick = false;
  for (const id in choice) assume(id, choice[id]);
  if (then === "update") await doUpdate();
  else {
    toast(all ? "All games kept. Update installs them." : "Saved. Change it any time on the Games tab.", true);
    await refreshAll();
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
  $("set-tray").setAttribute("aria-checked", String(!!s.tray));
  $("set-auto").setAttribute("aria-checked", String(!!s.autoInstall));
  $("set-autostart").setAttribute("aria-checked", String(!!s.autostart));
  $("row-auto").classList.toggle("off", !s.tray);           // (both need tray mode)
  $("row-autostart").classList.toggle("off", !s.tray);
  for (const [id, key] of [["set-code", "launchCode"], ["set-cmd", "playCommand"], ["set-source", "source"]]) {
    const f = $(id);
    if (document.activeElement !== f) f.value = s[key] || "";
  }
  $("set-source").placeholder = status.source;
  renderSync();
  $("about").textContent = "DisasterZone Launcher " + status.launcherVersion + " · releases from " + status.source;
}

function wireSettings() {
  $("btn-pick").onclick = pickFolder;
  $("foot-path").onclick = () => (status && status.flavorDir ? api.invoke("open_folder", { kind: "addons" }).catch(toast) : showTab("settings"));
  $("keep-minus").onclick = () => saveSettings({ keepBackups: Math.max(1, status.settings.keepBackups - 1) });
  $("keep-plus").onclick = () => saveSettings({ keepBackups: Math.min(10, status.settings.keepBackups + 1) });
  $("set-check").onclick = () => saveSettings({ checkOnStart: !status.settings.checkOnStart });
  $("set-tray").onclick = () => saveSettings({ tray: !status.settings.tray });
  $("set-auto").onclick = () => { if (status.settings.tray) saveSettings({ autoInstall: !status.settings.autoInstall }); };
  $("set-autostart").onclick = () => { if (status.settings.tray) saveSettings({ autostart: !status.settings.autostart }); };
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
// Discord sync (sync.rs): this PC's game syncs with the guild's bot server once linked with a /launcher code
// ---------------------------------------------------------------------------------------------------------------

let sync = { linked: false, user: "", server: "", last: null, waiting: 0, error: null };

function ago(t) {
  const s = Math.max(0, Math.floor(Date.now() / 1000) - t);
  return s < 60 ? "just now" : s < 3600 ? Math.floor(s / 60) + " min ago" : s < 86400 ? Math.floor(s / 3600) + " h ago" : Math.floor(s / 86400) + " days ago";
}

function renderSync() {
  const st = $("sync-state");
  $("row-link").classList.toggle("hidden", sync.linked);
  $("sync-linked-btns").classList.toggle("hidden", !sync.linked);
  if (!sync.linked) {
    st.textContent = sync.server ? "Not linked." : "No bot server is set yet.";
    st.style.color = "";
  } else if (sync.error) {
    st.textContent = "Linked as " + sync.user + " · " + sync.error;
    st.style.color = "rgb(230, 110, 100)";
  } else {
    const waiting = sync.waiting ? " · " + sync.waiting + " change" + (sync.waiting === 1 ? "" : "s") + " from Discord waiting for the game" : "";
    st.textContent = "Linked as " + sync.user + (sync.last ? " · synced " + ago(sync.last) : " · not synced yet") + waiting;
    st.style.color = "rgb(77, 181, 99)";
  }
  const f = $("set-server");
  if (document.activeElement !== f) f.value = status.settings.syncServer || "";
  f.placeholder = sync.server || "not set";
}

async function syncCall(cmd, args) {
  try {
    sync = await api.invoke(cmd, args);
  } catch (e) {
    toast(e);
    try { sync = await api.invoke("sync_status"); } catch (e2) { /* (shown as it was) */ }
  }
  renderSync();
}

function wireSync() {
  const code = $("set-linkcode");
  const link = () => {
    const v = code.value.trim();
    if (!v) { code.focus(); return; }
    $("btn-link").disabled = true;
    syncCall("sync_link", { code: v }).then(() => { $("btn-link").disabled = false; if (sync.linked) code.value = ""; });
  };
  $("btn-link").onclick = link;
  code.addEventListener("keydown", (e) => { if (e.key === "Enter") link(); });
  $("btn-sync").onclick = () => syncCall("sync_run");
  $("btn-unlink").onclick = () => syncCall("sync_unlink");
  const f = $("set-server");
  f.addEventListener("change", () => {
    if ((status.settings.syncServer || "") !== f.value.trim()) saveSettings({ syncServer: f.value }).then(() => syncCall("sync_status"));
  });
  f.addEventListener("keydown", (e) => { if (e.key === "Enter") f.blur(); });
}

// ---------------------------------------------------------------------------------------------------------------
// Tabs + start
// ---------------------------------------------------------------------------------------------------------------

function showTab(name) {
  for (const t of document.querySelectorAll(".tab")) t.classList.toggle("active", t.dataset.tab === name);
  for (const p of document.querySelectorAll(".page")) p.classList.toggle("active", p.id === "page-" + name);
  if (name === "backups") renderBackups();
  if (name === "games") {
    $("games-new").classList.add("hidden");
    try { localStorage.setItem("dz.gamesSeen", "1"); } catch (e) { /* (the badge just shows again) */ }
  }
}

function wireGames() {
  try { if (!localStorage.getItem("dz.gamesSeen")) $("games-new").classList.remove("hidden"); } catch (e) { /* (no badge) */ }
  $("p-save").onclick = () => savePicker(false);
  $("p-all").onclick = () => savePicker(true);
  // (dismissed: nothing saved; Update asks again)
  $("picker").addEventListener("click", (e) => { if (e.target === $("picker")) closePicker(); });
  document.addEventListener("keydown", (e) => { if (e.key === "Escape" && picker) closePicker(); });
  document.addEventListener("click", (e) => {
    if (menuOpen && !e.target.closest(".gmenu") && !e.target.closest("[data-menu]")) { menuOpen = null; placeMenu(); }
  });
  const close = () => { if (menuOpen && performance.now() - menuAt > 300) { menuOpen = null; placeMenu(); } };
  document.querySelector(".gscroll").addEventListener("scroll", close);
  window.addEventListener("resize", close);
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
  wireSync();
  wireGames();
  await api.listen("sync-status", (e) => { sync = { ...sync, ...e.payload }; if (status) renderSync(); });
  setInterval(() => { if (status) renderSync(); }, 30000);   // ("synced 2 min ago" moves on)
  await api.listen("progress", (e) => { if (working) showProgress(e.payload); });
  // the tray's background check installed something or found a new release
  await api.listen("state-changed", () => { if (!working) refreshAll(); });
  try {
    sync = await api.invoke("sync_status");
    await loadStatus();
  } catch (e) {
    toast(e);
    return;
  }
  if (status.settings.checkOnStart || !status.installed || !status.installed.present) await runCheck();
  else renderHero();
  renderBackups();
  checkLauncherUpdate();
  if (DEMO) { const tab = new URLSearchParams(location.search).get("tab"); if (tab) showTab(tab); }
  setInterval(() => { if (!working && !checking) runCheck(); }, RECHECK_MS);
}

start();

// ---------------------------------------------------------------------------------------------------------------
// Demo data (a plain browser, no Tauri)
// ---------------------------------------------------------------------------------------------------------------

// The demo is 1.5.4 (every game inside DisasterZone) meeting 1.5.5, the first release with game packs: the picker
// opens, Install / Remove / the 3D rooms switch / Restore all work on made-up folders. ?state=updated starts after
// the update (four games kept), for screenshots.
function demoApi() {
  const MB = 1048576;
  const NEW = "1.5.5";
  const params = new URLSearchParams(location.search);
  const updated = params.get("state") === "updated";
  const settings = { wowRoot: "C:\\World of Warcraft", flavor: "_anniversary_", channel: "stable", seenSeq: {}, hold: false,
    keepBackups: 3, checkOnStart: true, source: null, launchCode: "", playCommand: "", tray: false, autoInstall: false, autostart: false,
    syncServer: "", syncUser: "", packs: {}, packsPicked: updated };
  // the release (sizes measured for the mockup); upd: what 1.5.5 changes in a copy that is already there
  const release = [
    ["core", "DisasterZone", "core", "DisasterZone", "Loot council, raid plans, guild members and games for <Casual Disaster>.", 47.7, 17.9, 512, 4.0],
    ["rooms", "DisasterZone_Rooms", "extra", "3D rooms", "", 19.0, 13.6, 64, 0],
    ["azeromon", "DisasterZone_Azeromon", "game", "Azeromon", "Tame and battle the beasts of Elwynn.", 149.8, 19.5, 2210, 0],
    ["holdtheline", "DisasterZone_HoldTheLine", "game", "Hold the Line", "Fifteen minutes. One class. The whole Scourge.", 33.2, 8.5, 820, 0],
    ["illidan", "DisasterZone_Illidan", "game", "The Binding of Illidan", "Five floors of the Black Temple, new every run.", 1.1, 0.2, 40, 0],
    ["bingo", "DisasterZone_Bingo", "game", "Council Bingo", "Squares fill in as the raid happens.", 0.2, 0.1, 9, 0],
    ["chess", "DisasterZone_Chess", "game", "Karazhan Chess", "Play Medivh's echo, or a guildmate.", 0.2, 0.1, 12, 0],
    ["trainer", "DisasterZone_Trainer", "game", "Raid Trainer", "Practise each boss in 3D before the pull.", 0.2, 0.1, 14, 0],
  ].map(([id, folder, kind, title, tagline, mb, gz, files, upd]) => ({ id, folder, kind, title, tagline, required: kind === "core",
    defaultOn: kind !== "game", bytes: Math.round(mb * MB), gz: Math.round(gz * MB), files, upd: Math.round(upd * MB) }));
  const OLD_CORE = Math.round(232.5 * MB);  // (1.5.4 carries every game inside DisasterZone)
  // folder -> version on disk
  let disk = updated
    ? { DisasterZone: NEW, DisasterZone_Rooms: NEW, DisasterZone_Azeromon: NEW, DisasterZone_HoldTheLine: NEW, DisasterZone_Bingo: NEW, DisasterZone_Chess: NEW }
    : { DisasterZone: "1.5.4", DisasterZone_Rooms: "1.5.4" };
  if (updated) for (const p of release) if (p.kind === "game") settings.packs[p.id] = !!disk[p.folder];
  const now = Math.floor(Date.now() / 1000);
  const played = { azeromon: now - 2 * 86400, illidan: now - 9 * 86400, bingo: now - 86400 - 3600, chess: now - 3 * 3600 };
  const backups = [{ id: "20261016-201544-1.5.3", version: "1.5.3", build: "f60fe8911a371447", time: "", reason: "update", savedVariables: 3,
    folders: ["DisasterZone", "DisasterZone_Rooms"], added: [], bytes: Math.round(151 * MB), snap: { DisasterZone: "1.5.3", DisasterZone_Rooms: "1.5.3" } }];
  // the real changelog of earlier releases (demo-notes.json, from a published manifest), under example lines for 1.5.5
  const notes = { changelog: [{ version: NEW, date: "19 October 2026", title: "Pick your games", sections: [
    { heading: "Games as packs", kind: "new", lead: "Every game now installs on its own from the launcher's Games tab. Your progress stays when you remove one." },
    { heading: "Smaller backups", kind: "changed", lead: "A backup now keeps only the folders an update changes." },
    { heading: "Games hub", kind: "changed", lead: "Games you haven't installed still have a poster that says where to get them." }] }] };
  const notesReady = fetch("demo-notes.json").then((r) => r.json()).then((n) => { notes.changelog.push(...n.changelog); }).catch(() => {});
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  let progressFn = null;
  let demoSync = { linked: false, user: "", server: "https://bot.example", last: null, waiting: 0, error: null };

  const buildOf = (v) => (v === NEW ? "c0a1b2c3d4e5f607" : "4ad16fd3a1b2c3d4");
  const wanted = (p) => p.required || (settings.packs[p.id] ?? (p.defaultOn || !!disk[p.folder]));
  const action = (p) => (wanted(p)
    ? (!disk[p.folder] ? "install" : disk[p.folder] !== NEW && (p.kind === "core" || p.upd) ? "update" : "same")
    : (disk[p.folder] ? "remove" : ""));
  const diskBytes = (p) => (!disk[p.folder] ? 0 : p.id === "core" && disk[p.folder] !== NEW ? OLD_CORE : p.bytes);
  const download = (p) => ({ install: p.gz, update: p.upd }[action(p)] || 0);
  const backupId = (v) => {
    const d = new Date(), z = (n, w = 2) => String(n).padStart(w, "0");
    return d.getUTCFullYear() + z(d.getUTCMonth() + 1) + z(d.getUTCDate()) + "-" + z(d.getUTCHours()) + z(d.getUTCMinutes()) +
      z(d.getUTCSeconds()) + "." + z(d.getUTCMilliseconds(), 3) + "-" + v;
  };
  const backup = (reason, folders, added) => {
    backups.unshift({ id: backupId(disk.DisasterZone), version: disk.DisasterZone, build: buildOf(disk.DisasterZone), time: "", reason,
      savedVariables: 3, folders, added, snap: { ...disk },
      bytes: folders.reduce((n, f) => n + diskBytes(release.find((p) => p.folder === f)), 0) });
    backups.splice(settings.keepBackups + 1);
    return backups[0].id;
  };
  const installedNow = () => ({ present: !!disk.DisasterZone, version: disk.DisasterZone, build: buildOf(disk.DisasterZone), assetPack: 16,
    dev: false, linked: false, rooms: !!disk.DisasterZone_Rooms,
    packs: release.filter((p) => p.kind !== "core" && disk[p.folder]).map((p) => ({ id: p.id, folder: p.folder, kind: p.kind, title: p.title, linked: false })) });

  return {
    async invoke(cmd, args) {
      await wait(250);
      switch (cmd) {
        case "status": return { settings: { ...settings, packs: { ...settings.packs } }, installs: [{ root: settings.wowRoot, flavors: ["_anniversary_", "_classic_era_", "_retail_"] }],
          installed: installedNow(), flavorDir: settings.wowRoot + "\\" + settings.flavor, wowRunning: false, launcherVersion: "0.4.0",
          source: "https://raw.githubusercontent.com/tsumilo/DisasterZone-releases/main/" };
        case "check": {
          await notesReady;
          const packs = release.map((p) => ({ id: p.id, folder: p.folder, kind: p.kind, title: p.title, tagline: p.tagline, required: p.required,
            defaultOn: p.defaultOn, installed: !!disk[p.folder], wanted: wanted(p), chosen: settings.packs[p.id] ?? null, action: action(p),
            files: p.files, bytes: p.bytes, gz: p.gz, downloadBytes: download(p), diskBytes: diskBytes(p), played: played[p.id] ?? null }));
          const busy = packs.filter((p) => p.action === "install" || p.action === "update");
          return { version: NEW, build: buildOf(NEW), seq: 12, channel: settings.channel, assetPack: 16, published: "2026-10-19T18:00:00Z",
            notes, files: release.reduce((n, p) => n + p.files, 0), changed: busy.reduce((n, p) => n + (p.action === "install" ? p.files : 23), 0),
            removed: 0, downloadBytes: packs.reduce((n, p) => n + p.downloadBytes, 0), upToDate: packs.every((p) => p.action === "same" || p.action === ""),
            older: false, hold: settings.hold, zipUrl: null, packs, needsPick: !settings.packsPicked };
        }
        case "update": {
          const total = release.reduce((n, p) => n + download(p), 0);
          for (let i = 0; i <= 12; i++) { progressFn && progressFn({ payload: { step: "download", done: Math.round((i * total) / 12), total } }); await wait(140); }
          for (let i = 0; i <= 4; i++) { progressFn && progressFn({ payload: { step: "stage", done: i * 120, total: 480 } }); await wait(110); }
          progressFn && progressFn({ payload: { step: "swap", done: 0, total: 1 } });
          await wait(300);
          const moves = release.map((p) => ({ folder: p.folder, action: action(p) })).filter((m) => m.action && m.action !== "same");
          const id = backup("update", moves.filter((m) => m.action !== "install").map((m) => m.folder), moves.filter((m) => m.action === "install").map((m) => m.folder));
          for (const m of moves) { if (m.action === "remove") delete disk[m.folder]; else disk[m.folder] = NEW; }
          settings.hold = false;
          return { version: NEW, downloaded: total, changed: 23, removed: 0, backup: id, folders: moves };
        }
        case "set_packs": Object.assign(settings.packs, args.packs); if (args.picked != null) settings.packsPicked = args.picked; return { ...settings };
        case "remove_pack": {
          const p = release.find((x) => x.id === args.id);
          let id = null;
          if (p && disk[p.folder]) { id = backup("remove", [p.folder], []); delete disk[p.folder]; }
          settings.packs[args.id] = false;
          return id;
        }
        case "backups": return backups.slice();
        case "restore": {
          const b = backups.find((x) => x.id === args.id);
          backup("restore", Object.keys(disk), Object.keys(b.snap).filter((f) => !disk[f]));
          disk = { ...b.snap };
          for (const p of release) if (p.kind !== "core" && disk[p.folder]) settings.packs[p.id] = true;
          settings.hold = true;
          return null;
        }
        case "set_settings": for (const k in args) if (args[k] != null) settings[k] = args[k]; return { ...settings };
        case "set_wow_path": if (args.flavor) settings.flavor = args.flavor; return { ...settings };
        case "sync_status": return { ...demoSync };
        case "sync_link":
          if (args.code.toUpperCase() !== "DEMO1234") throw "That code is unknown or older than 15 minutes: type /launcher in Discord for a new one. (Demo: DEMO1234)";
          demoSync = { ...demoSync, linked: true, user: "Tsumil", last: Math.floor(Date.now() / 1000), waiting: 2 };
          return { ...demoSync };
        case "sync_run": demoSync.last = Math.floor(Date.now() / 1000); return { ...demoSync };
        case "sync_unlink": demoSync = { ...demoSync, linked: false, user: "", last: null, waiting: 0 }; return { ...demoSync };
        case "play": throw "Demo: Play would start Battle.net here.";
        case "open_folder": throw "Demo: this would open " + (args.folder || "the folder") + ".";
      }
      throw "unknown command " + cmd;
    },
    async listen(ev, fn) { if (ev === "progress") progressFn = fn; },
    async pickFolder() { return null; },
    minimize() {},
    close() {},
  };
}
