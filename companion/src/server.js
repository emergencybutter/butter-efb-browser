import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { WebSocketServer } from 'ws';
import { BrowserHost } from './browser-host.js';
import { config, ROOT_DIR } from './config.js';

const VERSION = '0.1.0';
const PUBLIC_DIR = path.join(ROOT_DIR, 'public');
const ACK_TIMEOUT_MS = 2000;
const MIME = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml' };

let host = null;
const clients = new Set();

// ------------------------------------------------------------------ HTTP

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://localhost');
  res.setHeader('Access-Control-Allow-Origin', '*');

  // Latest frame as a plain JPEG (used by the "http" frame transport).
  if (/^\/frame(\/\d+)?\.jpg$/.test(url.pathname)) {
    const buf = host && host.frameBuffer();
    if (!buf) {
      res.writeHead(204).end();
      return;
    }
    res.writeHead(200, { 'Content-Type': 'image/jpeg', 'Content-Length': buf.length, 'Cache-Control': 'no-store' });
    res.end(buf);
    return;
  }

  if (url.pathname === '/status') {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ version: VERSION, clients: clients.size, state: host && host.currentState() }));
    return;
  }

  // Debug viewer: open http://127.0.0.1:8737/ in a normal browser to test without the sim.
  const rel = url.pathname === '/' ? 'index.html' : url.pathname.slice(1);
  const file = path.join(PUBLIC_DIR, path.normalize(rel));
  if (!file.startsWith(PUBLIC_DIR)) {
    res.writeHead(403).end();
    return;
  }
  fs.readFile(file, (err, data) => {
    if (err) {
      res.writeHead(404).end('Not found');
      return;
    }
    res.writeHead(200, { 'Content-Type': MIME[path.extname(file)] || 'application/octet-stream' });
    res.end(data);
  });
});

// ------------------------------------------------------------------ WebSocket

const wss = new WebSocketServer({ server, maxPayload: 1024 * 1024 });

function send(client, msg) {
  if (client.ws.readyState === client.ws.OPEN) client.ws.send(JSON.stringify(msg));
}

function broadcast(msg) {
  for (const c of clients) send(c, msg);
}

/** Frames are flow-controlled per client: one in flight, newest wins. */
function sendFrame(client) {
  const f = host && host.lastFrame;
  if (!f || client.paused) return;
  if (client.awaitingAck && Date.now() - client.sentAt < ACK_TIMEOUT_MS) {
    client.dirty = true;
    return;
  }
  if (client.lastSentId === f.id) return;
  client.awaitingAck = true;
  client.dirty = false;
  client.sentAt = Date.now();
  client.lastSentId = f.id;
  const msg = { type: 'frame', id: f.id, width: f.width, height: f.height };
  if (client.transport !== 'http') msg.data = f.data;
  send(client, msg);
}

function updateStreaming() {
  if (!host) return;
  const anyActive = [...clients].some((c) => c.configured && !c.paused);
  host.enqueue(() => host.setStreaming(anyActive));
}

wss.on('connection', (ws, req) => {
  const client = { ws, transport: 'data', paused: false, configured: false, awaitingAck: false, dirty: false, sentAt: 0, lastSentId: 0 };
  clients.add(client);
  console.log(`[ws] client connected from ${req.socket.remoteAddress} (${clients.size} total)`);
  send(client, { type: 'hello', version: VERSION, home: config.home });

  ws.on('message', (raw) => {
    let msg;
    try {
      msg = JSON.parse(raw.toString());
    } catch {
      return;
    }
    handleMessage(client, msg);
  });

  ws.on('close', () => {
    clients.delete(client);
    console.log(`[ws] client disconnected (${clients.size} total)`);
    updateStreaming();
  });
  ws.on('error', () => {});
});

function handleMessage(client, msg) {
  if (!host) return;
  switch (msg.type) {
    case 'config': {
      if (msg.transport === 'http' || msg.transport === 'data') client.transport = msg.transport;
      client.configured = true;
      client.lastSentId = 0;
      client.awaitingAck = false;
      host.enqueue(async () => {
        await host.configure(msg);
        updateStreaming();
        const state = host.currentState();
        if (state) send(client, { type: 'state', ...state });
        sendFrame(client);
      });
      break;
    }
    case 'ack':
      client.awaitingAck = false;
      if (client.dirty) sendFrame(client);
      break;
    case 'pause':
      client.paused = true;
      updateStreaming();
      break;
    case 'resume':
      client.paused = false;
      client.lastSentId = 0;
      client.awaitingAck = false;
      updateStreaming();
      sendFrame(client);
      break;
    case 'navigate':
      host.enqueue(() => host.navigate(msg.url));
      break;
    case 'home':
      host.enqueue(() => host.navigate(config.home));
      break;
    case 'back':
      host.enqueue(() => host.historyGo(-1));
      break;
    case 'forward':
      host.enqueue(() => host.historyGo(1));
      break;
    case 'reload':
      host.enqueue(() => host.reload());
      break;
    case 'stop':
      host.enqueue(() => host.stop());
      break;
    case 'closeTab':
      host.enqueue(() => host.closeTab());
      break;
    case 'mouse':
      host.enqueue(() => host.mouse(msg));
      break;
    case 'wheel':
      host.enqueue(() => host.wheel(msg));
      break;
    case 'key':
      host.enqueue(() => host.key(msg));
      break;
    case 'text':
      host.enqueue(() => host.text(msg));
      break;
    case 'select':
      host.enqueue(() => host.selectOption(msg));
      break;
    case 'dialog':
      host.enqueue(() => host.dialogResponse(msg));
      break;
    case 'ping':
      send(client, { type: 'pong', t: msg.t });
      break;
  }
}

// ------------------------------------------------------------------ browser lifecycle

async function startBrowser() {
  host = new BrowserHost(config);
  host.on('frame', () => {
    for (const c of clients) sendFrame(c);
  });
  host.on('state', (state) => broadcast({ type: 'state', ...state }));
  host.on('focus', (focus) => broadcast({ type: 'focus', ...focus }));
  host.on('select', (sel) => broadcast({ type: 'select', ...sel }));
  host.on('dialog', (dialog) => broadcast({ type: 'dialog', dialog }));
  host.on('notice', (n) => {
    console.log(`[browser] ${n.level}: ${n.message}`);
    broadcast({ type: 'notice', ...n });
  });
  host.on('crash', () => {
    console.error('[browser] browser process exited, restarting in 2s...');
    broadcast({ type: 'notice', level: 'error', message: 'Browser crashed, restarting...' });
    host = null;
    setTimeout(() => startBrowser().then(() => {
      for (const c of clients) {
        c.lastSentId = 0;
        c.awaitingAck = false;
      }
      updateStreaming();
    }), 2000);
  });
  await host.start();
}

async function main() {
  console.log(`EFB Browser Companion v${VERSION}`);
  console.log(`  browser : ${config.browserPath || '(not found)'}`);
  console.log(`  profile : ${config.profileDir}`);
  console.log(`  home    : ${config.home}`);

  await startBrowser();

  server.listen(config.port, config.host, () => {
    console.log(`\nListening on ws://${config.host}:${config.port}`);
    console.log(`Test viewer: http://${config.host === '0.0.0.0' ? '127.0.0.1' : config.host}:${config.port}/`);
    console.log('Leave this window open while using the Browser app on the EFB. Ctrl+C to quit.\n');
  });
  server.on('error', (err) => {
    console.error(`Cannot listen on port ${config.port}: ${err.message}`);
    shutdown(1);
  });
}

let shuttingDown = false;
async function shutdown(code = 0) {
  if (shuttingDown) return;
  shuttingDown = true;
  console.log('Shutting down...');
  for (const c of clients) c.ws.close();
  if (host) await host.close();
  process.exit(code);
}

process.on('SIGINT', () => shutdown(0));
process.on('SIGTERM', () => shutdown(0));

main().catch((err) => {
  console.error('Fatal:', err && err.message ? err.message : err);
  shutdown(1);
});
