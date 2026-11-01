#!/usr/bin/env node
'use strict';
// Unit tests run the real Firefox background.js inside a simulated WebExtension API.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const root = path.resolve(__dirname, '../extensions/firefox');
const source = fs.readFileSync(path.join(root, 'background.js'), 'utf8');
const manifest = JSON.parse(fs.readFileSync(path.join(root, 'manifest.json'), 'utf8'));
assert.equal(manifest.browser_specific_settings.gecko.id, 'qdm@queen.local');
assert.ok(manifest.permissions.includes('downloads'));
assert.equal(manifest.background.persistent, true);

let total = 0;
async function test(name, config, item, check) {
  const calls = [];
  const listeners = {};
  const state = Object.assign({ token: 'a'.repeat(64), port: 17876, autoCapture: true }, config.storage || {});
  const browser = {
    storage: { local: {
      async get(request) {
        if (typeof request === 'string') return { [request]: state[request] };
        return Object.assign({}, request, state);
      },
      async set(x) { Object.assign(state, x); calls.push(['store', x]); }
    } },
    downloads: {
      onCreated: { addListener(fn) { listeners.created = fn; } },
      async pause(id) { calls.push(['pause', id]); if (config.pauseError) throw new Error('pause refused'); },
      async resume(id) { calls.push(['resume', id]); if (config.resumeError) throw new Error('resume refused'); },
      async cancel(id) { calls.push(['cancel', id]); if (config.cancelError) throw new Error('cancel refused'); },
      async erase(selector) { calls.push(['erase', selector.id]); }
    },
    runtime: {
      onInstalled: { addListener(fn) { listeners.installed = fn; } },
      onMessage: { addListener(fn) { listeners.message = fn; } },
      async openOptionsPage() { calls.push(['options']); }
    },
    contextMenus: {
      onClicked: { addListener(fn) { listeners.context = fn; } },
      async removeAll() {},
      create(x) { calls.push(['menu', x.id]); }
    },
    browserAction: {
      async setBadgeText(x) { calls.push(['badge', x.text]); },
      async setBadgeBackgroundColor() {}
    }
  };
  const ctx = vm.createContext({
    browser, URL, AbortSignal, console,
    fetch: async (_url, req) => {
      calls.push(['fetch', JSON.parse(req.body)]);
      if (config.fetchError) throw new Error('bridge offline');
      return { ok: config.responseStatus !== 400, async json() {
        return config.responseStatus === 400 ? { error: 'rejected' } : { accepted: 1, ids: ['sample-id'] };
      } };
    }
  });
  vm.runInContext(source, ctx, { filename: 'background.js' });
  assert.ok(listeners.created, 'Must register downloads.onCreated');
  await vm.runInContext('autoCapture(' + JSON.stringify(item) + ')', ctx);
  await check(calls, state, listeners);
  total++;
  process.stdout.write('PASS ' + name + '\n');
}
const url = 'https://files.example.test/download/item.bin';
const item = { id: 17, state: 'in_progress', url, filename: 'C:\\Downloads\\item.bin' };
const has = (calls, name) => calls.some(x => x[0] === name);
const untouched = calls => !calls.some(x => ['pause','resume','cancel','erase','fetch'].includes(x[0]));
(async () => {
  await test('64-byte token paired and successful capture', {}, item, (c, s) => {
    assert.deepEqual(c.filter(x => ['pause','cancel','erase'].includes(x[0])).map(x=>x[0]), ['cancel','erase']);
    assert.ok(c.findIndex(x=>x[0]==='fetch') < c.findIndex(x=>x[0]==='cancel'));
    assert.equal(c.find(x=>x[0]==='fetch')[1].filename, 'item.bin');
    assert.equal(s.lastMessage.includes('QDM'), true);
  });
  await test('unpaired browser downloads as normal', { storage: { token: '' } }, item, c => {
    assert.ok(untouched(c));
  });
  await test('opt-out browser downloads as normal', { storage: { autoCapture: false } }, item, c => {
    assert.ok(untouched(c));
  });
  await test('reject unsupported schemes', {}, { ...item, url: 'blob:https://site.test/x' }, c => {
    assert.ok(untouched(c));
  });
  await test('do not intercept other extensions', {}, { ...item, byExtensionId: 'other@test' }, c => {
    assert.ok(untouched(c));
  });
  await test('skip private mode', {}, { ...item, incognito: true }, c => {
    assert.ok(untouched(c));
  });
  await test('skip bridge local API', {}, { ...item, url: 'http://127.0.0.1:17876/downloads' }, c => {
    assert.ok(untouched(c));
  });
  await test('completed browser download stays in browser', {}, { ...item, state: 'complete' }, c => {
    assert.ok(!has(c,'pause') && !has(c,'fetch') && !has(c,'cancel'));
  });
  await test('bridge offline never interrupts original browser request', {fetchError: true}, item, c => {
    assert.ok(has(c,'fetch') && !has(c,'pause') && !has(c,'resume') && !has(c,'cancel'));
  });
  await test('bridge rejects request leaves original browser request running', {responseStatus:400}, item, c => {
    assert.ok(has(c,'fetch') && !has(c,'pause') && !has(c,'resume') && !has(c,'cancel'));
  });
  await test('browser cancellation fails, original has never been paused', {cancelError: true}, item, c => {
    assert.ok(has(c,'fetch') && !has(c,'pause') && !has(c,'resume') && !has(c,'erase'));
  });
  await test('successful capture without supplied filename', {}, { ...item, filename: undefined }, c => {
    assert.ok(!Object.hasOwn(c.find(x=>x[0]==='fetch')[1], 'filename'));
  });
  await test('menu installation and manual handoff remain', {}, item, async (_c,_s,l) => { assert.ok(l.installed && l.context && l.message); });
  console.log('PASS all ' + total + ' background test cases');
})().catch(e => { console.error(e.stack || e); process.exitCode = 1; });

