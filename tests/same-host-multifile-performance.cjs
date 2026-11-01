#!/usr/bin/env node
'use strict';
// Localhost-only multi-file throughput probe; isolated state and verified file hashes.
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const assert = require('node:assert/strict');
const { CliClient, fileHash } = require('./helpers/cli-client.cjs');
const { createHttpFixture, MiB } = require('./helpers/http-fixture.cjs');
const arg = (name, fallback) => { const i = process.argv.indexOf(name); return i < 0 ? fallback : process.argv[i + 1]; };
const binary = path.resolve(arg('--binary', 'target/release/QDM.exe'));
const reportPath = arg('--report', null);
const fileSize = Number(arg('--size-mib', '128')) * MiB + 137;
const lanes = Number(arg('--connections', '32'));
const concurrency = Number(arg('--concurrent', '2'));
if (!(fileSize > 0 && fileSize <= 512 * MiB && lanes >= 1 && lanes <= 64 && concurrency >= 1 && concurrency <= 4)) {
  throw new Error('Invalid benchmark limits');
}
(async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'qdm-multi-perf-'));
  const origin = await createHttpFixture({ sizeBytes: fileSize, chunkSize: 64 * 1024, delayMs: 0 });
  const report = { suite: 'QDM isolated same-host multi-file end-to-end benchmark', binary, sha256: await fileHash(binary),
    fileSize, connectionsPerFile: lanes, downloadsAtOnce: concurrency, results: [], startedAt: new Date().toISOString() };
  async function run(count) {
    const dir = path.join(root, 'mode-' + count);
    const data = path.join(dir, 'state'), outputs = path.join(dir, 'downloads');
    await fs.mkdir(outputs, { recursive: true });
    const client = new CliClient(binary, data, outputs);
    try {
      await client.snapshot();
      await client.settings({ connections: lanes, max_concurrent: concurrency, retries: 2, speed_limit_kib: 0 });
      const started = Date.now();
      const ids = await Promise.all(Array.from({ length: count }, async (_, index) =>
        client.add({ url: origin.url + '?case=' + count + '-' + index, filename: 'file-' + index + '.bin' })));
      const completed = await Promise.all(ids.map(id => client.terminal(id, 120000)));
      const elapsedMs = Date.now() - started;
      const expected = origin.hash();
      for (const task of completed) {
        assert.equal(task.status, 'completed', task.error || task.status);
        assert.equal(await fileHash(task.file_path), expected);
      }
      return { count, elapsedMs, endToEndMiBps: Math.round(count * fileSize / MiB / (elapsedMs / 1000) * 100) / 100,
        peakConnections: completed.map(x => x.peak_connections),
        dynamicSplits: completed.map(x => x.dynamic_splits), verified: true };
    } finally { try { await client.shutdown(); } catch { await client.crash(); } }
  }
  try {
    report.results.push(await run(1));
    report.results.push(await run(2));
    report.status = 'pass';
  } catch (e) { report.status = 'fail'; report.error = String(e.stack || e); process.exitCode = 1; }
  finally {
    await origin.close();
    await fs.rm(root, { force: true, recursive: true, maxRetries: 6, retryDelay: 100 });
    report.finishedAt = new Date().toISOString();
    report.sha256After = await fileHash(binary);
    if (report.sha256 !== report.sha256After) { report.status = 'fail'; report.error = 'Binary changed during benchmark'; process.exitCode = 1; }
    if (reportPath) { await fs.writeFile(reportPath, JSON.stringify(report, null, 2)); }
    console.log(JSON.stringify(report, null, 2));
  }
})().catch(e => { console.error(e.stack || e); process.exitCode = 1; });