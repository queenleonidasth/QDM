'use strict';
const api = globalThis.browser || chrome;
const token = document.getElementById('token');
const port = document.getElementById('port');
const toggle = document.getElementById('autoCapture');
const sessionToggle = document.getElementById('forwardSession');
const message = document.getElementById('message');
api.storage.local.get({ token: '', port: 17876, autoCapture: true, forwardSession: true }).then(config => {
  token.value = config.token;
  port.value = config.port;
  toggle.checked = config.autoCapture;
  sessionToggle.checked = config.forwardSession !== false;
});
toggle.addEventListener('change', async () => {
  await api.storage.local.set({ autoCapture: toggle.checked });
  message.textContent = toggle.checked ? 'เปิดดักดาวน์โหลดอัตโนมัติแล้ว' : 'ปิดดักดาวน์โหลดอัตโนมัติแล้ว';
});
sessionToggle.addEventListener('change', async () => {
  await api.storage.local.set({ forwardSession: sessionToggle.checked });
  message.textContent = sessionToggle.checked
    ? 'เปิดส่ง session แล้ว — QDM จะได้รับคุกกี้/Referer/User-Agent ของไฟล์ที่ดัก'
    : 'ปิดส่ง session แล้ว — เว็บที่ต้องล็อกอินจะเก็บ session ไว้ในเบราว์เซอร์';
});
document.getElementById('pair-form').addEventListener('submit', async event => {
  event.preventDefault();
  const config = { token: token.value.trim(), port: Number(port.value) };
  if (!/^[0-9a-f]{64}$/i.test(config.token) || config.port < 17876 || config.port > 17885) {
    message.textContent = 'กรุณาคัดลอกรหัสและพอร์ตจาก QDM ให้ครบ';
    return;
  }
  message.textContent = 'กำลังเชื่อมต่อ…';
  try {
    const response = await fetch('http://127.0.0.1:' + config.port + '/health', {
      headers: { Authorization: 'Bearer ' + config.token }, signal: AbortSignal.timeout(5000)
    });
    if (!response.ok) throw new Error('รหัสไม่ตรง หรือ QDM ยังไม่พร้อม');
    const result = await response.json();
    if (result.name !== 'QDM') throw new Error('ไม่พบ QDM ที่พอร์ตนี้');
    await api.storage.local.set(config);
    message.textContent = 'เชื่อมต่อแล้ว — Zen จะส่งไฟล์ที่กดดาวน์โหลดเข้า QDM โดยอัตโนมัติ';
  } catch (error) {
    message.textContent = 'เชื่อมต่อไม่สำเร็จ: ' + error.message + ' — ตรวจสอบว่า QDM เปิดอยู่';
  }
});

document.getElementById('check-connection').addEventListener('click', async () => {
  message.textContent = 'กำลังตรวจสอบการเชื่อมต่อ QDM…';
  try {
    const result = await api.runtime.sendMessage({ action: 'qdm-status' });
    if (!result?.ok) throw new Error(result?.error || 'ไม่สามารถเชื่อมต่อ QDM');
    const config = await api.storage.local.get({ token: '', port: 17876 });
    token.value = config.token;
    port.value = config.port;
    message.textContent = 'เชื่อมต่อ QDM อัตโนมัติแล้ว (พอร์ต ' + result.port + ') — ไม่ต้องกรอกรหัส';
  } catch (error) { message.textContent = 'เชื่อมต่ออัตโนมัติไม่สำเร็จ: ' + error.message; }
});
