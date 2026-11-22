/// <reference types="@microsoft/msfs-types/js/datastorage" />

export type FrameTransport = 'data' | 'http';

export interface BrowserSettings {
  /** Address of the PC running the companion program. */
  host: string;
  port: number;
  /** Page zoom: 1 = 100%. Higher values make text bigger (narrower layout). */
  zoom: number;
  /** Request mobile versions of websites. */
  mobile: boolean;
  /** JPEG quality 10..100. */
  quality: number;
  /** How frames reach the EFB: inline base64 over the WebSocket, or fetched over HTTP. */
  transport: FrameTransport;
  /** Dragging scrolls the page (tablet style) instead of sending a mouse drag. */
  dragScroll: boolean;
}

export const DEFAULT_SETTINGS: BrowserSettings = {
  host: '127.0.0.1',
  port: 8737,
  zoom: 1,
  mobile: false,
  quality: 70,
  transport: 'data',
  dragScroll: true,
};

const STORAGE_KEY = 'BUTTERSIM_EFB_BROWSER_SETTINGS';

function readRaw(): string | null {
  try {
    if (typeof GetStoredData === 'function') {
      const v = GetStoredData(STORAGE_KEY);
      if (typeof v === 'string' && v.length > 0) return v;
    }
  } catch {
    /* not available */
  }
  try {
    return window.localStorage ? window.localStorage.getItem(STORAGE_KEY) : null;
  } catch {
    return null;
  }
}

function writeRaw(value: string): void {
  try {
    if (typeof SetStoredData === 'function') {
      SetStoredData(STORAGE_KEY, value);
      return;
    }
  } catch {
    /* not available */
  }
  try {
    window.localStorage && window.localStorage.setItem(STORAGE_KEY, value);
  } catch {
    /* ignore */
  }
}

export function loadSettings(): BrowserSettings {
  const raw = readRaw();
  if (raw) {
    try {
      return { ...DEFAULT_SETTINGS, ...JSON.parse(raw) };
    } catch {
      /* corrupted, use defaults */
    }
  }
  return { ...DEFAULT_SETTINGS };
}

export function saveSettings(settings: BrowserSettings): void {
  writeRaw(JSON.stringify(settings));
}
