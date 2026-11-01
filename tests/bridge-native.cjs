#!/usr/bin/env node
'use strict';

// End-to-end acceptance of the executable's authenticated browser bridge.
// Node standard-library HTTP/TCP clients, a separate origin fixture, and an
// independently hashed output file are used instead of importing bridge code.
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const http = require('node:http');
const net = require('node:net');
const { CliClient, fileHash } = require('./helpers/cli-client.cjs');
const { createHttpFixture, MiB } = require('./helpers/http-fixture.cjs');

function argument(name, fallback) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : fallback;
}
const binary = path.resolve(argument('--binary', process.env.QDM_BINARY ||
  path.join(__dirname, '..', 'target', 'release', process.platform === 'win32' ? 'QDM.exe' : 'QDM')));
const reportPath = argument('--report', null);
const preserve = process.argv.includes('--keep');
const chromiumOrigin = 'chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
const firefoxOrigin = 'moz-extension://11111111-2222-4333-8444-555555555555';
const cases = [];
let context;
const test = (name, run) => cases.push({ name, run });

async function request(options = {}) {
  const body = options.body === undefined ? null :
    Buffer.isBuffer(options.body) ? options.body : Buffer.from(String(options.body));
  const headers = {
    Host: `127.0.0.1:${context.integration.port}`,
    ...(options.auth === false ? {} : { Authorization: `Bearer ${context.integration.token}` }),
    ...(body === null ? {} : { 'Content-Length': String(body.length), 'Content-Type': 'application/json' }),
    ...options.headers,
  };
  for (const [name, value] of Object.entries(headers)) if (value === null) delete headers[name];
  return new Promise((resolve, reject) => {
    const req = http.request({ hostname: '127.0.0.1', port: context.integration.port,
      path: options.route || '/health', method: options.method || 'GET', headers, agent: false }, (res) => {
      const chunks = [];
      let length = 0;
      res.on('data', (chunk) => {
        length += chunk.length;
        if (length > MiB) { res.destroy(new Error('Bridge response exceeded 1 MiB')); return; }
        chunks.push(chunk);
      });
      res.on('error', reject);
      res.on('end', () => {
        const text = Buffer.concat(chunks).toString('utf8');
        let value = null;
        try { if (text) value = JSON.parse(text); }
        catch { reject(new Error(`Non-JSON bridge response, status ${res.statusCode}`)); return; }
        resolve({ status: res.statusCode, headers: res.headers, value });
      });
    });
    req.setTimeout(15000, () => req.destroy(new Error('Bridge HTTP request timed out')));
    req.on('error', reject);
    req.end(body);
  });
}

async function rawRequest(value) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    const socket = net.createConnection({ host: '127.0.0.1', port: context.integration.port }, () => socket.end(value));
    socket.setTimeout(15000, () => socket.destroy(new Error('Bridge raw request timed out')));
    socket.on('error', reject);
    socket.on('data', (chunk) => chunks.push(chunk));
    socket.on('end', () => {
      const text = Buffer.concat(chunks).toString('utf8');
      const match = /^HTTP\/1\.1 (\d{3})\b/.exec(text);
      if (!match) { reject(new Error('Bridge closed without an HTTP response')); return; }
      resolve({ status: Number(match[1]), bytes: Buffer.byteLength(text) });
    });
  });
}

const post = (value, options = {}) => request({ method: 'POST', route: '/downloads', body: JSON.stringify(value), ...options });
async function count() { return (await context.client.snapshot()).downloads.length; }
async function rejectedPost(value, expectedStatus, options = {}) {
  const before = await count();
  const response = await post(value, options);
  assert.equal(response.status, expectedStatus);
  assert.equal(await count(), before, 'Rejected request unexpectedly added a download');
  return response;
}

test('authenticated health and cache-control', async () => {
  const response = await request();
  assert.equal(response.status, 200);
  assert.equal(response.value.name, 'QDM');
  assert.ok(/^\d+\.\d+\.\d+/.test(response.value.version));
  assert.equal(response.headers['cache-control'], 'no-store');
  assert.equal(response.headers['x-content-type-options'], 'nosniff');
  assert.equal(response.headers['access-control-allow-origin'], undefined);
  return { status: response.status, version: response.value.version };
});

test('missing and incorrect bearer authentication rejected', async () => {
  assert.equal((await request({ auth: false })).status, 401);
  assert.equal((await request({ headers: { Authorization: 'Bearer definitely-incorrect' } })).status, 401);
  assert.equal((await request({ headers: { Authorization: `Bearer ${context.integration.token}x` } })).status, 401);
  await rejectedPost({ url: context.fixture.url }, 401, { auth: false });
  return { unauthorizedStatuses: [401, 401, 401, 401] };
});

test('extension origins allowed with exact reflected CORS', async () => {
  for (const origin of [chromiumOrigin, firefoxOrigin]) {
    const response = await request({ headers: { Origin: origin } });
    assert.equal(response.status, 200);
    assert.equal(response.headers['access-control-allow-origin'], origin);
    assert.equal(response.headers.vary, 'Origin');
  }
  const preflight = await request({ method: 'OPTIONS', route: '/downloads', auth: false,
    headers: { Origin: chromiumOrigin, 'Access-Control-Request-Method': 'POST',
      'Access-Control-Request-Headers': 'authorization,content-type' } });
  assert.equal(preflight.status, 204);
  assert.equal(preflight.value, null);
  assert.equal(preflight.headers['access-control-allow-origin'], chromiumOrigin);
  assert.match(preflight.headers['access-control-allow-headers'], /Authorization/i);
  return { chromium: 200, firefox: 200, unauthenticatedPreflight: 204 };
});

test('web origins, malformed extension origins and foreign Host rejected', async () => {
  for (const origin of ['https://example.invalid', 'null', `${chromiumOrigin}/path`,
    'chrome-extension://zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz']) {
    const response = await request({ headers: { Origin: origin } });
    assert.equal(response.status, 403);
    assert.equal(response.headers['access-control-allow-origin'], undefined);
  }
  for (const host of ['example.invalid', `localhost:${context.integration.port}`, '127.0.0.1:1']) {
    assert.equal((await request({ headers: { Host: host } })).status, 403);
  }
  assert.equal((await request({ method: 'OPTIONS', route: '/downloads', auth: false,
    headers: { Origin: 'https://example.invalid' } })).status, 403);
  return { deniedOriginCases: 5, deniedHostCases: 3 };
});

test('unknown routes and unsupported methods have no side effects', async () => {
  const before = await count();
  assert.equal((await request({ route: '/unknown' })).status, 404);
  assert.equal((await request({ method: 'DELETE', route: '/downloads' })).status, 404);
  assert.equal(await count(), before);
  return { statuses: [404, 404] };
});

test('32 KiB header limit includes final header terminator', async () => {
  const prefix = `GET /health HTTP/1.1\r\nHost: 127.0.0.1:${context.integration.port}\r\nAuthorization: Bearer ${context.integration.token}\r\nX-Padding: `;
  const total = 32769;
  const payload = prefix + 'x'.repeat(total - Buffer.byteLength(prefix) - 4) + '\r\n\r\n';
  assert.equal(Buffer.byteLength(payload), total);
  assert.equal((await rawRequest(payload)).status, 431);
  assert.equal((await request()).status, 200, 'Bridge must remain available after rejection');
  return { headerBytes: total, status: 431 };
});

test('duplicate headers rejected without selecting an authentication value', async () => {
  const response = await rawRequest(`GET /health HTTP/1.1\r\nHost: 127.0.0.1:${context.integration.port}\r\nAuthorization: Bearer ${context.integration.token}\r\nAuthorization: Bearer wrong\r\n\r\n`);
  assert.equal(response.status, 400);
  return { status: 400 };
});

test('JSON media type is exact and malformed JSON rejected', async () => {
  await rejectedPost({ url: context.fixture.url }, 415, { headers: { 'Content-Type': 'text/plain' } });
  await rejectedPost({ url: context.fixture.url }, 415, { headers: { 'Content-Type': 'application/jsonp' } });
  const before = await count();
  const malformed = await request({ method: 'POST', route: '/downloads', body: '{ definitely invalid' });
  assert.equal(malformed.status, 400);
  assert.equal(await count(), before);
  return { plainText: 415, misleadingMediaType: 415, malformedJson: 400 };
});

test('body limit, missing length and transfer encoding rejected', async () => {
  const prefix = `POST /downloads HTTP/1.1\r\nHost: 127.0.0.1:${context.integration.port}\r\nAuthorization: Bearer ${context.integration.token}\r\nContent-Type: application/json\r\n`;
  assert.equal((await rawRequest(`${prefix}Content-Length: 262145\r\n\r\n`)).status, 413);
  assert.equal((await rawRequest(`${prefix}\r\n`)).status, 413);
  assert.equal((await rawRequest(`${prefix}Transfer-Encoding: chunked\r\n\r\n0\r\n\r\n`)).status, 400);
  return { overLimit: 413, missingLength: 413, chunked: 400 };
});

test('request object and optional field types are validated before adding', async () => {
  for (const value of [null, [], 'text']) await rejectedPost(value, 400);
  const wrongTypes = { start: 'false', filename: 123, expectedSha256: 123,
    expected_sha256: 123, scheduled_at: 'tomorrow', urls: 'not-an-array', headers: [] };
  for (const [field, value] of Object.entries(wrongTypes)) {
    await rejectedPost({ url: context.fixture.url, [field]: value }, 400);
  }
  return { rejectedTopLevelTypes: 3, rejectedFieldTypes: Object.keys(wrongTypes) };
});

test('invalid URL batches rejected before the first valid URL is accepted', async () => {
  for (const invalid of ['ftp://example.invalid/file', 'file:///C:/secret',
    'https://user:password@example.invalid/file', 'not a URL', 123]) {
    await rejectedPost({ urls: [context.fixture.url, invalid], start: false }, 400);
  }
  await rejectedPost({ urls: [], start: false }, 400);
  await rejectedPost({ urls: Array(201).fill(context.fixture.url), start: false }, 400);
  return { invalidMixedBatches: 5, emptyBatch: 400, oversizedBatch: 400 };
});

test('invalid expected hashes and unsafe session headers rejected', async () => {
  await rejectedPost({ url: context.fixture.url, expectedSha256: 'short', start: false }, 400);
  for (const headers of [{ Range: 'bytes=1-2' }, { Host: 'example.invalid' },
    { Cookie: 'test=1\r\nInjected: yes' }, { 'bad header': 'value' },
    Object.fromEntries(Array.from({ length: 65 }, (_, i) => [`x-test-${i}`, 'value']))]) {
    await rejectedPost({ url: context.fixture.url, headers, start: false }, 400);
  }
  return { invalidHash: 400, rejectedHeaderCases: 5 };
});

test('valid JSON at 256 KiB body boundary accepted without starting', async () => {
  const base = { url: `${context.fixture.url}?boundary=1`, start: false, padding: '' };
  base.padding = 'x'.repeat(262144 - Buffer.byteLength(JSON.stringify(base)));
  const body = JSON.stringify(base);
  assert.equal(Buffer.byteLength(body), 262144);
  const response = await request({ method: 'POST', route: '/downloads', body,
    headers: { 'Content-Type': 'Application/JSON; charset=utf-8' } });
  assert.equal(response.status, 201);
  assert.equal(response.value.accepted, 1);
  const task = await context.client.task(response.value.ids[0]);
  assert.equal(task.status, 'paused');
  return { bodyBytes: Buffer.byteLength(body), status: 201, taskStatus: task.status };
});

test('valid paused batch preserves URLs and creates no origin requests', async () => {
  const urls = [`${context.fixture.url}?batch=one`, `${context.fixture.url}?batch=two`];
  const before = context.fixture.requests.length;
  const response = await post({ urls, start: false }, { headers: { Origin: chromiumOrigin } });
  assert.equal(response.status, 201);
  assert.equal(response.value.accepted, 2);
  assert.equal(new Set(response.value.ids).size, 2);
  for (let i = 0; i < response.value.ids.length; i++) {
    const task = await context.client.task(response.value.ids[i]);
    assert.equal(task.status, 'paused');
    assert.equal(task.url, urls[i]);
  }
  assert.equal(context.fixture.requests.length, before);
  return { accepted: 2, originRequests: 0 };
});

test('bridge-created download uses session headers and matches independent SHA-256', async () => {
  context.fixture.behavior.requireHeader = { name: 'x-qdm-test-session', value: 'synthetic-session' };
  const response = await post({ url: `${context.fixture.url}?download=1`, filename: 'ไฟล์-QDM-bridge.bin',
    headers: { 'X-QDM-Test-Session': 'synthetic-session' }, expectedSha256: context.fixture.hash() });
  assert.equal(response.status, 201);
  assert.equal(response.value.accepted, 1);
  const task = await context.client.terminal(response.value.ids[0]);
  assert.equal(task.status, 'completed', task.error || 'Bridge download did not complete');
  assert.equal(task.expected_sha256, context.fixture.hash());
  assert.ok(task.file_path);
  const stat = await fs.stat(task.file_path);
  const hash = await fileHash(task.file_path);
  assert.equal(stat.size, context.fixture.behavior.sizeBytes);
  assert.equal(hash, context.fixture.hash());
  assert.equal(task.sha256, hash);
  assert.equal(path.basename(task.file_path), 'ไฟล์-QDM-bridge.bin');
  context.completedId = task.id;
  context.completedHash = hash;
  return { bytes: stat.size, sha256: hash, sessionHeaderRequiredByOrigin: true,
    peakOriginConnections: context.fixture.metrics.peakRangeSockets, integrity: task.integrity };
});

test('200-URL maximum batch accepted completely while paused', async () => {
  const before = await count();
  const response = await post({ urls: Array.from({ length: 200 }, (_, i) => `${context.fixture.url}?max-batch=${i}`), start: false });
  assert.equal(response.status, 201);
  assert.equal(response.value.accepted, 200);
  assert.equal(new Set(response.value.ids).size, 200);
  const snapshot = await context.client.snapshot();
  assert.equal(snapshot.downloads.length, before + 200);
  const ids = new Set(response.value.ids);
  assert.ok(snapshot.downloads.filter((task) => ids.has(task.id)).every((task) => task.status === 'paused'));
  return { accepted: 200 };
});

test('Windows CLI cannot open a live profile through a case-alias path', async () => {
  if (process.platform !== 'win32') return { skipped: true, reason: 'Windows mutex/profile alias behavior requires Windows' };
  const duplicate = new CliClient(binary, context.dataDir.toUpperCase(), context.downloadDir);
  context.clients.push(duplicate);
  await assert.rejects(duplicate.snapshot(), /QDM exited|already|profile|running/i);
  const exit = await duplicate.exit;
  assert.notEqual(exit.code, 0, 'Second CLI must fail instead of touching the live state');
  assert.equal((await request()).status, 200);
  return { secondExitCode: exit.code, originalBridgeStatus: 200 };
});

test('pairing token and completed records persist across clean restart', async () => {
  const token = context.integration.token;
  const pairingPath = path.join(context.dataDir, 'browser-pairing-v2.bin');
  const before = await fs.readFile(pairingPath);
  let protection;
  if (process.platform === 'win32') {
    assert.ok(!before.includes(Buffer.from(token)), 'Pairing file must not contain the plaintext token on Windows');
    protection = 'Windows protected bytes contain no plaintext token';
  } else {
    const stat = await fs.stat(pairingPath);
    assert.equal(stat.mode & 0o777, 0o600);
    protection = 'POSIX pairing file mode 0600';
  }
  const expectedCount = await count();
  const exit = await context.client.shutdown();
  assert.equal(exit.code, 0);
  await context.start();
  assert.ok(context.integration.token === token, 'Pairing token changed unexpectedly after restart');
  assert.ok(before.equals(await fs.readFile(pairingPath)), 'Pairing file changed unnecessarily');
  assert.equal((await request()).status, 200);
  assert.equal(await count(), expectedCount);
  const completed = await context.client.task(context.completedId);
  assert.equal(completed.status, 'completed');
  assert.equal(await fileHash(completed.file_path), context.completedHash);
  return { pairingBytes: before.length, tokenPersisted: true, protection, records: expectedCount };
});

async function main() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'qdm-rust-bridge-'));
  const dataDir = path.join(root, 'state');
  const downloadDir = path.join(root, 'downloads');
  await fs.mkdir(downloadDir, { recursive: true });
  const fixture = await createHttpFixture({ sizeBytes: 2 * MiB + 37, delayMs: 1 });
  context = { root, dataDir, downloadDir, fixture, clients: [], client: null, integration: null,
    async start() {
      this.client = new CliClient(binary, dataDir, downloadDir, ['--bridge']);
      this.clients.push(this.client);
      this.integration = await this.client.command('integration');
      assert.ok(Number.isInteger(this.integration.port) && this.integration.port >= 17876 && this.integration.port <= 17885);
      assert.ok(/^[a-f0-9]{64}$/i.test(this.integration.token), 'Pairing token must be 64 hexadecimal characters');
    },
  };
  const report = { schema: 1, suite: 'QDM native browser bridge executable acceptance',
    startedAt: new Date().toISOString(), binary, binarySha256: await fileHash(binary),
    platform: process.platform, architecture: process.arch, node: process.version, os: os.release(),
    testedSubject: 'Actual QDM --cli --bridge process; independent TCP/HTTP requests and origin fixture', cases: [] };
  const save = async () => {
    if (reportPath) {
      await fs.mkdir(path.dirname(path.resolve(reportPath)), { recursive: true });
      await fs.writeFile(reportPath, `${JSON.stringify(report, null, 2)}\n`);
    }
  };
  try {
    await context.start();
    for (const item of cases) {
      const started = Date.now();
      process.stdout.write(`RUN  ${item.name}\n`);
      try {
        const detail = await item.run();
        const status = detail?.skipped ? 'skipped' : 'passed';
        report.cases.push({ name: item.name, status, elapsedMs: Date.now() - started, detail });
        process.stdout.write(`${status === 'passed' ? 'PASS' : 'SKIP'} ${item.name} (${Date.now() - started} ms)\n`);
      } catch (error) {
        // Never write the pairing token into logs or report artifacts, even if
        // an assertion or transport error happens to include a request value.
        const sanitize = (value) => String(value || '').replaceAll(context.integration?.token || '<no-token>', '<redacted>');
        report.cases.push({ name: item.name, status: 'failed', elapsedMs: Date.now() - started,
          error: sanitize(error.message), stack: sanitize(error.stack) });
        process.stdout.write(`FAIL ${item.name}: ${sanitize(error.message).slice(0, 2500)}\n`);
      }
      await save();
    }
    report.finishedAt = new Date().toISOString();
    report.passed = report.cases.filter((item) => item.status === 'passed').length;
    report.failed = report.cases.filter((item) => item.status === 'failed').length;
    report.skipped = report.cases.filter((item) => item.status === 'skipped').length;
    await save();
    process.stdout.write(`RESULT ${report.passed} passed; ${report.failed} failed; ${report.skipped} skipped\n`);
    if (report.failed) process.exitCode = 1;
  } finally {
    for (const client of context.clients) {
      if (!client.exited) { try { await client.shutdown(); } catch { await client.crash(); } }
    }
    await fixture.close();
    if (!preserve) await fs.rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
}

main().catch((error) => {
  const token = context?.integration?.token;
  const detail = String(error.stack || error);
  process.stderr.write(`${token ? detail.replaceAll(token, '<redacted>') : detail}\n`);
  process.exitCode = 1;
});
