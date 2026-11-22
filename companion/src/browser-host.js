import { EventEmitter } from 'node:events';
import puppeteer from 'puppeteer-core';
import { PAGE_SCRIPT } from './page-script.js';

const clamp01 = (v) => Math.min(1, Math.max(0, Number(v) || 0));
const BUTTON_MASK = { none: 0, left: 1, right: 2, middle: 4 };
const MODIFIER_KEYS = [
  ['alt', 'Alt', 1],
  ['ctrl', 'Control', 2],
  ['meta', 'Meta', 4],
  ['shift', 'Shift', 8],
];

/**
 * Owns the headless browser: tabs, viewport, screencast and input injection.
 *
 * Events:
 *  - 'frame'  { id, data (base64 jpeg), width, height }
 *  - 'state'  { url, title, loading, canGoBack, canGoForward, tabs, tabIndex, mobile, zoom }
 *  - 'focus'  { editable, inputType, value }
 *  - 'select' { options, selectedIndex }
 *  - 'dialog' { kind, message, defaultValue } | null (dialog closed)
 *  - 'notice' { level, message }
 */
export class BrowserHost extends EventEmitter {
  constructor(config) {
    super();
    this.config = config;
    this.browser = null;
    this.tabs = [];
    this.active = null;
    /** Size of the EFB view in device pixels, zoom = device pixels per CSS pixel. */
    this.view = { width: 900, height: 1200, zoom: 1, mobile: false };
    this.quality = 70;
    this.streaming = false;
    this.lastFrame = null;
    this.frameSeq = 0;
    this.lastStateJson = '';
    this.pendingDialog = null;
    this.mouseButton = 'none';
    this.queue = Promise.resolve();
    this.stateTimer = null;
    this.closing = false;
  }

  log(...args) {
    if (this.config.verbose) console.log('[browser]', ...args);
  }

  /** Run input/commands sequentially so that mouse/keyboard ordering is preserved. */
  enqueue(fn) {
    this.queue = this.queue.then(fn).catch((err) => {
      this.log('command failed:', err && err.message);
    });
    return this.queue;
  }

  async start() {
    if (!this.config.browserPath) {
      throw new Error('No Chromium based browser found. Install Microsoft Edge or Google Chrome, or pass --browser "<path to exe>".');
    }
    this.browser = await puppeteer.launch({
      executablePath: this.config.browserPath,
      headless: this.config.headless,
      userDataDir: this.config.profileDir,
      defaultViewport: null,
      ignoreDefaultArgs: ['--mute-audio', '--enable-automation'],
      args: [
        '--disable-blink-features=AutomationControlled',
        '--autoplay-policy=no-user-gesture-required',
        '--no-first-run',
        '--no-default-browser-check',
        '--hide-crash-restore-bubble',
        '--disable-features=Translate,MediaRouter',
        // Headless screencasts ignore the emulated deviceScaleFactor: render at a fixed
        // high DPI instead and let the screencast downscale frames to the EFB size.
        `--force-device-scale-factor=${this.config.renderScale}`,
      ],
    });
    this.baseUserAgent = (await this.browser.userAgent()).replace('HeadlessChrome', 'Chrome');
    this.browser.on('disconnected', () => {
      if (!this.closing) this.emit('crash');
    });

    const pages = await this.browser.pages();
    const page = pages[0] || (await this.browser.newPage());
    const tab = await this.addTab(page);
    await this.activate(tab);
    this.navigate(this.config.home);

    this.stateTimer = setInterval(() => this.refreshState(), 1000);
  }

  async close() {
    this.closing = true;
    clearInterval(this.stateTimer);
    if (this.browser) await this.browser.close().catch(() => {});
  }

  userAgent() {
    if (!this.view.mobile) return this.baseUserAgent;
    const ver = (this.baseUserAgent.match(/Chrome\/([\d.]+)/) || [])[1] || '130.0.0.0';
    return `Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/${ver} Mobile Safari/537.36`;
  }

  cssSize() {
    const { width, height, zoom } = this.view;
    return {
      w: Math.max(200, Math.round(width / zoom)),
      h: Math.max(150, Math.round(height / zoom)),
    };
  }

  // ---------------------------------------------------------------- tabs

  async addTab(page) {
    const tab = { page, cdp: null, loading: false, screencasting: false, viewKey: '' };
    this.tabs.push(tab);
    tab.cdp = await page.createCDPSession();

    await page.exposeFunction('__efbFocus', (info) => {
      if (tab === this.active) this.onFocusReport(info);
    });
    await page.exposeFunction('__efbSelect', (info) => {
      if (tab === this.active) this.emit('select', info);
    });
    await page.evaluateOnNewDocument(PAGE_SCRIPT);
    page.evaluate(PAGE_SCRIPT).catch(() => {});

    tab.cdp.on('Page.screencastFrame', (ev) => this.onScreencastFrame(tab, ev));

    page.on('request', (req) => {
      if (req.isNavigationRequest() && req.frame() === page.mainFrame()) {
        tab.loading = true;
        this.scheduleState();
      }
    });
    page.on('requestfailed', (req) => {
      if (req.isNavigationRequest() && req.frame() === page.mainFrame()) {
        tab.loading = false;
        this.scheduleState();
      }
    });
    page.on('load', () => {
      tab.loading = false;
      this.scheduleState();
    });
    page.on('framenavigated', (frame) => {
      if (frame === page.mainFrame()) this.scheduleState();
    });
    page.on('popup', (popup) => {
      if (!popup) return;
      this.enqueue(async () => {
        const t = await this.addTab(popup);
        await this.activate(t);
      });
    });
    page.on('close', () => this.enqueue(() => this.removeTab(tab)));
    page.on('dialog', (dialog) => {
      if (dialog.type() === 'beforeunload') {
        dialog.accept().catch(() => {});
        return;
      }
      if (tab !== this.active) {
        dialog.dismiss().catch(() => {});
        return;
      }
      this.pendingDialog = dialog;
      this.emit('dialog', { kind: dialog.type(), message: dialog.message(), defaultValue: dialog.defaultValue() });
    });
    return tab;
  }

  async removeTab(tab) {
    const idx = this.tabs.indexOf(tab);
    if (idx < 0) return;
    this.tabs.splice(idx, 1);
    if (this.active === tab) {
      this.active = null;
      if (this.tabs.length > 0) {
        await this.activate(this.tabs[Math.max(0, idx - 1)]);
      } else {
        // Never leave the browser without a tab.
        const t = await this.addTab(await this.browser.newPage());
        await this.activate(t);
        this.navigate(this.config.home);
      }
    }
    this.scheduleState();
  }

  async activate(tab) {
    if (this.active && this.active !== tab) await this.stopScreencast(this.active);
    this.active = tab;
    this.onFocusReport({ editable: false, inputType: '', value: '' });
    await this.applyViewport(tab);
    if (this.streaming) await this.startScreencast(tab);
    this.scheduleState();
  }

  async closeTab() {
    if (this.tabs.length <= 1 || !this.active) return;
    await this.active.page.close({ runBeforeUnload: false }).catch(() => {});
  }

  // ---------------------------------------------------------------- viewport & streaming

  /** Apply EFB view settings. Restarts the screencast when the geometry changed. */
  async configure({ width, height, zoom, mobile, quality }) {
    const prevMobile = this.view.mobile;
    if (width > 0) this.view.width = Math.round(Math.min(width, 4096));
    if (height > 0) this.view.height = Math.round(Math.min(height, 4096));
    if (zoom > 0) this.view.zoom = Math.min(4, Math.max(0.25, zoom));
    if (typeof mobile === 'boolean') this.view.mobile = mobile;
    if (quality > 0) this.quality = Math.min(100, Math.max(10, Math.round(quality)));
    if (!this.active) return;
    const changed = await this.applyViewport(this.active);
    if (changed && this.streaming) {
      await this.stopScreencast(this.active);
      await this.startScreencast(this.active);
    }
    if (prevMobile !== this.view.mobile) {
      // A new user agent only applies on the next load.
      this.active.page.reload().catch(() => {});
    }
    this.scheduleState();
  }

  async applyViewport(tab) {
    const { w, h } = this.cssSize();
    const key = `${w}x${h}@${this.view.zoom}/${this.view.mobile}/${this.quality}`;
    if (tab.viewKey === key) return false;
    tab.viewKey = key;
    await tab.page.setUserAgent(this.userAgent());
    await tab.page.setViewport({
      width: w,
      height: h,
      deviceScaleFactor: this.view.zoom,
      isMobile: this.view.mobile,
      hasTouch: false,
    });
    return true;
  }

  async setStreaming(on) {
    if (this.streaming === on) return;
    this.streaming = on;
    if (!this.active) return;
    if (on) await this.startScreencast(this.active);
    else await this.stopScreencast(this.active);
  }

  async startScreencast(tab) {
    if (tab.screencasting) return;
    tab.screencasting = true;
    await tab.cdp.send('Page.startScreencast', {
      format: 'jpeg',
      quality: this.quality,
      maxWidth: this.view.width,
      maxHeight: this.view.height,
      everyNthFrame: 1,
    });
  }

  async stopScreencast(tab) {
    if (!tab.screencasting) return;
    tab.screencasting = false;
    await tab.cdp.send('Page.stopScreencast').catch(() => {});
  }

  onScreencastFrame(tab, { data, metadata, sessionId }) {
    tab.cdp.send('Page.screencastFrameAck', { sessionId }).catch(() => {});
    if (tab !== this.active) return;
    this.lastFrame = {
      id: ++this.frameSeq,
      data,
      buffer: null,
      width: this.view.width,
      height: this.view.height,
    };
    this.emit('frame', this.lastFrame);
  }

  frameBuffer() {
    const f = this.lastFrame;
    if (!f) return null;
    if (!f.buffer) f.buffer = Buffer.from(f.data, 'base64');
    return f.buffer;
  }

  // ---------------------------------------------------------------- state

  scheduleState() {
    clearTimeout(this.stateDebounce);
    this.stateDebounce = setTimeout(() => this.refreshState(), 50);
  }

  async refreshState() {
    const tab = this.active;
    if (!tab) return;
    let title = '';
    let hist = null;
    try {
      title = await tab.page.title();
    } catch {}
    try {
      hist = await tab.cdp.send('Page.getNavigationHistory');
    } catch {}
    const state = {
      url: tab.page.url(),
      title,
      loading: tab.loading,
      canGoBack: hist ? hist.entries.slice(0, hist.currentIndex).some((e) => e.url !== 'about:blank') : false,
      canGoForward: hist ? hist.currentIndex < hist.entries.length - 1 : false,
      tabs: this.tabs.length,
      tabIndex: this.tabs.indexOf(tab),
      mobile: this.view.mobile,
      zoom: this.view.zoom,
    };
    const json = JSON.stringify(state);
    if (json !== this.lastStateJson) {
      this.lastStateJson = json;
      this.emit('state', state);
    }
  }

  currentState() {
    return this.lastStateJson ? JSON.parse(this.lastStateJson) : null;
  }

  onFocusReport(info) {
    // focusout is followed by focusin when moving between fields: debounce the "blur" case.
    clearTimeout(this.focusTimer);
    const next = { editable: !!info.editable, inputType: info.inputType || '', value: info.value || '' };
    const apply = () => {
      const json = JSON.stringify(next);
      if (json === this.lastFocusJson) return;
      this.lastFocusJson = json;
      this.emit('focus', next);
    };
    if (next.editable) apply();
    else this.focusTimer = setTimeout(apply, 120);
  }

  // ---------------------------------------------------------------- navigation

  normalizeUrl(input) {
    const s = String(input || '').trim();
    if (!s) return null;
    if (/^(https?|file|about|data|chrome|edge|view-source):/i.test(s)) return s;
    if (/^(localhost|\d{1,3}(\.\d{1,3}){3}|\[[0-9a-f:]+\])(:\d+)?(\/.*)?$/i.test(s)) return 'http://' + s;
    if (!/\s/.test(s) && /^[^/?#]+\.[a-z][a-z0-9-]+(:\d+)?([/?#].*)?$/i.test(s)) return 'https://' + s;
    return this.config.search.replace('%s', encodeURIComponent(s));
  }

  navigate(input) {
    const url = this.normalizeUrl(input);
    if (!url || !this.active) return;
    const tab = this.active;
    tab.loading = true;
    this.scheduleState();
    tab.page.goto(url, { waitUntil: 'domcontentloaded', timeout: 60000 }).catch((err) => {
      const msg = err && err.message ? err.message.split('\n')[0] : String(err);
      if (!/net::ERR_ABORTED|Navigating frame was detached|frame got detached/i.test(msg)) {
        this.emit('notice', { level: 'error', message: msg });
      }
    });
  }

  async historyGo(delta) {
    const tab = this.active;
    if (!tab) return;
    const hist = await tab.cdp.send('Page.getNavigationHistory');
    const entry = hist.entries[hist.currentIndex + delta];
    if (entry) await tab.cdp.send('Page.navigateToHistoryEntry', { entryId: entry.id });
  }

  async reload() {
    if (this.active) await this.active.cdp.send('Page.reload', { ignoreCache: false });
  }

  async stop() {
    if (!this.active) return;
    await this.active.cdp.send('Page.stopLoading');
    this.active.loading = false;
    this.scheduleState();
  }

  // ---------------------------------------------------------------- input

  modifierMask(mods = {}) {
    return MODIFIER_KEYS.reduce((m, [name, , bit]) => (mods[name] ? m | bit : m), 0);
  }

  toCss(x, y) {
    const { w, h } = this.cssSize();
    return { x: clamp01(x) * w, y: clamp01(y) * h };
  }

  /** x / y are normalized (0..1) coordinates inside the view. */
  async mouse({ action, x, y, button = 'left', clickCount = 1, modifiers }) {
    const tab = this.active;
    if (!tab) return;
    const p = this.toCss(x, y);
    const mods = this.modifierMask(modifiers);
    const send = (type, btn, buttons, count) =>
      tab.cdp.send('Input.dispatchMouseEvent', {
        type,
        x: p.x,
        y: p.y,
        button: btn,
        buttons,
        clickCount: count,
        modifiers: mods,
      });
    if (!(button in BUTTON_MASK)) button = 'left';
    switch (action) {
      case 'move':
        await send('mouseMoved', this.mouseButton, BUTTON_MASK[this.mouseButton], 0);
        break;
      case 'down':
        this.mouseButton = button;
        await send('mouseMoved', 'none', 0, 0);
        await send('mousePressed', button, BUTTON_MASK[button], clickCount);
        break;
      case 'up':
        this.mouseButton = 'none';
        await send('mouseReleased', button, 0, clickCount);
        break;
      case 'click':
        await send('mouseMoved', 'none', 0, 0);
        await send('mousePressed', button, BUTTON_MASK[button], clickCount);
        await send('mouseReleased', button, 0, clickCount);
        break;
    }
  }

  /** Wheel: dx/dy in CSS pixels. Scroll (drag): ndx/ndy as a fraction of the view size. */
  async wheel({ x, y, dx = 0, dy = 0, ndx, ndy }) {
    const tab = this.active;
    if (!tab) return;
    const p = this.toCss(x, y);
    const { w, h } = this.cssSize();
    const deltaX = ndx !== undefined ? Number(ndx) * w : Number(dx);
    const deltaY = ndy !== undefined ? Number(ndy) * h : Number(dy);
    if (!deltaX && !deltaY) return;
    await tab.cdp.send('Input.dispatchMouseEvent', {
      type: 'mouseWheel',
      x: p.x,
      y: p.y,
      deltaX,
      deltaY,
      modifiers: 0,
    });
  }

  async key({ key, modifiers = {} }) {
    const kb = this.active && this.active.page.keyboard;
    if (!kb || !key) return;
    const held = MODIFIER_KEYS.filter(([name]) => modifiers[name]).map(([, k]) => k);
    for (const k of held) await kb.down(k);
    try {
      await kb.press(key);
    } catch (err) {
      // Unknown key name: fall back to sending it as text if printable.
      if (key.length === 1) await kb.sendCharacter(key);
      else this.log('unknown key', key);
    }
    for (const k of held.reverse()) await kb.up(k);
  }

  async text({ text }) {
    const tab = this.active;
    if (!tab || !text) return;
    if (text.length <= 64) await tab.page.keyboard.type(String(text));
    else await tab.cdp.send('Input.insertText', { text: String(text) });
  }

  async selectOption({ index }) {
    const tab = this.active;
    if (!tab) return;
    const i = Number.isInteger(index) ? index : -1;
    for (const frame of tab.page.frames()) {
      const done = await frame
        .evaluate((n) => (window.__efbApplySelect ? window.__efbApplySelect(n) : false), i)
        .catch(() => false);
      if (done) break;
    }
  }

  async dialogResponse({ accept, text }) {
    const d = this.pendingDialog;
    this.pendingDialog = null;
    if (!d) return;
    this.emit('dialog', null);
    if (accept) await d.accept(text !== undefined ? String(text) : undefined).catch(() => {});
    else await d.dismiss().catch(() => {});
  }
}
