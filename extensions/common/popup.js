'use strict';
const api = globalThis.browser || chrome;
const message = document.getElementById('message');
api.storage.local.get({ lastMessage: '' }).then(value => { message.textContent = value.lastMessage; });
(api.action || api.browserAction).setBadgeText({ text: '' });
document.getElementById('settings').addEventListener('click', () => api.runtime.openOptionsPage());
document.getElementById('send').addEventListener('click', async () => {
  const urls = document.getElementById('urls').value.split(/\r?\n/).map(x => x.trim()).filter(Boolean);
  if (!urls.length || urls.length > 200 || urls.some(x => !/^https?:\/\//i.test(x))) { message.textContent = 'ใส่ลิงก์ HTTP(S) 1–200 ลิงก์ บรรทัดละลิงก์'; return; }
  const button = document.getElementById('send'); button.disabled = true;
  try {
    const result = await api.runtime.sendMessage({ action: 'qdm-send', urls });
    if (!result?.ok) throw new Error(result?.error || 'เปิด QDM แล้วลองอีกครั้ง');
    message.textContent = `เพิ่ม ${result.accepted} รายการใน QDM แล้ว`; document.getElementById('urls').value = '';
  } catch (error) { message.textContent = error.message; }
  finally { button.disabled = false; }
});
