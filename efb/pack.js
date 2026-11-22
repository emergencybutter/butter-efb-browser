#!/usr/bin/env node
/**
 * Assembles the Community package from the built app (PackageSources/BrowserApp/dist)
 * without needing the sim / fspackagetool:
 *
 *   node pack.js            -> efb/Packages/buttersim-efb-browser
 *   node pack.js --install  -> also copies it into the MSFS 2024 Community folder
 *
 * The package can also be built with the SDK project EFBBrowserProject.xml (DevMode).
 */
const fs = require('fs');
const path = require('path');

const PACKAGE_NAME = 'buttersim-efb-browser';
const APP_FOLDER = 'BrowserApp';
const VERSION = '0.1.0';

const root = __dirname;
const dist = path.join(root, 'PackageSources', APP_FOLDER, 'dist');
const out = path.join(root, 'Packages', PACKAGE_NAME);

function copyDir(src, dst, filter) {
  fs.mkdirSync(dst, { recursive: true });
  for (const entry of fs.readdirSync(src, { withFileTypes: true })) {
    const s = path.join(src, entry.name);
    const d = path.join(dst, entry.name);
    if (entry.isDirectory()) copyDir(s, d, filter);
    else if (!filter || filter(s)) fs.copyFileSync(s, d);
  }
}

function listFiles(dir) {
  const files = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, entry.name);
    if (entry.isDirectory()) files.push(...listFiles(p));
    else files.push(p);
  }
  return files;
}

/** Windows FILETIME: 100ns intervals since 1601-01-01. */
function fileTime(date) {
  return (BigInt(date.getTime()) + 11644473600000n) * 10000n;
}

function findCommunityFolder() {
  const env = process.env;
  const candidates = [
    env.LOCALAPPDATA && path.join(env.LOCALAPPDATA, 'Packages', 'Microsoft.Limitless_8wekyb3d8bbwe', 'LocalCache', 'UserCfg.opt'),
    env.APPDATA && path.join(env.APPDATA, 'Microsoft Flight Simulator 2024', 'UserCfg.opt'),
  ].filter(Boolean);
  for (const cfg of candidates) {
    if (!fs.existsSync(cfg)) continue;
    const m = fs.readFileSync(cfg, 'utf8').match(/InstalledPackagesPath\s+"([^"]+)"/);
    if (m) return path.join(m[1], 'Community');
  }
  return null;
}

if (!fs.existsSync(path.join(dist, `${APP_FOLDER}.js`))) {
  console.error(`Build the app first: cd PackageSources/${APP_FOLDER} && npm run build`);
  process.exit(1);
}

fs.rmSync(out, { recursive: true, force: true });
copyDir(dist, path.join(out, 'html_ui', 'efb_ui', 'efb_apps', APP_FOLDER), (f) => !f.endsWith('.map'));
copyDir(
  path.join(root, 'PackageDefinitions', PACKAGE_NAME, 'ContentInfo'),
  path.join(out, 'ContentInfo', PACKAGE_NAME),
);

const manifest = {
  dependencies: [],
  content_type: 'MISC',
  title: 'EFB Web Browser',
  manufacturer: '',
  creator: 'ButterSim',
  package_version: VERSION,
  minimum_game_version: '1.3.23',
  minimum_compatibility_version: '3.19.0.47',
  builder: 'Microsoft Flight Simulator 2024',
  package_order_hint: '',
  release_notes: { neutral: { LastUpdate: '', OlderHistory: '' } },
};
fs.writeFileSync(path.join(out, 'manifest.json'), JSON.stringify(manifest, null, 2));

const content = listFiles(out)
  .filter((f) => !/[\\/](layout|manifest)\.json$/.test(f))
  .map((f) => {
    const st = fs.statSync(f);
    return {
      path: path.relative(out, f).split(path.sep).join('/').toLowerCase(),
      size: st.size,
      date: fileTime(st.mtime),
    };
  });
// FILETIME exceeds Number precision: write it as a raw integer.
const layout = JSON.stringify({ content }, (k, v) => (typeof v === 'bigint' ? `__BIG${v}__` : v), 2).replace(
  /"__BIG(\d+)__"/g,
  '$1',
);
fs.writeFileSync(path.join(out, 'layout.json'), layout);
console.log(`Package written to ${out} (${content.length} files)`);

if (process.argv.includes('--install')) {
  const community = findCommunityFolder();
  if (!community) {
    console.error('Could not find the MSFS 2024 Community folder (UserCfg.opt). Copy the package manually.');
    process.exit(1);
  }
  const target = path.join(community, PACKAGE_NAME);
  fs.rmSync(target, { recursive: true, force: true });
  copyDir(out, target);
  console.log(`Installed to ${target}`);
}
