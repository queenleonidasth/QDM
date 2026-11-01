'use strict';
const api = globalThis.browser || chrome;
const token = document.getElementById('token'), port = document.getElementById('port'), message = document.getElementById('message');
api.storage.local.get({ token: '', port: 17876 }).then(config => { token.value = config.token; port.value = config.port; });
document.getElementById('pair-form').addEventListener('submit', async event => {
  event.preventDefault();
  const config = { token: token.value.trim(), port: Number(port.value) };
  if (!/^[0-9a-f]{64}$/.test(config.token) || config.port < 17876 || config.port > 17885) { message.textContent = 'กรุณาคัดลอกรหัสและพอร์ตจาก QDM ให้ครบ'; return; }
  message.textContent = 'กำลังเชื่อมต่อ…';
  try {
    const response = await fetch(`http://127.0.0.1:${config.port}/health`, { headers: { Authorization: `Bearer ${config.token}` }, signal: AbortSignal.timeout(5000) });
    if (!response.ok) throw new Error('รหัสไม่ตรง หรือ QDM ยังไม่พร้อม');
    const result = await response.json();
    if (result.name !== 'QDM') throw new Error('ไม่พบ QDM ที่พอร์ตนี้');
    await api.storage.local.set(config);
    message.textContent = 'เชื่อมต่อแล้ว คลิกขวาที่ลิงก์ → ดาวน์โหลดด้วย QDM ได้เลย';
  } catch (error) { message.textContent = `เชื่อมต่อไม่สำเร็จ: ${error.message} — ตรวจสอบว่า QDM เปิดอยู่`; }
});
