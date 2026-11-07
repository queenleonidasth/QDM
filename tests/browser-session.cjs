'use strict';
// Verifies the browser-session collector used by both QDM extensions. It is
// pure JavaScript and runs without a browser by stubbing the cookies API.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(
  path.join(__dirname, '..', 'extensions', 'chromium', 'session.js'),
  'utf8'
);

const calls = [];
const jar = [
  { name: 'cf_clearance', value: 'token', path: '/' },
  { name: 'session', value: 'value=1', path: '/app' },
  { name: 'bad;name', value: 'ignored', path: '/' },
  { name: 'bad\nvalue', value: 'ignored', path: '/' }
];
const context = {
  chrome: {
    cookies: {
      getAll: async ({ url }) => {
        calls.push(url);
        return jar;
      }
    }
  },
  navigator: { userAgent: 'Mozilla/5.0 Chrome/131', languages: ['en-US', 'th'] }
};
vm.createContext(context);
vm.runInContext(source, context);

(async () => {
  const headers = await context.QDMSession.browserSession(
    'https://example.com/file.zip',
    'https://example.com/page'
  );
  assert.equal(headers['User-Agent'], 'Mozilla/5.0 Chrome/131');
  assert.equal(headers['Accept-Language'], 'en-US,th');
  assert.equal(headers['Referer'], 'https://example.com/page');
  assert.equal(headers.Cookie, 'session=value=1; cf_clearance=token');
  assert.deepEqual(calls, ['https://example.com/file.zip']);

  const unsafeReferer = await context.QDMSession.browserSession(
    'https://example.com/file.zip',
    'javascript:alert(1)'
  );
  assert.equal(unsafeReferer['Referer'], undefined, 'only HTTP(S) referers are forwarded');

  context.chrome.cookies.getAll = async () => { throw new Error('cookies denied'); };
  const denied = await context.QDMSession.browserSession(
    'https://example.com/file.zip',
    'https://example.com/page'
  );
  assert.equal(denied.Cookie, undefined);
  assert.equal(denied['User-Agent'], 'Mozilla/5.0 Chrome/131', 'session failure is best effort');

  context.chrome.cookies.getAll = async () => [{ name: 'big', value: 'x'.repeat(20000), path: '/' }];
  const huge = await context.QDMSession.browserSession('https://example.com/file.zip');
  assert.equal(huge.Cookie.length, 16384, 'cookie header is capped');

  console.log('browser session forwarding: ok');
})().catch(error => {
  console.error(error);
  process.exit(1);
});
