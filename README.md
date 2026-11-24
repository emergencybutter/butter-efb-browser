# EFB Web Browser for MSFS 2024

A full web browser for the Microsoft Flight Simulator 2024 EFB tablet.

The sim's HTML engine (Coherent GT) cannot run modern websites, and most sites refuse to be framed,
so the browser is split in two:

```
 ┌──────────── PC ────────────┐                       ┌──────── MSFS 2024 EFB ────────┐
 │ companion (Rust, 1 .exe)   │   ws://127.0.0.1:8737  │ Browser app (TypeScript)      │
 │  headless Edge/Chrome  ────┼── JPEG frames ───────▶ │  shows frames                 │
 │  (real Chromium engine)  ◀─┼── mouse / keys / nav ──┤  toolbar, URL bar, keyboard   │
 └────────────────────────────┘                       └───────────────────────────────┘
```

- **companion/**: a small Rust program (single ~4 MB .exe, no runtime to install) that runs a real
  Chromium (the Microsoft Edge already installed on Windows, no download) in headless mode, streams
  the page through the DevTools screencast and injects input. MSFS starts and stops it (EXE.xml).
  Cookies and logins persist in `%LOCALAPPDATA%\EFB Browser Companion\profile`.
- **efb/**: the EFB app (MSFS 2024 SDK DevMode project + build scripts).

## Features

- Address bar (URLs or search terms), back / forward / reload / stop / home
- Tap to click, drag to scroll (tablet style) or raw mouse mode, mouse-wheel scrolling, hover
- Typing into web pages: when a text field is focused on the page a keyboard bar appears on the EFB;
  physical keyboard and the sim's OS/VR keyboard both work. The keyboard button forces typing mode.
- `<select>` dropdowns shown as an EFB picker (headless Chromium does not render native popups)
- `alert` / `confirm` / `prompt` dialogs shown on the EFB
- Popups and `target=_blank` links open as tabs (the tab button closes the current tab)
- Zoom (50 – 200 %), mobile-site mode, JPEG quality, frame transport (WebSocket or HTTP)
- Audio plays on the PC (YouTube, LiveATC...)
- Streaming pauses when the app is not visible on the EFB

## Install

Run `EFB-Browser-Setup-<version>.msi` from the [latest release](https://github.com/emergencybutter/butter-efb-browser/releases/latest) (requires Microsoft Edge or Google Chrome, and MSFS 2024 started
at least once). It installs per user, without admin rights:

- the companion in `%LOCALAPPDATA%\Programs\EFB Browser Companion\`,
- the EFB app in the sim's Community folder (found through `UserCfg.opt`, Store and Steam versions),
- an entry in the sim's `EXE.xml` (backed up first) so that **MSFS starts the companion** in the
  background and it **quits when the sim exits**.

If Windows denies access to the sim's folders (typically a Store/Xbox install whose data was moved to
another drive, where the package folder is a junction into a protected `WpSystem` folder), setup asks
for administrator rights for that step only (`--install-sim` / `--uninstall-sim`, not in silent installs).

Quit the sim before running setup (it asks): the sim only picks up Community packages when it starts.
Then start a flight, open the EFB and launch the **Browser** app. Uninstall from Windows Settings > Apps,
which removes the `EXE.xml` entry and the Community package (browser data is kept in
`%LOCALAPPDATA%\EFB Browser Companion\`, with `companion.log` and `install.log`).

The setup only copies the .exe and runs `efb-browser-companion.exe --install` (the EFB package is
embedded in the .exe); `--uninstall` undoes it. Both also work without the installer. The MSI also
passes `--replace-inno` (takes over an install made by the older `EFB-Browser-Setup-*.exe`, so it is
upgraded in place) and `--error-dialog` (shows why setup failed, unless installing silently).

## Building

Requirements: [Rust](https://rustup.rs), Node.js 18+, and [Inno Setup 6](https://jrsoftware.org/isinfo.php)
for the installer (`winget install JRSoftware.InnoSetup`).

```
powershell -ExecutionPolicy Bypass -File installer\build.ps1     # -> dist\EFB-Browser-Setup-<version>.msi (and .exe)
```

It builds the EFB app (`npm run build`), assembles its package (`node efb\pack.js`), builds the companion
(which embeds that package) and the installers: `installer\EFBBrowser.wxs` (MSI, the one released) and `installer\EFBBrowser.iss`
(Inno Setup). Bump the version in `companion/Cargo.toml`.

Without installing, for development:

1. **Companion**: `companion\start-companion.bat` (builds it on first run, then runs it with a console).
   Test it without the sim at <http://127.0.0.1:8737/>.
2. **EFB app**:
   ```
   cd efb\PackageSources\BrowserApp
   npm install
   npm run build
   cd ..\..
   node pack.js --install      # copies efb\Packages\buttersim-efb-browser to your Community folder
   ```
   (Or open `efb\EFBBrowserProject.xml` in the sim's DevMode Project Editor and build it there.)
   Rebuild the companion afterwards if you want `--install` / the installer to ship the new EFB app.

## Companion options

`start-companion.bat [options]`, `efb-browser-companion.exe [options]` or `cargo run --release -- [options]`
(in `companion/`). `--help` lists them:

| Option | Env var | Default | |
|---|---|---|---|
| `--port 8737` | `EFB_BROWSER_PORT` | 8737 | WebSocket / HTTP port |
| `--host 127.0.0.1` | `EFB_BROWSER_HOST` | 127.0.0.1 | `0.0.0.0` to accept other machines (then set the address in the EFB settings) |
| `--home <url>` | `EFB_BROWSER_HOME` | butr.cc | Home page |
| `--search <url with %s>` | `EFB_BROWSER_SEARCH` | Google | Search engine |
| `--browser <exe>` | `EFB_BROWSER_EXE` | Edge, then Chrome | Chromium executable |
| `--profile <dir>` | `EFB_BROWSER_PROFILE` | `%LOCALAPPDATA%\EFB Browser Companion\profile` | Persistent browser profile |
| `--render-scale 2` | `EFB_BROWSER_RENDER_SCALE` | 2 | Internal render DPI; text stays sharp up to this zoom |
| `--show` | `EFB_BROWSER_SHOW=true` | off | Show the browser window (debugging) |
| `--verbose` | `EFB_BROWSER_VERBOSE=true` | off | Log failed commands |
| `--exit-with-sim` | | off | Background mode used by `EXE.xml`: no console, logs to `companion.log`, quits when MSFS exits |

## Development

- `companion/`: `cargo run -- --port 8738 --verbose` runs a second instance next to a running one
  (use another `--profile` too: a browser profile can only be used by one browser at a time).
  `cargo test` runs the unit tests. Sources: `host.rs` (tabs, screencast, input), `cdp.rs` (DevTools
  protocol client), `server.rs` (HTTP/WebSocket), `install.rs` (`--install` / `--uninstall`, EXE.xml),
  `win.rs` (console, job object, process lookup), `assets/` (page script and debug viewer, embedded in the .exe).
  `EFB_BROWSER_SIM_PROCESS=notepad.exe` makes `--exit-with-sim` watch another process, to test without the sim.
- `efb/PackageSources/BrowserApp`: `npm run watch` rebuilds on change.
- **Dev harness**: `npm run harness`, then open `dev/harness.html` in Edge/Chrome (with the companion running).
  It mounts the real `BrowserPage` component outside the sim with the Coherent API stubbed, so you can
  iterate on the UI without restarting MSFS. Then run `node pack.js --install` to update the Community package.
- In the sim, use the Coherent GT debugger (DevMode) to inspect the EFB.

### Protocol (JSON over WebSocket)

EFB → companion: `config {width,height,zoom,mobile,quality,transport}`, `ack {id}`, `pause`, `resume`,
`navigate {url}`, `home`, `back`, `forward`, `reload`, `stop`, `closeTab`,
`mouse {action: down|up|move|click, x, y, button}` (x/y normalized 0..1), `wheel {x,y,dx,dy | ndx,ndy}`,
`key {key, modifiers}`, `text {text}`, `select {index}`, `dialog {accept,text}`.

Companion → EFB: `hello`, `frame {id, data?}` (one frame in flight per client, newest wins; with the
HTTP transport the image is fetched from `/frame/<id>.jpg`), `state {url,title,loading,canGoBack,canGoForward,tabs}`,
`focus {editable}`, `select {options,selectedIndex}`, `dialog {dialog}`, `notice {message}`.

## Known limitations / ideas

- Video plays at the screencast frame rate (roughly 10–30 fps depending on the page and the PC).
- Not tested in VR yet; the OS keyboard path (`SetInputTextFromOS`) is wired but unverified.
- If frames don't display in the sim, switch **Frame transport** to HTTP in the settings.
- The installer and the .exe are not code-signed: Windows SmartScreen warns on first run.
- Ideas: bookmarks, a tab list, pinch zoom, binary WebSocket frames.
