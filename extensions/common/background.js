'use strict';
const api = globalThis.browser || chrome;
async function settings() { return api.storage.local.get({ token: '', port: 17876 }); }
async function handoff(urls) {
  const config = await settings();
  if (!config.token) { await api.runtime.openOptionsPage(); throw new Error('กรุณาจับคู่กับ QDM ก่อน'); }
  const response = await fetch(`http://127.0.0.1:${config.port}/downloads`, {
    method: 'POST', headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${config.token}` },
    body: JSON.stringify({ urls }), signal: AbortSignal.timeout(10000)
  });
  const result = await response.json();
  if (!response.ok) throw new Error(result.error || 'ส่งลิงก์ไม่สำเร็จ');
  return result;
}
api.runtime.onInstalled.addListener(async () => {
  await api.contextMenus.removeAll();
  api.contextMenus.create({ id: 'qdm-download', title: 'ดาวน์โหลดด้วย QDM', contexts: ['link', 'image', 'video', 'audio'] });
});
api.contextMenus.onClicked.addListener(async info => {
  if (info.menuItemId !== 'qdm-download') return;
  const action = api.action || api.browserAction;
  try {
    const url = info.linkUrl || info.srcUrl;
    if (!/^https?:\/\//i.test(url || '')) throw new Error('ลิงก์นี้ไม่ใช่ HTTP(S) โดยตรง');
    await handoff([url]);
    await api.storage.local.set({ lastMessage: 'ส่งลิงก์เข้า QDM แล้ว' });
    await action.setBadgeText({ text: '✓' });
    await action.setBadgeBackgroundColor({ color: '#347f60' });
  } catch (error) {
    await api.storage.local.set({ lastMessage: error.message });
    await action.setBadgeText({ text: '!' });
    await action.setBadgeBackgroundColor({ color: '#a84b3d' });
  }
});
api.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (message?.action !== 'qdm-send') return false;
  handoff(message.urls).then(result => sendResponse({ ok: true, ...result }), error => sendResponse({ ok: false, error: error.message }));
  return true;
});
