import { Subject } from '@microsoft/msfs-sdk';
import { BrowserSettings, loadSettings, saveSettings } from './Settings';

export interface RemoteState {
  url: string;
  title: string;
  loading: boolean;
  canGoBack: boolean;
  canGoForward: boolean;
  tabs: number;
  tabIndex: number;
}

export interface FocusInfo {
  editable: boolean;
  inputType: string;
  value: string;
}

export interface SelectInfo {
  options: { label: string; disabled: boolean }[];
  selectedIndex: number;
}

export interface DialogInfo {
  kind: 'alert' | 'confirm' | 'prompt';
  message: string;
  defaultValue: string;
}

export interface Modifiers {
  ctrl?: boolean;
  shift?: boolean;
  alt?: boolean;
  meta?: boolean;
}

const EMPTY_STATE: RemoteState = {
  url: '',
  title: '',
  loading: false,
  canGoBack: false,
  canGoForward: false,
  tabs: 1,
  tabIndex: 0,
};

const RECONNECT_DELAY_MS = 2000;

/**
 * Connection to the companion program running on the PC.
 * Owns the protocol; UI components only observe the subjects and call the command methods.
 */
export class BrowserClient {
  public readonly connected = Subject.create(false);
  public readonly state = Subject.create<RemoteState>(EMPTY_STATE);
  public readonly focus = Subject.create<FocusInfo>({ editable: false, inputType: '', value: '' });
  public readonly select = Subject.create<SelectInfo | null>(null);
  public readonly dialog = Subject.create<DialogInfo | null>(null);
  public readonly notice = Subject.create('');
  public readonly hasFrame = Subject.create(false);

  /** Called with an image URL (data: or http:) for every new frame. Must call `ack` once displayed. */
  public onFrame: ((src: string, id: number) => void) | null = null;

  private _settings: BrowserSettings = loadSettings();
  private ws: WebSocket | null = null;
  private active = false;
  private reconnectTimer: number | null = null;
  private viewWidth = 0;
  private viewHeight = 0;
  private noticeTimer: number | null = null;

  public get settings(): Readonly<BrowserSettings> {
    return this._settings;
  }

  public get address(): string {
    return `${this._settings.host}:${this._settings.port}`;
  }

  /** Start / resume streaming (app visible). */
  public resume(): void {
    this.active = true;
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.send({ type: 'resume' });
    } else {
      this.connect();
    }
  }

  /** Stop streaming (app hidden). The connection is kept open. */
  public pause(): void {
    this.active = false;
    this.send({ type: 'pause' });
  }

  public destroy(): void {
    this.active = false;
    this.clearReconnect();
    if (this.ws) {
      this.ws.onclose = null;
      this.ws.close();
      this.ws = null;
    }
  }

  public updateSettings(patch: Partial<BrowserSettings>): void {
    const prev = this._settings;
    this._settings = { ...prev, ...patch };
    saveSettings(this._settings);
    if (prev.host !== this._settings.host || prev.port !== this._settings.port) {
      this.reconnectNow();
    } else {
      this.sendConfig();
    }
  }

  /** Size of the view area in pixels. */
  public setViewSize(width: number, height: number): void {
    width = Math.round(width);
    height = Math.round(height);
    if (width < 50 || height < 50) return;
    if (Math.abs(width - this.viewWidth) < 2 && Math.abs(height - this.viewHeight) < 2) return;
    this.viewWidth = width;
    this.viewHeight = height;
    this.sendConfig();
  }

  public reconnectNow(): void {
    this.clearReconnect();
    if (this.ws) {
      this.ws.onclose = null;
      this.ws.close();
      this.ws = null;
    }
    this.connected.set(false);
    this.connect();
  }

  // ------------------------------------------------------------ commands

  public navigate(url: string): void {
    this.send({ type: 'navigate', url });
  }
  public home(): void {
    this.send({ type: 'home' });
  }
  public back(): void {
    this.send({ type: 'back' });
  }
  public forward(): void {
    this.send({ type: 'forward' });
  }
  public reloadOrStop(): void {
    this.send({ type: this.state.get().loading ? 'stop' : 'reload' });
  }
  public closeTab(): void {
    this.send({ type: 'closeTab' });
  }
  /** x / y normalized to 0..1 inside the view. */
  public mouse(action: 'down' | 'up' | 'move' | 'click', x: number, y: number, button = 'left'): void {
    this.send({ type: 'mouse', action, x, y, button });
  }
  public wheel(x: number, y: number, dx: number, dy: number): void {
    this.send({ type: 'wheel', x, y, dx, dy });
  }
  /** Scroll by a fraction of the view size (drag scrolling). */
  public scrollBy(x: number, y: number, ndx: number, ndy: number): void {
    this.send({ type: 'wheel', x, y, ndx, ndy });
  }
  public key(key: string, modifiers?: Modifiers): void {
    this.send({ type: 'key', key, modifiers });
  }
  public text(text: string): void {
    if (text) this.send({ type: 'text', text });
  }
  public chooseOption(index: number): void {
    this.select.set(null);
    this.send({ type: 'select', index });
  }
  public answerDialog(accept: boolean, text?: string): void {
    this.dialog.set(null);
    this.send({ type: 'dialog', accept, text });
  }
  public ack(id: number): void {
    this.send({ type: 'ack', id });
  }

  // ------------------------------------------------------------ connection

  private connect(): void {
    if (this.ws) return;
    this.clearReconnect();
    let ws: WebSocket;
    try {
      ws = new WebSocket(`ws://${this.address}`);
    } catch (e) {
      this.scheduleReconnect();
      return;
    }
    this.ws = ws;
    ws.onopen = (): void => {
      this.connected.set(true);
      this.sendConfig();
      if (!this.active) this.send({ type: 'pause' });
    };
    ws.onmessage = (ev: MessageEvent): void => this.onMessage(ev.data);
    ws.onerror = (): void => {
      /* onclose follows */
    };
    ws.onclose = (): void => {
      this.ws = null;
      this.connected.set(false);
      this.hasFrame.set(false);
      this.scheduleReconnect();
    };
  }

  private scheduleReconnect(): void {
    this.clearReconnect();
    this.reconnectTimer = window.setTimeout(() => {
      this.reconnectTimer = null;
      if (this.active) this.connect();
      else this.scheduleReconnect();
    }, RECONNECT_DELAY_MS);
  }

  private clearReconnect(): void {
    if (this.reconnectTimer !== null) {
      window.clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }

  private sendConfig(): void {
    if (this.viewWidth === 0) return;
    const s = this._settings;
    this.send({
      type: 'config',
      width: this.viewWidth,
      height: this.viewHeight,
      zoom: s.zoom,
      mobile: s.mobile,
      quality: s.quality,
      transport: s.transport,
    });
  }

  private send(msg: object): void {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(msg));
    }
  }

  private showNotice(text: string): void {
    this.notice.set(text);
    if (this.noticeTimer !== null) window.clearTimeout(this.noticeTimer);
    this.noticeTimer = window.setTimeout(() => this.notice.set(''), 5000);
  }

  private onMessage(raw: string): void {
    let msg: any;
    try {
      msg = JSON.parse(raw);
    } catch {
      return;
    }
    switch (msg.type) {
      case 'frame': {
        const src = msg.data
          ? 'data:image/jpeg;base64,' + msg.data
          : `http://${this.address}/frame/${msg.id}.jpg`;
        this.hasFrame.set(true);
        if (this.onFrame) this.onFrame(src, msg.id);
        else this.ack(msg.id);
        break;
      }
      case 'state':
        this.state.set({
          url: msg.url || '',
          title: msg.title || '',
          loading: !!msg.loading,
          canGoBack: !!msg.canGoBack,
          canGoForward: !!msg.canGoForward,
          tabs: msg.tabs || 1,
          tabIndex: msg.tabIndex || 0,
        });
        break;
      case 'focus':
        this.focus.set({ editable: !!msg.editable, inputType: msg.inputType || '', value: msg.value || '' });
        break;
      case 'select':
        this.select.set({ options: msg.options || [], selectedIndex: msg.selectedIndex });
        break;
      case 'dialog':
        this.dialog.set(msg.dialog || null);
        break;
      case 'notice':
        this.showNotice(msg.message || '');
        break;
    }
  }
}
