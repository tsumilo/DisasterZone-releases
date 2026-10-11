# DisasterZone Launcher

Desktop app (Windows, macOS, Linux) that installs and updates the DisasterZone addon — both folders,
`DisasterZone` and `DisasterZone_Rooms` — and keeps backups you can go back to. Tauri 2: Rust in `src-tauri/`,
the window is plain HTML/CSS/JS in `ui/` (no bundler).

## How releases reach it

1. `dev\tools\release.py full` — the release zip, as before.
2. `dev\tools\release.py publish [--channel stable|beta]` — writes a signed manifest (`channels/<channel>.json`
   + `.sig`) and content-addressed objects into the public releases repository, pushes, and attaches the zip to a
   GitHub release. Format: `dev\tools\launchlib.py`.
3. The launcher reads `source.txt` + `channels/<channel>.json`, checks the Ed25519 signature against
   `release_key.json` (the public half of `dev\keys\launcher_key.json`), downloads only the files that differ,
   builds the new folders under `<game>\Interface\DZLauncher\stage`, backs up the old folders and the
   DisasterZone saved variables, and swaps them in.

A development copy (BuildInfo id `"dev"`, or a linked folder) is never touched.

## Building the launcher itself

- Here: `npm install`, then `npx tauri dev` (window with live reload) or `npx tauri build` (installers in
  `src-tauri\target\release\bundle`).
- All three OSes: `dev\tools\release.py launcher-push` copies this folder into the releases repository and pushes
  tag `launcher-v<version>`; `ci/launcher.yml` builds and publishes the installers. Raise the version in
  `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` and `package.json` first.

## Tests

- `cd src-tauri && cargo test` — core: signatures, unsafe paths, install/update/repair, bad objects, dev copies,
  backups, restore, pruning, WoW folder detection.
- `DZ_PUB=<folder from release.py publish --out> cargo test --test e2e_published` — installs a real published tree.
- `dev\tests\launcher\test_publish.py` — the Python publisher.
- Opening `ui/index.html` in a browser runs the window on demo data.
