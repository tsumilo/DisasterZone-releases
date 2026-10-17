# DisasterZone Launcher

Desktop app (Windows, macOS, Linux) that installs and updates the DisasterZone addon and keeps backups you can go
back to. A release is a set of folders, one per pack (manifest format 2, launcher 0.4): the core `DisasterZone`,
the Extra `DisasterZone_Rooms` (3D rooms, on by default) and one `DisasterZone_<Game>` per game. The Games tab
installs and removes games; the first release with games asks once which to keep (played ones ticked, from
`DisasterZoneDB.games[id].opened` in the saved variables). Tauri 2: Rust in `src-tauri/`, the window is plain
HTML/CSS/JS in `ui/` (no bundler; card art in `ui/games/`).

## How releases reach it

1. `dev\tools\release.py full` — the release zip, as before.
2. `dev\tools\release.py publish [--channel stable|beta]` — writes a signed manifest (`channels/<channel>.json`
   + `.sig`) and content-addressed objects into the public releases repository, pushes, and attaches the zip to a
   GitHub release. Format: `dev\tools\launchlib.py`.
3. The launcher reads `source.txt` + `channels/<channel>.json`, checks the Ed25519 signature against
   `release_key.json` (the public half of `dev\keys\launcher_key.json`), downloads only the files that differ,
   builds the folders that change under `<game>\Interface\DZLauncher\stage`, backs up those folders and the
   DisasterZone saved variables, and swaps them in. Folders that don't change aren't touched or backed up, so a
   restore applies every newer backup first (newest down to the one picked). `DisasterZone_*` folders a release
   doesn't list (the launcher's own `DisasterZone_Bridge`) are never touched.

A development copy (BuildInfo id `"dev"`, or `DisasterZone` or any `DisasterZone_*` folder being a link) is never touched.

## Building the launcher itself

- Here: `npm install`, then `npx tauri dev` (window with live reload) or `npx tauri build` (installers in
  `src-tauri\target\release\bundle`). `tauri build` signs the self-update files, so it needs
  `TAURI_SIGNING_PRIVATE_KEY` (contents of `dev\keys\updater.key`) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
  (`dev\keys\updater_password.txt`) in the environment; CI has both as repository secrets.
- Self-update: the launcher reads `latest.json` from the release `launcher-latest` (set in `tauri.conf.json`,
  public key from `dev\keys\updater.key.pub`). Losing `updater.key` means members must reinstall the launcher by hand.
  The window looks at start, on its 30-minute re-check, when it comes back from the tray, on Check again and when
  a release needs a newer launcher (then the hero's button updates it, or opens the releases page if none is found).
  Tray mode's 5-minute check installs it by itself (that setting on, window closed) or shows the window's bar at once.
  Settings > About shows the newest check and why it failed.
- All three OSes: `dev\tools\release.py launcher-push` copies this folder into the releases repository and pushes
  tag `launcher-v<version>`; `ci/launcher.yml` builds and publishes the installers. Raise the version in
  `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` and `package.json` first.

## Tests

- `cd src-tauri && cargo test` — core: signatures, unsafe paths, install/update/repair, bad objects, dev copies,
  backups, restore, pruning, WoW folder detection.
- `DZ_PUB=<folder from release.py publish --out> cargo test --test e2e_published` — installs a real published tree
  (every pack). `DZ_PUB_KEY=<public hex>` checks a tree signed with a test key instead of `release_key.json`.
- `dev\tests\launcher\test_publish.py` — the Python publisher.
- Serving `ui/` and opening it in a browser runs the window on demo data: 1.5.4 meeting the first release with game
  packs (the picker opens); `?state=updated` starts after that update; `?tab=games` opens a tab.
  `?launcher=ready|tray|fail|gate|gate-none|gate-fail` shows the launcher's own update states (see demoApi in app.js).
