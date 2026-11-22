import { GamepadUiView, RequiredProps, TVNode, UiViewProps } from '@efb/efb-api';
import { FSComponent, NodeReference, Subscription, VNode } from '@microsoft/msfs-sdk';
import { BrowserClient, DialogInfo, RemoteState, SelectInfo } from '../BrowserClient';
import { bindSimInput } from '../SimInput';
import './BrowserPage.scss';

declare const BASE_URL: string;

interface BrowserPageProps extends RequiredProps<UiViewProps, 'appViewService'> {
  client: BrowserClient;
}

/** keyCode -> DOM key name for keys that do not produce text. */
const SPECIAL_KEYS: Record<number, string> = {
  8: 'Backspace',
  9: 'Tab',
  13: 'Enter',
  27: 'Escape',
  33: 'PageUp',
  34: 'PageDown',
  35: 'End',
  36: 'Home',
  37: 'ArrowLeft',
  38: 'ArrowUp',
  39: 'ArrowRight',
  40: 'ArrowDown',
  45: 'Insert',
  46: 'Delete',
};

const DRAG_THRESHOLD_PX = 8;
const MOVE_THROTTLE_MS = 40;
const SCROLL_FLUSH_MS = 30;
const ZOOM_STEPS = [0.5, 0.67, 0.75, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2];
const QUALITY_PRESETS: { label: string; value: number }[] = [
  { label: 'Low', value: 40 },
  { label: 'Medium', value: 70 },
  { label: 'High', value: 90 },
];

/** classList.toggle(name, force) is not reliable in Coherent GT. */
function setClass(el: Element, name: string, on: boolean): void {
  if (on) el.classList.add(name);
  else el.classList.remove(name);
}

const icon = (name: string): string => `${BASE_URL}/Assets/${name}.svg`;

export class BrowserPage extends GamepadUiView<HTMLDivElement, BrowserPageProps> {
  public readonly tabName = BrowserPage.name;

  private get client(): BrowserClient {
    return this.props.client;
  }

  // toolbar
  private readonly backBtn = FSComponent.createRef<HTMLDivElement>();
  private readonly fwdBtn = FSComponent.createRef<HTMLDivElement>();
  private readonly reloadBtn = FSComponent.createRef<HTMLDivElement>();
  private readonly reloadIcon = FSComponent.createRef<HTMLImageElement>();
  private readonly homeBtn = FSComponent.createRef<HTMLDivElement>();
  private readonly urlInput = FSComponent.createRef<HTMLInputElement>();
  private readonly goBtn = FSComponent.createRef<HTMLDivElement>();
  private readonly tabBtn = FSComponent.createRef<HTMLDivElement>();
  private readonly tabCount = FSComponent.createRef<HTMLSpanElement>();
  private readonly kbBtn = FSComponent.createRef<HTMLDivElement>();
  private readonly settingsBtn = FSComponent.createRef<HTMLDivElement>();

  // viewport
  private readonly viewport = FSComponent.createRef<HTMLDivElement>();
  private readonly img0 = FSComponent.createRef<HTMLImageElement>();
  private readonly img1 = FSComponent.createRef<HTMLImageElement>();
  private readonly loadingBar = FSComponent.createRef<HTMLDivElement>();
  private readonly notice = FSComponent.createRef<HTMLDivElement>();

  // overlays
  private readonly offline = FSComponent.createRef<HTMLDivElement>();
  private readonly offlineAddress = FSComponent.createRef<HTMLSpanElement>();
  private readonly retryBtn = FSComponent.createRef<HTMLDivElement>();
  private readonly offlineSettingsBtn = FSComponent.createRef<HTMLDivElement>();
  private readonly selectOverlay = FSComponent.createRef<HTMLDivElement>();
  private readonly selectList = FSComponent.createRef<HTMLDivElement>();
  private readonly dialogOverlay = FSComponent.createRef<HTMLDivElement>();
  private readonly dialogMessage = FSComponent.createRef<HTMLDivElement>();
  private readonly dialogInput = FSComponent.createRef<HTMLInputElement>();
  private readonly dialogOk = FSComponent.createRef<HTMLDivElement>();
  private readonly dialogCancel = FSComponent.createRef<HTMLDivElement>();

  // keyboard bridge
  private readonly kbBar = FSComponent.createRef<HTMLDivElement>();
  private readonly kbInput = FSComponent.createRef<HTMLInputElement>();
  private readonly kbDone = FSComponent.createRef<HTMLDivElement>();

  // settings
  private readonly settingsOverlay = FSComponent.createRef<HTMLDivElement>();
  private readonly hostInput = FSComponent.createRef<HTMLInputElement>();
  private readonly portInput = FSComponent.createRef<HTMLInputElement>();
  private readonly zoomOut = FSComponent.createRef<HTMLDivElement>();
  private readonly zoomIn = FSComponent.createRef<HTMLDivElement>();
  private readonly zoomLabel = FSComponent.createRef<HTMLSpanElement>();
  private readonly mobileToggle = FSComponent.createRef<HTMLDivElement>();
  private readonly dragToggle = FSComponent.createRef<HTMLDivElement>();
  private readonly qualityGroup = FSComponent.createRef<HTMLDivElement>();
  private readonly transportGroup = FSComponent.createRef<HTMLDivElement>();
  private readonly settingsClose = FSComponent.createRef<HTMLDivElement>();

  private readonly subs: Subscription[] = [];
  private readonly cleanups: (() => void)[] = [];
  private sizeTimer: number | null = null;
  private frontImg = 0;

  // pointer state
  private pressed = false;
  private dragging = false;
  private startX = 0;
  private startY = 0;
  private lastX = 0;
  private lastY = 0;
  private lastMoveAt = 0;
  private pendingScroll = { x: 0, y: 0, ndx: 0, ndy: 0 };
  private scrollTimer: number | null = null;

  // keyboard state
  private manualTyping = false;
  /** Set when the typing field lost focus (click outside...) until the user taps the page again. */
  private kbDismissed = false;
  /** True while we blur the typing field ourselves (not a user Enter / Escape). */
  private kbBlurringByCode = false;

  public render(): TVNode<HTMLDivElement> {
    return (
      <div ref={this.gamepadUiViewRef} class="browser-page">
        <div class="toolbar">
          <div ref={this.backBtn} class="tb-btn disabled"><img src={icon('back')} /></div>
          <div ref={this.fwdBtn} class="tb-btn disabled"><img src={icon('forward')} /></div>
          <div ref={this.reloadBtn} class="tb-btn"><img ref={this.reloadIcon} src={icon('reload')} /></div>
          <div ref={this.homeBtn} class="tb-btn"><img src={icon('home')} /></div>
          <div class="url-box">
            <input ref={this.urlInput} type="text" class="url-input" placeholder="Search or enter address" />
            <div ref={this.goBtn} class="go-btn hidden">Go</div>
          </div>
          <div ref={this.tabBtn} class="tb-btn tab-btn hidden">
            <img src={icon('tab-close')} />
            <span ref={this.tabCount} class="tab-count">1</span>
          </div>
          <div ref={this.kbBtn} class="tb-btn"><img src={icon('keyboard')} /></div>
          <div ref={this.settingsBtn} class="tb-btn"><img src={icon('settings')} /></div>
        </div>

        <div ref={this.viewport} class="viewport">
          <img ref={this.img0} class="frame" />
          <img ref={this.img1} class="frame" />
          <div ref={this.loadingBar} class="loading-bar hidden"><div class="loading-bar-fill" /></div>
          <div ref={this.notice} class="notice hidden" />

          <div ref={this.offline} class="overlay offline">
            <div class="panel">
              <div class="panel-title">Browser companion not connected</div>
              <div class="panel-text">
                The EFB browser displays pages rendered by the companion program running on your PC.
              </div>
              <div class="panel-text">
                Start <b>start-companion.bat</b> on the PC, then this screen will connect automatically to
                <span ref={this.offlineAddress} class="mono"> 127.0.0.1:8737</span>.
              </div>
              <div class="panel-buttons">
                <div ref={this.retryBtn} class="btn primary">Retry now</div>
                <div ref={this.offlineSettingsBtn} class="btn">Settings</div>
              </div>
            </div>
          </div>

          <div ref={this.selectOverlay} class="overlay hidden">
            <div class="panel list-panel">
              <div ref={this.selectList} class="option-list" />
            </div>
          </div>

          <div ref={this.dialogOverlay} class="overlay hidden">
            <div class="panel">
              <div ref={this.dialogMessage} class="panel-text dialog-message" />
              <input ref={this.dialogInput} type="text" class="text-field hidden" />
              <div class="panel-buttons">
                <div ref={this.dialogCancel} class="btn">Cancel</div>
                <div ref={this.dialogOk} class="btn primary">OK</div>
              </div>
            </div>
          </div>

          <div ref={this.settingsOverlay} class="overlay hidden">
            <div class="panel settings-panel">
              <div class="panel-title">Browser settings</div>
              <div class="setting-row">
                <div class="setting-label">Companion address</div>
                <div class="setting-control">
                  <input ref={this.hostInput} type="text" class="text-field host-field" />
                  <span class="sep">:</span>
                  <input ref={this.portInput} type="text" class="text-field port-field" />
                </div>
              </div>
              <div class="setting-row">
                <div class="setting-label">Zoom</div>
                <div class="setting-control">
                  <div ref={this.zoomOut} class="btn small">-</div>
                  <span ref={this.zoomLabel} class="zoom-label">100%</span>
                  <div ref={this.zoomIn} class="btn small">+</div>
                </div>
              </div>
              <div class="setting-row">
                <div class="setting-label">Mobile websites</div>
                <div class="setting-control">
                  <div ref={this.mobileToggle} class="toggle"><div class="knob" /></div>
                </div>
              </div>
              <div class="setting-row">
                <div class="setting-label">Drag to scroll</div>
                <div class="setting-control">
                  <div ref={this.dragToggle} class="toggle"><div class="knob" /></div>
                </div>
              </div>
              <div class="setting-row">
                <div class="setting-label">Image quality</div>
                <div ref={this.qualityGroup} class="setting-control segmented" />
              </div>
              <div class="setting-row">
                <div class="setting-label">Frame transport</div>
                <div ref={this.transportGroup} class="setting-control segmented" />
              </div>
              <div class="panel-buttons">
                <div ref={this.settingsClose} class="btn primary">Done</div>
              </div>
            </div>
          </div>
        </div>

        <div ref={this.kbBar} class="kb-bar hidden">
          <img class="kb-icon" src={icon('keyboard')} />
          <input ref={this.kbInput} type="text" class="kb-input" placeholder="Typing into the page..." />
          <div ref={this.kbDone} class="btn small">Done</div>
        </div>
      </div>
    );
  }

  public onAfterRender(node: VNode): void {
    super.onAfterRender(node);

    // ---- frames
    this.client.onFrame = (src, id): void => this.showFrame(src, id);

    // ---- toolbar
    this.onTap(this.backBtn, () => this.client.back());
    this.onTap(this.fwdBtn, () => this.client.forward());
    this.onTap(this.reloadBtn, () => this.client.reloadOrStop());
    this.onTap(this.homeBtn, () => this.client.home());
    this.onTap(this.tabBtn, () => this.client.closeTab());
    this.onTap(this.kbBtn, () => this.setManualTyping(!this.manualTyping));
    this.onTap(this.settingsBtn, () => this.toggleSettings(true));

    const url = this.urlInput.instance;
    let urlAtFocus = '';
    const submitUrl = (): void => {
      const value = url.value.trim();
      if (value && value !== urlAtFocus) this.client.navigate(value);
      urlAtFocus = value;
      if (document.activeElement === url) url.blur();
    };
    this.cleanups.push(
      bindSimInput(url, {
        onOsText: (text) => {
          url.value = text;
          submitUrl();
        },
        onSubmit: submitUrl,
        // The sim may swallow Enter and only unfocus the field.
        submitOnKeyboardBlur: true,
      }),
    );
    url.addEventListener('focus', () => {
      url.value = this.client.state.get().url;
      urlAtFocus = url.value;
      url.select();
      this.toggle(this.goBtn.instance, false);
    });
    url.addEventListener('blur', () => {
      this.toggle(this.goBtn.instance, true);
      this.renderUrl(this.client.state.get());
    });
    url.addEventListener('keydown', (e: KeyboardEvent) => {
      if (e.keyCode === 27) url.blur();
    });
    // Keep the focus in the field while pressing Go.
    this.goBtn.instance.addEventListener('mousedown', (e: MouseEvent) => e.preventDefault());
    this.goBtn.instance.addEventListener('click', (e: MouseEvent) => {
      e.stopPropagation();
      urlAtFocus = ''; // Go always navigates, even to the same address
      submitUrl();
    });

    // ---- viewport pointer input
    const vp = this.viewport.instance;
    vp.addEventListener('mousedown', this.onMouseDown);
    vp.addEventListener('mousemove', this.onMouseMove);
    vp.addEventListener('mouseup', this.onMouseUp);
    vp.addEventListener('mouseleave', this.onMouseUp);
    vp.addEventListener('wheel', this.onWheel);

    // ---- keyboard bridge
    const kb = this.kbInput.instance;
    this.cleanups.push(
      bindSimInput(kb, {
        onOsText: (text) => this.client.text(text),
        onSubmit: () => this.client.key('Enter'),
        submitOnKeyboardBlur: () => !this.kbBlurringByCode,
      }),
    );
    kb.addEventListener('keydown', this.onKbKeyDown);
    kb.addEventListener('keypress', this.onKbKeyPress);
    kb.addEventListener('input', this.onKbInput);
    kb.addEventListener('blur', () => {
      // Leaving the field (click outside the EFB, URL bar...) ends typing until the page is tapped again.
      this.manualTyping = false;
      this.kbDismissed = true;
      this.updateKeyboardBar();
    });
    this.onTap(this.kbDone, () => this.setManualTyping(false, true));

    // ---- overlays
    this.onTap(this.retryBtn, () => this.client.reconnectNow());
    this.onTap(this.offlineSettingsBtn, () => this.toggleSettings(true));
    this.selectOverlay.instance.addEventListener('mousedown', (e: MouseEvent) => {
      if (e.target === this.selectOverlay.instance) this.client.chooseOption(-1);
    });
    this.cleanups.push(bindSimInput(this.dialogInput.instance));
    this.onTap(this.dialogOk, () => this.client.answerDialog(true, this.dialogInput.instance.value));
    this.onTap(this.dialogCancel, () => this.client.answerDialog(false));

    this.setupSettings();

    // ---- subscriptions
    this.subs.push(
      this.client.connected.sub((c) => {
        this.toggle(this.offline.instance, c);
        this.offlineAddress.instance.textContent = ' ' + this.client.address;
        if (!c) {
          this.hideFrames();
          this.client.select.set(null);
          this.client.dialog.set(null);
        }
      }, true),
      this.client.state.sub((s) => this.renderState(s), true),
      this.client.focus.sub((f) => {
        if (f.editable) this.kbDismissed = false;
        this.updateKeyboardBar();
      }, true),
      this.client.select.sub((s) => this.renderSelect(s), true),
      this.client.dialog.sub((d) => this.renderDialog(d), true),
      this.client.notice.sub((n) => {
        this.notice.instance.textContent = n;
        this.toggle(this.notice.instance, !n);
      }, true),
    );

    // The EFB has no resize event we can rely on: poll the viewport size.
    this.sizeTimer = window.setInterval(() => this.reportSize(), 500);
    this.reportSize();
  }

  public onResume(): void {
    this.reportSize();
  }

  public destroy(): void {
    if (this.sizeTimer !== null) window.clearInterval(this.sizeTimer);
    if (this.scrollTimer !== null) window.clearTimeout(this.scrollTimer);
    this.client.onFrame = null;
    this.subs.forEach((s) => s.destroy());
    this.cleanups.forEach((c) => c());
    super.destroy();
  }

  // ------------------------------------------------------------ helpers

  private onTap(ref: NodeReference<HTMLElement>, fn: () => void): void {
    ref.instance.addEventListener('click', (e: MouseEvent) => {
      e.stopPropagation();
      if (ref.instance.classList.contains('disabled')) return;
      fn();
    });
  }

  private toggle(el: HTMLElement, hidden: boolean): void {
    if (hidden) el.classList.add('hidden');
    else el.classList.remove('hidden');
  }

  private reportSize(): void {
    const r = this.viewport.instance.getBoundingClientRect();
    this.client.setViewSize(r.width, r.height);
  }

  // ------------------------------------------------------------ frames

  /**
   * Double buffering without ever hiding an image: the new frame loads in the image
   * underneath, then is stacked on top. In Coherent GT `onload` can fire before the
   * image is actually drawn; since the previous frame stays visible below, a late
   * draw can never show as a blank flash.
   */
  private showFrame(src: string, id: number): void {
    const imgs = [this.img0.instance, this.img1.instance];
    const back = imgs[1 - this.frontImg];
    const front = imgs[this.frontImg];
    back.onload = (): void => {
      back.onload = null;
      // Give the engine one frame to upload the texture before raising it.
      requestAnimationFrame(() => {
        back.classList.add('visible');
        back.style.zIndex = '2';
        front.style.zIndex = '1';
        this.frontImg = 1 - this.frontImg;
        this.client.ack(id);
      });
    };
    back.onerror = (): void => {
      back.onerror = null;
      this.client.ack(id);
    };
    back.src = src;
  }

  private hideFrames(): void {
    this.img0.instance.classList.remove('visible');
    this.img1.instance.classList.remove('visible');
  }

  // ------------------------------------------------------------ state rendering

  private renderState(s: RemoteState): void {
    setClass(this.backBtn.instance, 'disabled', !s.canGoBack);
    setClass(this.fwdBtn.instance, 'disabled', !s.canGoForward);
    this.reloadIcon.instance.src = icon(s.loading ? 'stop' : 'reload');
    this.toggle(this.loadingBar.instance, !s.loading);
    this.toggle(this.tabBtn.instance, s.tabs <= 1);
    this.tabCount.instance.textContent = String(s.tabs);
    this.renderUrl(s);
  }

  private renderUrl(s: RemoteState): void {
    if (document.activeElement === this.urlInput.instance) return;
    this.urlInput.instance.value = s.url.startsWith('data:') ? s.title : s.url;
  }

  private renderSelect(s: SelectInfo | null): void {
    const list = this.selectList.instance;
    list.innerHTML = '';
    this.toggle(this.selectOverlay.instance, !s);
    if (!s) return;
    s.options.forEach((opt, i) => {
      const row = document.createElement('div');
      row.className = 'option' + (i === s.selectedIndex ? ' selected' : '') + (opt.disabled ? ' disabled' : '');
      row.textContent = opt.label;
      if (!opt.disabled) {
        row.addEventListener('click', (e: MouseEvent) => {
          e.stopPropagation();
          this.client.chooseOption(i);
        });
      }
      list.appendChild(row);
    });
  }

  private renderDialog(d: DialogInfo | null): void {
    this.toggle(this.dialogOverlay.instance, !d);
    if (!d) return;
    this.dialogMessage.instance.textContent = d.message;
    this.toggle(this.dialogInput.instance, d.kind !== 'prompt');
    this.dialogInput.instance.value = d.defaultValue || '';
    this.toggle(this.dialogCancel.instance, d.kind === 'alert');
  }

  // ------------------------------------------------------------ pointer input

  private normalized(e: MouseEvent): { x: number; y: number } {
    const r = this.viewport.instance.getBoundingClientRect();
    return {
      x: Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)),
      y: Math.min(1, Math.max(0, (e.clientY - r.top) / r.height)),
    };
  }

  private isOverlayEvent(e: MouseEvent): boolean {
    const t = e.target as HTMLElement | null;
    return !!t && t !== this.viewport.instance && !t.classList.contains('frame');
  }

  private readonly onMouseDown = (e: MouseEvent): void => {
    if (this.isOverlayEvent(e) || e.button !== 0 || !this.client.connected.get()) return;
    // Keep keyboard focus on the typing field while interacting with the page.
    e.preventDefault();
    if (document.activeElement === this.urlInput.instance) this.urlInput.instance.blur();
    this.pressed = true;
    this.dragging = false;
    if (this.kbDismissed) {
      this.kbDismissed = false;
      this.updateKeyboardBar();
    }
    this.startX = this.lastX = e.clientX;
    this.startY = this.lastY = e.clientY;
    if (!this.client.settings.dragScroll) {
      const p = this.normalized(e);
      this.client.mouse('down', p.x, p.y);
    }
  };

  private readonly onMouseMove = (e: MouseEvent): void => {
    if (!this.client.connected.get()) return;
    const p = this.normalized(e);
    if (this.pressed && this.client.settings.dragScroll) {
      if (!this.dragging && Math.hypot(e.clientX - this.startX, e.clientY - this.startY) > DRAG_THRESHOLD_PX) {
        this.dragging = true;
      }
      if (this.dragging) {
        const r = this.viewport.instance.getBoundingClientRect();
        this.queueScroll(p.x, p.y, (this.lastX - e.clientX) / r.width, (this.lastY - e.clientY) / r.height);
        this.lastX = e.clientX;
        this.lastY = e.clientY;
      }
      return;
    }
    if (this.isOverlayEvent(e)) return;
    const now = Date.now();
    if (!this.pressed && now - this.lastMoveAt < MOVE_THROTTLE_MS) return;
    this.lastMoveAt = now;
    this.client.mouse('move', p.x, p.y);
  };

  private readonly onMouseUp = (e: MouseEvent): void => {
    if (!this.pressed) return;
    this.pressed = false;
    if (this.client.settings.dragScroll) {
      if (!this.dragging) {
        const r = this.viewport.instance.getBoundingClientRect();
        this.client.mouse('click', (this.startX - r.left) / r.width, (this.startY - r.top) / r.height);
      } else {
        this.flushScroll();
      }
    } else {
      const p = this.normalized(e);
      this.client.mouse('up', p.x, p.y);
    }
    this.dragging = false;
  };

  private readonly onWheel = (e: WheelEvent): void => {
    if (!this.client.connected.get() || this.isOverlayEvent(e)) return;
    e.preventDefault();
    const scale = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? 400 : 1;
    const p = this.normalized(e);
    this.client.wheel(p.x, p.y, e.deltaX * scale, e.deltaY * scale);
  };

  /** Coalesce drag-scroll deltas so we do not flood the companion. */
  private queueScroll(x: number, y: number, ndx: number, ndy: number): void {
    const s = this.pendingScroll;
    s.x = x;
    s.y = y;
    s.ndx += ndx;
    s.ndy += ndy;
    if (this.scrollTimer === null) {
      this.scrollTimer = window.setTimeout(() => this.flushScroll(), SCROLL_FLUSH_MS);
    }
  }

  private flushScroll(): void {
    if (this.scrollTimer !== null) {
      window.clearTimeout(this.scrollTimer);
      this.scrollTimer = null;
    }
    const s = this.pendingScroll;
    if (s.ndx !== 0 || s.ndy !== 0) this.client.scrollBy(s.x, s.y, s.ndx, s.ndy);
    s.ndx = 0;
    s.ndy = 0;
  }

  // ------------------------------------------------------------ keyboard bridge

  private setManualTyping(on: boolean, blurRemote = false): void {
    this.manualTyping = on;
    if (on) this.kbDismissed = false;
    if (!on && blurRemote && this.client.focus.get().editable) {
      // "Done": leave the remote field too, like closing a phone keyboard.
      this.client.focus.set({ editable: false, inputType: '', value: '' });
    }
    this.updateKeyboardBar();
  }

  private updateKeyboardBar(): void {
    const typing = this.manualTyping || (this.client.focus.get().editable && !this.kbDismissed);
    const kb = this.kbInput.instance;
    const active = document.activeElement;
    const otherFieldFocused = !!active && active !== kb && (active.tagName === 'INPUT' || active.tagName === 'TEXTAREA');
    this.toggle(this.kbBar.instance, !typing);
    setClass(this.kbBtn.instance, 'active', typing);
    if (typing) {
      if (active !== kb && !otherFieldFocused) kb.focus();
    } else if (active === kb) {
      this.kbBlurringByCode = true;
      kb.blur();
      this.kbBlurringByCode = false;
    }
  }

  private readonly onKbKeyDown = (e: KeyboardEvent): void => {
    if (e.keyCode === 13) return; // Enter is handled by bindSimInput (onSubmit)
    const name = SPECIAL_KEYS[e.keyCode];
    const modifiers = { ctrl: e.ctrlKey, shift: e.shiftKey, alt: e.altKey };
    if (name) {
      e.preventDefault();
      this.client.key(name, modifiers);
      return;
    }
    if (e.ctrlKey && e.keyCode >= 65 && e.keyCode <= 90) {
      // Ctrl+A / Ctrl+Z ... editing shortcuts.
      e.preventDefault();
      this.client.key(String.fromCharCode(e.keyCode).toLowerCase(), modifiers);
    }
  };

  private readonly onKbKeyPress = (e: KeyboardEvent): void => {
    const code = e.charCode || e.which;
    if (code >= 32 && !e.ctrlKey) {
      e.preventDefault();
      this.client.text(String.fromCharCode(code));
    }
  };

  /** Fallback for text that bypasses keypress (IME, paste...). */
  private readonly onKbInput = (): void => {
    const kb = this.kbInput.instance;
    if (kb.value) {
      this.client.text(kb.value);
      kb.value = '';
    }
  };

  // ------------------------------------------------------------ settings

  private toggleSettings(show: boolean): void {
    this.toggle(this.settingsOverlay.instance, !show);
    if (show) this.refreshSettingsUi();
  }

  private setupSettings(): void {
    const c = this.client;
    this.cleanups.push(bindSimInput(this.hostInput.instance), bindSimInput(this.portInput.instance));

    const commitAddress = (): void => {
      const host = this.hostInput.instance.value.trim() || '127.0.0.1';
      const port = parseInt(this.portInput.instance.value, 10);
      c.updateSettings({ host, port: port > 0 && port < 65536 ? port : c.settings.port });
      this.refreshSettingsUi();
    };
    this.hostInput.instance.addEventListener('change', commitAddress);
    this.portInput.instance.addEventListener('change', commitAddress);
    for (const input of [this.hostInput.instance, this.portInput.instance]) {
      input.addEventListener('keydown', (e: KeyboardEvent) => {
        if (e.keyCode === 13) input.blur();
      });
    }

    const stepZoom = (dir: number): void => {
      const cur = c.settings.zoom;
      let idx = ZOOM_STEPS.findIndex((z) => z >= cur - 0.001);
      if (idx < 0) idx = ZOOM_STEPS.length - 1;
      idx = Math.min(ZOOM_STEPS.length - 1, Math.max(0, idx + dir));
      c.updateSettings({ zoom: ZOOM_STEPS[idx] });
      this.refreshSettingsUi();
    };
    this.onTap(this.zoomOut, () => stepZoom(-1));
    this.onTap(this.zoomIn, () => stepZoom(1));
    this.onTap(this.mobileToggle, () => {
      c.updateSettings({ mobile: !c.settings.mobile });
      this.refreshSettingsUi();
    });
    this.onTap(this.dragToggle, () => {
      c.updateSettings({ dragScroll: !c.settings.dragScroll });
      this.refreshSettingsUi();
    });

    for (const preset of QUALITY_PRESETS) {
      const seg = document.createElement('div');
      seg.className = 'segment';
      seg.textContent = preset.label;
      seg.setAttribute('data-value', String(preset.value));
      seg.addEventListener('click', () => {
        c.updateSettings({ quality: preset.value });
        this.refreshSettingsUi();
      });
      this.qualityGroup.instance.appendChild(seg);
    }
    for (const t of ['data', 'http'] as const) {
      const seg = document.createElement('div');
      seg.className = 'segment';
      seg.textContent = t === 'data' ? 'WebSocket' : 'HTTP';
      seg.setAttribute('data-value', t);
      seg.addEventListener('click', () => {
        c.updateSettings({ transport: t });
        this.refreshSettingsUi();
      });
      this.transportGroup.instance.appendChild(seg);
    }
    this.onTap(this.settingsClose, () => this.toggleSettings(false));
  }

  private refreshSettingsUi(): void {
    const s = this.client.settings;
    if (document.activeElement !== this.hostInput.instance) this.hostInput.instance.value = s.host;
    if (document.activeElement !== this.portInput.instance) this.portInput.instance.value = String(s.port);
    this.zoomLabel.instance.textContent = `${Math.round(s.zoom * 100)}%`;
    setClass(this.mobileToggle.instance, 'on', s.mobile);
    setClass(this.dragToggle.instance, 'on', s.dragScroll);
    const markSegments = (group: HTMLElement, value: string): void => {
      const segs = group.querySelectorAll('.segment');
      for (let i = 0; i < segs.length; i++) {
        setClass(segs[i], 'selected', segs[i].getAttribute('data-value') === value);
      }
    };
    const q = QUALITY_PRESETS.reduce((best, p) => (Math.abs(p.value - s.quality) < Math.abs(best.value - s.quality) ? p : best));
    markSegments(this.qualityGroup.instance, String(q.value));
    markSegments(this.transportGroup.instance, s.transport);
    this.offlineAddress.instance.textContent = ' ' + this.client.address;
  }
}
