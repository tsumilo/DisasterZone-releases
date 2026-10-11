# DisasterZone

Releases of the DisasterZone World of Warcraft addon, and the **DisasterZone Launcher** that installs and updates it.

## Install

**With the launcher (recommended):** take the newest *DisasterZone Launcher* release from
[Releases](https://github.com/tsumilo/DisasterZone-releases/releases) and download the file for your computer:

- **Windows:** `…-setup.exe`. Windows may say "Windows protected your PC": click *More info*, then *Run anyway*.
- **macOS:** `.dmg`. The first time, right-click the app and choose *Open*.
- **Linux:** `.AppImage` (make it executable, then run it) or `.deb`.

The launcher finds World of Warcraft, installs the addon, and keeps it (and itself) up to date. Before every
update it backs up the addon and your DisasterZone saved variables, so you can always go back.

**By hand:** download `DisasterZone-<version>.zip` from the newest addon release and unzip it into
`World of Warcraft\_anniversary_\Interface\AddOns` (it contains two folders, `DisasterZone` and `DisasterZone_Rooms`).

## What is in this repository

- `channels/` — the signed release lists the launcher reads (`stable`, `beta`).
- `objects/` — the addon's files, one per version of each file, named by their SHA-256.
- `launcher/` — the launcher's source; `.github/workflows/launcher.yml` builds it.
