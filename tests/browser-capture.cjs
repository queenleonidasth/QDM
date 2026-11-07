'use strict';
// Drives the Chromium background service worker with a stubbed browser to verify
// redirect resolution, session headers, HTML/complete skips and the rule that
// the browser's own download is only cancelled after QDM starts receiving bytes.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const extension = path.join(__dirname, '..', 'extensions', 'chromium');
const sessionSource = fs.readFileSync(path.join(extension, 'session.js'), 'utf8');
const backgroundSource = fs.readFileSync(path.join(extension, 'background.js'), 'utf8');

const requests = [];
const cancelled = [];
let captured = null;
let qdmTask = { status: 'downloading', downloadedBytes: 1024 };
const config = { token: 'a'.repeat(64), port: 17876, autoCapture: true, forwardSession: true };
const browserDownloads = new Map();
const context = {
  URL,
  console,
  setTimeout,
  AbortSignal,
  navigator: { userAgent: 'Mozilla/5.0 Chrome/131', languages: ['en-US', 'th'] },
  chrome: {
    storage: {
      local: {
        get: async defaults => ({ ...defaults, ...config }),
        set: async values => Object.assign(config, values)
      }
    },
    cookies: {
      getAll: async ({ url }) => {
        context.cookieUrls.push(url);
        return [{ name: 'cf_clearance', value: 'ok', path: '/' }];
      }
    },
    runtime: {
      onInstalled: { addListener() {} },
      onStartup: { addListener() {} },
      onMessage: { addListener() {} },
      sendNativeMessage: async () => { throw new Error('native host unavailable'); }
    },
    downloads: {
      onCreated: { addListener: listener => { captured = listener; } },
      search: async ({ id }) => browserDownloads.has(id) ? [browserDownloads.get(id)] : [],
      cancel: async id => { cancelled.push(id); },
      erase: async () => {}
    },
    contextMenus: { removeAll: async () => {}, create() {}, onClicked: { addListener() {} } },
    action: { setBadgeText: async () => {}, setBadgeBackgroundColor: async () => {} },
    tabs: { query: async () => [{ url: 'https://example.com/page' }] }
  },
  importScripts: () => vm.runInContext(sessionSource, context),
  fetch: async (url, options = {}) => {
    requests.push({ url: String(url), method: options.method || 'GET', body: options.body });
    if (String(url).includes('/task-status')) {
      return { ok: true, status: 200, json: async () => qdmTask };
    }
    if (String(url).endsWith('/health')) {
      return { ok: true, status: 200, json: async () => ({ name: 'QDM' }) };
    }
    return { ok: true, status: 201, json: async () => ({ accepted: 1, ids: ['task'] }) };
  }
};
context.cookieUrls = [];
vm.createContext(context);
vm.runInContext(backgroundSource, context);

const item = id => ({
  id,
  url: 'http://127.0.0.1:8129/start',
  referrer: 'http://127.0.0.1:8129/',
  filename: 'C:\\Users\\QUEEN\\Downloads\\session.bin',
  mime: 'application/octet-stream',
  state: 'in_progress'
});
const settle = ms => new Promise(resolve => setTimeout(resolve, ms));

(async () => {
  assert.equal(typeof captured, 'function', 'the download listener is registered');

  browserDownloads.set(7, {
    ...item(7),
    finalUrl: 'http://127.0.0.1:8129/download.bin',
    mime: 'application/octet-stream',
    state: 'in_progress'
  });
  captured(item(7));
  await settle(2500);
  const first = requests.find(request => request.method === 'POST');
  assert.ok(first, 'an accepted capture posts to QDM');
  const forwarded = JSON.parse(first.body);
  assert.equal(forwarded.url, 'http://127.0.0.1:8129/download.bin', 'the redirected-final URL is forwarded');
  assert.equal(forwarded.filename, 'session.bin');
  assert.equal(forwarded.headers.Cookie, 'cf_clearance=ok');
  assert.equal(forwarded.headers.Referer, 'http://127.0.0.1:8129/');
  assert.equal(forwarded.headers['User-Agent'], 'Mozilla/5.0 Chrome/131');
  assert.equal(context.cookieUrls.at(-1), 'http://127.0.0.1:8129/download.bin', 'cookies are read for the final host');
  assert.deepEqual(cancelled, [7], 'the browser copy is cancelled only after QDM starts');

  // A real failure in QDM leaves the browser's working download untouched.
  cancelled.length = 0;
  requests.length = 0;
  qdmTask = { status: 'error', downloadedBytes: 0 };
  browserDownloads.set(8, { ...item(8), finalUrl: 'http://127.0.0.1:8129/download.bin', state: 'in_progress' });
  captured(item(8));
  await settle(2500);
  assert.equal(requests.filter(request => request.method === 'POST').length, 1, 'the failed task was still offered to QDM');
  assert.deepEqual(cancelled, [], 'the browser keeps its download when QDM fails');

  // Session forwarding can be disabled without losing capture.
  cancelled.length = 0;
  requests.length = 0;
  qdmTask = { status: 'downloading', downloadedBytes: 1024 };
  config.forwardSession = false;
  browserDownloads.set(9, { ...item(9), finalUrl: 'http://127.0.0.1:8129/download.bin', state: 'in_progress' });
  captured(item(9));
  await settle(2500);
  const disabled = requests.find(request => request.method === 'POST');
  assert.ok(disabled, 'the download is still captured with forwarding disabled');
  assert.equal(JSON.parse(disabled.body).headers, undefined, 'no session headers when disabled');

  // HTML challenge pages and already-finished browser downloads stay in the browser.
  requests.length = 0;
  cancelled.length = 0;
  browserDownloads.set(10, {
    ...item(10),
    finalUrl: 'https://testfile.org/files-5GB-zip',
    mime: 'text/html',
    state: 'in_progress'
  });
  captured(item(10));
  await settle(1200);
  assert.equal(requests.filter(request => request.method === 'POST').length, 0, 'HTML challenge pages are left to the browser');

  browserDownloads.set(11, { ...item(11), finalUrl: 'http://127.0.0.1:8129/download.bin', state: 'complete' });
  captured(item(11));
  await settle(1200);
  assert.equal(requests.filter(request => request.method === 'POST').length, 0, 'finished browser downloads are not re-fetched');

  console.log('browser capture forwarding: ok');
})().catch(error => {
  console.error(error);
  process.exit(1);
});
