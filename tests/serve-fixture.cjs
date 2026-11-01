#!/usr/bin/env node
'use strict';

const fs = require('node:fs/promises');
const path = require('node:path');
const readline = require('node:readline');
const { createHttpFixture, MiB, KiB } = require('./helpers/http-fixture.cjs');

function option(name, fallback) {
  const index = process.argv.indexOf(name);
  return index < 0 ? fallback : process.argv[index + 1];
}

(async () => {
  const fixture = await createHttpFixture({
    sizeBytes: Math.round(Number(option('--size-mib', 64)) * MiB),
    delayMs: Number(option('--delay-ms', 150)),
    chunkSize: Math.round(Number(option('--chunk-kib', 16)) * KiB),
    validators: option('--validators', 'etag'),
  });
  const infoPath = option('--info', null);
  const reportPath = option('--report', null);
  const info = { pid: process.pid, url: fixture.url, filename: 'fixture.bin', sizeBytes: fixture.behavior.sizeBytes, sha256: fixture.hash() };
  if (infoPath) {
    await fs.mkdir(path.dirname(path.resolve(infoPath)), { recursive: true });
    await fs.writeFile(infoPath, `${JSON.stringify(info, null, 2)}\n`);
  }
  process.stdout.write(`${JSON.stringify({ ready: true, ...info })}\n`);
  let stopping = false;
  const lines = readline.createInterface({ input: process.stdin });
  async function stop() {
    if (stopping) return;
    stopping = true;
    lines.close();
    await fixture.close();
    if (reportPath) await fs.writeFile(reportPath, `${JSON.stringify({ ...info, metrics: fixture.metrics, requests: fixture.requests }, null, 2)}\n`);
  }
  lines.on('line', (line) => {
    try {
      const request = JSON.parse(line);
      if (request.op === 'behavior') {
        for (const key of ['delayMs', 'version', 'denied']) if (Object.hasOwn(request.values || {}, key)) fixture.behavior[key] = request.values[key];
        process.stdout.write(`${JSON.stringify({ ok: true })}\n`);
      } else if (request.op === 'snapshot') process.stdout.write(`${JSON.stringify({ metrics: fixture.metrics })}\n`);
      else if (request.op === 'shutdown') stop().catch((error) => { process.stderr.write(`${error.stack}\n`); process.exitCode = 1; });
    } catch (error) { process.stdout.write(`${JSON.stringify({ ok: false, error: error.message })}\n`); }
  });
  process.on('SIGINT', () => stop().catch(() => {}));
  process.on('SIGTERM', () => stop().catch(() => {}));
})().catch((error) => { process.stderr.write(`${error.stack}\n`); process.exitCode = 1; });
