'use strict';
const api = globalThis.browser || chrome;
const action = api.action || api.browserAction;
const inflight = new Set();
const defaults = { token: '', port: 17876, autoCapture: true, forwardSession: true };
const nativeHost = 'com.queen.qdm';
let pendingConnection = null;

async function settings() {
  return api.storage.local.get(defaults);
}
async function sessionHeaders(config, url, referer) {
  if (!config || config.forwardSession === false) return {};
  try { return await QDMSession.browserSession(url, referer); } catch (_) { return {}; }
}
function withSession(payload, headers) {
  return Object.keys(headers).length ? { ...payload, headers } : payload;
}
// A download's url is usually the page request that started it, while a
// redirect (including a Cloudflare verification hand-off) means the real file
// lives at finalUrl. Wait for the redirect chain, then prefer the final URL and
// the final host's cookies. HTML pages are not files, so leave them to the
// browser instead of handing a challenge page to QDM.
async function resolvedDownload(item) {
  await new Promise(resolve => setTimeout(resolve, 400));
  let current = item;
  try {
    const [found] = await api.downloads.search({ id: item.id });
    if (found) current = found;
  } catch (_) { /* fall back to the event payload */ }
  if (current.state === 'interrupted' || current.state === 'complete' || current.error) return null;
  const url = current.finalUrl || item.finalUrl || item.url;
  const mime = String(current.mime || item.mime || '').toLowerCase();
  let isHtmlPath = false;
  try { isHtmlPath = /\.(?:html?|xhtml)$/i.test(new URL(url).pathname); } catch (_) {}
  if (mime.startsWith('text/html') && !isHtmlPath) return null;
  return {
    url,
    referrer: item.referrer || current.referrer || '',
    filename: current.filename || item.filename
  };
}
function valid(config) {
  return /^[a-f0-9]{64}$/i.test(config?.token || '') &&
    Number.isInteger(config?.port) && config.port >= 17876 && config.port <= 17885;
}
async function health(config) {
  if (!valid(config)) return false;
  try {
    const response = await fetch('http://127.0.0.1:' + config.port + '/health', {
      headers: { Authorization: 'Bearer ' + config.token },
      signal: AbortSignal.timeout(1700)
    });
    return response.ok && (await response.json()).name === 'QDM';
  } catch (_) { return false; }
}
async function discoverNative(start = true) {
  const found = await api.runtime.sendNativeMessage(nativeHost, { action: 'discover', start });
  if (!found?.ok || !valid(found)) {
    throw new Error(found?.error || 'ไม่พบ QDM Native Host ที่ติดตั้งบนเครื่อง');
  }
  await api.storage.local.set({ token: found.token, port: found.port });
  return { ...await settings(), token: found.token, port: found.port };
}
async function resolveConnection() {
  if (pendingConnection) return pendingConnection;
  pendingConnection = (async () => {
    const saved = await settings();
    if (valid(saved)) {
      if (await health(saved)) return saved;
      // The OS may allocate another bridge port after a restart.
      for (let port = 17876; port <= 17885; port++) {
        if (port === saved.port) continue;
        const candidate = { ...saved, port };
        if (await health(candidate)) {
          await api.storage.local.set({ port });
          return candidate;
        }
      }
    }
    // Native messaging authenticates this extension ID and reads the
    // current Windows-DPAPI-protected pairing secret without user input.
    // It also launches QDM when necessary.
    return discoverNative(true);
  })().finally(() => { pendingConnection = null; });
  return pendingConnection;
}
function eligible(item) {
  if (!item || !Number.isInteger(item.id)) return false;
  if (item.state && item.state !== 'in_progress') return false;
  if (item.byExtensionId || item.incognito) return false;
  if (typeof item.url !== 'string' || !/^https?:\/\//i.test(item.url)) return false;
  try {
    const u = new URL(item.url);
    if (u.username || u.password) return false;
    if (u.hostname === '127.0.0.1' && /^\/(health|downloads)$/.test(u.pathname) &&
        Number(u.port) >= 17876 && Number(u.port) <= 17885) return false;
    return true;
  } catch (_) { return false; }
}
function basename(filename) {
  if (typeof filename !== 'string') return undefined;
  const name = filename.split(/[\\/]/).pop();
  return name && name !== '.' && name !== '..' ? name : undefined;
}
async function feedback(message, error = false) {
  await api.storage.local.set({ lastMessage: message });
  await action.setBadgeText({ text: error ? '!' : '✓' });
  await action.setBadgeBackgroundColor({ color: error ? '#a84b3d' : '#347f60' });
}
// The browser's copy is only cancelled after QDM proves it is receiving bytes.
// A Cloudflare challenge or an expired one-time link then leaves the browser's
// working download alone instead of losing both copies.
async function qdmStarted(paired, id, timeout = 15000) {
  if (!id) return false;
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, 500));
    try {
      const response = await fetch('http://127.0.0.1:' + paired.port + '/task-status?id=' + encodeURIComponent(id), {
        headers: { Authorization: 'Bearer ' + paired.token },
        signal: AbortSignal.timeout(2500)
      });
      if (!response.ok) return false;
      const task = await response.json();
      if (task.status === 'completed') return true;
      if (task.status === 'downloading' && Number(task.downloadedBytes) > 0) return true;
      if (task.status === 'error' || task.status === 'failed') return false;
    } catch (_) { return false; }
  }
  return false;
}
async function sendDownload(payload, paired) {
  const response = await fetch('http://127.0.0.1:' + paired.port + '/downloads', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + paired.token },
    body: JSON.stringify(payload),
    signal: AbortSignal.timeout(8000)
  });
  // A rejected authentication never accepted the download: safe to refresh.
  if (response.status === 401) {
    const error = new Error('QDM pairing needs a refresh');
    error.authorizationRejected = true;
    throw error;
  }
  const result = await response.json();
  if (!response.ok || !result || result.accepted !== (payload.urls ? payload.urls.length : 1)) {
    throw new Error((result && result.error) || 'QDM ไม่รับรายการดาวน์โหลด');
  }
  return result;
}
async function handoff(payload, _config = null, openOptions = false) {
  try {
    const paired = await resolveConnection();
    try {
      return await sendDownload(payload, paired);
    } catch (error) {
      if (!error.authorizationRejected) throw error;
      // If QDM replaced its secret between health and POST, re-pair once.
      return await sendDownload(payload, await discoverNative(true));
    }
  } catch (error) {
    if (openOptions) await api.runtime.openOptionsPage();
    throw error;
  }
}
async function autoCapture(item) {
  if (!eligible(item) || inflight.has(item.id)) return;
  inflight.add(item.id);
  try {
    const config = await settings();
    if (!config.autoCapture) return;
    // Do not pause the browser request before ownership is acknowledged:
    // a pre-body pause can make Firefox downloads impossible to resume.
    const target = await resolvedDownload(item);
    if (!target) return;
    const file = basename(target.filename);
    const headers = await sessionHeaders(config, target.url, target.referrer);
    const payload = withSession({ url: target.url, ...(file ? { filename: file } : {}) }, headers);
    let accepted;
    try {
      accepted = await handoff(payload);
    } catch (error) {
      await feedback('QDM ไม่พร้อม — ให้ Zen ดาวน์โหลดตามปกติ: ' + error.message, true);
      return;
    }
    const paired = await resolveConnection();
    if (!(await qdmStarted(paired, accepted.ids && accepted.ids[0]))) {
      await feedback('QDM เริ่มดาวน์โหลดไม่ได้ — ให้ Zen ดาวน์โหลดต่อ', true);
      return;
    }
    let current = null;
    try { [current] = await api.downloads.search({ id: item.id }); } catch (_) { /* optional */ }
    if (current && current.state === 'complete') {
      await feedback('Zen โหลดเสร็จแล้ว — QDM มีสำเนาแยก');
      return;
    }
    try {
      await api.downloads.cancel(item.id);
      try { await api.downloads.erase({ id: item.id }); } catch (_) { /* optional history cleanup */ }
      await feedback('ย้ายการดาวน์โหลดเข้า QDM แล้ว (' + accepted.accepted + ' รายการ)');
    } catch (_) {
      await feedback('QDM รับงานแล้ว แต่ Zen ยกเลิกงานเดิมไม่สำเร็จ (อาจมีไฟล์ซ้ำ)', true);
    }
  } catch (error) {
    try { await feedback('Zen ดาวน์โหลดต่อ: ' + error.message, true); } catch (_) {}
  } finally {
    inflight.delete(item.id);
  }
}
api.downloads.onCreated.addListener(item => { void autoCapture(item); });
api.runtime.onInstalled.addListener(async () => {
  await api.contextMenus.removeAll();
  api.contextMenus.create({ id: 'qdm-download', title: 'ดาวน์โหลดด้วย QDM', contexts: ['link', 'image', 'video', 'audio'] });
  const stored = await api.storage.local.get('autoCapture');
  if (typeof stored.autoCapture !== 'boolean') await api.storage.local.set({ autoCapture: true });
});
api.runtime.onStartup.addListener(() => {
  // Refresh after browser/Windows restart; the saved token is a fallback.
  void discoverNative(true).catch(() => {});
});
api.contextMenus.onClicked.addListener(async info => {
  if (info.menuItemId !== 'qdm-download') return;
  try {
    const url = info.linkUrl || info.srcUrl;
    if (!/^https?:\/\//i.test(url || '')) throw new Error('ลิงก์นี้ไม่ใช่ HTTP(S) โดยตรง');
    const config = await settings();
    const headers = await sessionHeaders(config, url, info.pageUrl || info.frameUrl || '');
    await handoff(withSession({ urls: [url] }, headers), null, true);
    await feedback('ส่งลิงก์เข้า QDM แล้ว');
  } catch (error) { await feedback(error.message, true); }
});
api.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (message?.action === 'qdm-status') {
    resolveConnection()
      .then(c => sendResponse({ ok: true, port: c.port }),
            error => sendResponse({ ok: false, error: error.message }));
    return true;
  }
  if (message?.action !== 'qdm-send') return false;
  (async () => {
    const urls = Array.isArray(message.urls) ? message.urls : [];
    const config = await settings();
    let headers = {};
    // A single URL may use the referring tab's session. A batch can mix hosts,
    // so forwarding one cookie jar to every URL would leak it across sites.
    if (urls.length === 1) {
      let referer = typeof message.referrer === 'string' ? message.referrer : '';
      if (!referer) {
        try {
          const [tab] = await api.tabs.query({ active: true, currentWindow: true });
          referer = tab?.url || '';
        } catch (_) { /* tab context is optional */ }
      }
      headers = await sessionHeaders(config, urls[0], referer);
    }
    return handoff(withSession({ urls }, headers), null, true);
  })().then(result => sendResponse({ ok: true, ...result }),
          error => sendResponse({ ok: false, error: error.message }));
  return true;
});
