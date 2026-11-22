import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
export const ROOT_DIR = path.resolve(__dirname, '..');

/** Parse `--key value` / `--flag` command line arguments. */
function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (!a.startsWith('--')) continue;
    const key = a.slice(2);
    const next = argv[i + 1];
    if (next === undefined || next.startsWith('--')) {
      out[key] = true;
    } else {
      out[key] = next;
      i++;
    }
  }
  return out;
}

/** Locate a Chromium based browser installed on this machine. */
export function findBrowser() {
  const env = process.env;
  const candidates = [
    env['ProgramFiles(x86)'] && path.join(env['ProgramFiles(x86)'], 'Microsoft', 'Edge', 'Application', 'msedge.exe'),
    env.ProgramFiles && path.join(env.ProgramFiles, 'Microsoft', 'Edge', 'Application', 'msedge.exe'),
    env.ProgramFiles && path.join(env.ProgramFiles, 'Google', 'Chrome', 'Application', 'chrome.exe'),
    env['ProgramFiles(x86)'] && path.join(env['ProgramFiles(x86)'], 'Google', 'Chrome', 'Application', 'chrome.exe'),
    env.LOCALAPPDATA && path.join(env.LOCALAPPDATA, 'Google', 'Chrome', 'Application', 'chrome.exe'),
    env.ProgramFiles && path.join(env.ProgramFiles, 'BraveSoftware', 'Brave-Browser', 'Application', 'brave.exe'),
    '/usr/bin/google-chrome',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  ].filter(Boolean);
  return candidates.find((p) => {
    try {
      return fs.statSync(p).isFile();
    } catch {
      return false;
    }
  });
}

const args = parseArgs(process.argv.slice(2));
const env = process.env;

export const config = {
  /** Interface to listen on. Use 0.0.0.0 to allow connections from other machines. */
  host: args.host || env.EFB_BROWSER_HOST || '127.0.0.1',
  port: Number(args.port || env.EFB_BROWSER_PORT || 8737),
  /** Page opened at startup and by the Home button. */
  home: args.home || env.EFB_BROWSER_HOME || 'https://www.google.com',
  /** Search URL used when the address bar does not contain a URL. `%s` is replaced by the query. */
  search: args.search || env.EFB_BROWSER_SEARCH || 'https://www.google.com/search?q=%s',
  browserPath: args.browser || env.EFB_BROWSER_EXE || findBrowser(),
  /** Persistent profile so cookies / logins survive restarts. */
  profileDir: path.resolve(args.profile || env.EFB_BROWSER_PROFILE || path.join(ROOT_DIR, 'profile')),
  /** `--show` opens a visible browser window, useful for debugging. */
  headless: !(args.show || env.EFB_BROWSER_SHOW === 'true'),
  /** Internal render resolution multiplier (sharpness at zoom levels up to this value). */
  renderScale: Math.min(4, Math.max(1, Number(args['render-scale'] || env.EFB_BROWSER_RENDER_SCALE || 2))),
  verbose: Boolean(args.verbose || env.EFB_BROWSER_VERBOSE === 'true'),
};
