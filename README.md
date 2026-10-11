<p align="center">
  <img src="assets/banner.png" alt="DisasterZone — BiS lists, Loot Council, Raids, Strategy boards" width="100%">
</p>

<p align="center">
  <b>The guild addon of &lt;Casual Disaster&gt;</b> for the World of Warcraft TBC Anniversary realms.<br>
  <sub>Best-in-slot lists · a Loot Council that sees them · raid sign-ups and teams · strategy boards · and a few games on the side</sub>
</p>

<p align="center"><img src="assets/divider.png" width="70%" alt=""></p>

<h2 align="center">Get the launcher</h2>

<p align="center">
  One click installs DisasterZone, keeps it up to date, and keeps backups you can go back to.
</p>

<p align="center">
  <a href="https://github.com/tsumilo/DisasterZone-releases/releases/latest"><img src="assets/btn-windows.png" width="240" alt="Download for Windows"></a>&nbsp;
  <a href="https://github.com/tsumilo/DisasterZone-releases/releases/latest"><img src="assets/btn-macos.png" width="240" alt="Download for macOS"></a>&nbsp;
  <a href="https://github.com/tsumilo/DisasterZone-releases/releases/latest"><img src="assets/btn-linux.png" width="240" alt="Download for Linux"></a>
</p>

<p align="center"><sub>
  <b>Windows</b> may say <i>Windows protected your PC</i>: click <i>More info</i>, then <i>Run anyway</i>.
  &nbsp;·&nbsp; <b>macOS</b>: the first time, right-click the app and choose <i>Open</i>.
  &nbsp;·&nbsp; <b>Linux</b>: make the <code>.AppImage</code> executable, then run it.
</sub></p>

<table align="center">
  <tr>
    <td align="center" width="25%"><b>Finds the game</b><br><sub>Windows, macOS, and Linux under Wine, Lutris, Bottles or Steam</sub></td>
    <td align="center" width="25%"><b>Small updates</b><br><sub>Downloads only the files that changed, and checks every one</sub></td>
    <td align="center" width="25%"><b>Safe to undo</b><br><sub>Backs up the addon and your saved variables before each update</sub></td>
    <td align="center" width="25%"><b>Signed</b><br><sub>Only installs releases signed by the DisasterZone key</sub></td>
  </tr>
</table>

<p align="center"><img src="assets/divider.png" width="70%" alt=""></p>

<h2 align="center">Install by hand</h2>

<p align="center">
  <a href="https://github.com/tsumilo/DisasterZone-releases/releases"><img src="assets/btn-zip.png" width="280" alt="Download the addon zip"></a>
</p>

<p align="center">
  Take <code>DisasterZone-&lt;version&gt;.zip</code> from the newest <b>DisasterZone</b> release and unzip it into<br>
  <code>World of Warcraft\_anniversary_\Interface\AddOns</code>. It holds two folders, <code>DisasterZone</code> and <code>DisasterZone_Rooms</code>.<br>
  <sub>Small fixes also arrive in game, from guildmates who already have them, no download needed.</sub>
</p>

<p align="center"><img src="assets/divider.png" width="70%" alt=""></p>

<details>
  <summary><b>What is in this repository</b></summary>
  <br>

  - `channels/` — the signed release lists the launcher reads (`stable`, `beta`).
  - `objects/` — the addon's files, one per version of each file, named by their SHA-256.
  - `launcher/` — the launcher's source (Tauri: Rust + HTML); `.github/workflows/launcher.yml` builds it for every platform.
  - Releases — each addon version as a zip, and each launcher version with its installers.
</details>

<p align="center">
  <br>
  <img src="launcher/src-tauri/icons/128x128.png" width="56" alt="DisasterZone seal"><br>
  <sub>Made for &lt;Casual Disaster&gt;</sub>
</p>
