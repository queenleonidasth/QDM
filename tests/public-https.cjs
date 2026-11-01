#!/usr/bin/env node
'use strict';

// Real-network smoke tests of the actual Rust executable. The reference uses
// Node's HTTPS implementation independently of QDM/reqwest/native TLS.
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs/promises');
const https = require('node:https');
const os = require('node:os');
const path = require('node:path');
const { CliClient, delay, fileHash } = require('./helpers/cli-client.cjs');

const MAX_BYTES = 5 * 1024 * 1024;
const REFERENCE_DEADLINE_MS = 30000;
const REQUEST_TIMEOUT_MS = 15000;
const QDM_DEADLINE_MS = 45000;
const MAX_REDIRECTS = 5;
const SOURCES = [
  {
    name: 'Electron v45.0.0 license',
    url: 'https://github.com/electron/electron/raw/refs/tags/v45.0.0/LICENSE',
    filename: 'electron-v45.0.0-LICENSE.txt',
  },
  {
    name: 'RFC 9110 — HTTP Semantics',
    url: 'https://www.rfc-editor.org/rfc/rfc9110.txt',
    filename: 'rfc9110.txt',
  },
];

function argument(name, fallback) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : fallback;
}
const binary = path.resolve(argument('--binary', process.env.QDM_BINARY || path.join(__dirname, '..', 'target', 'release', process.platform === 'win32' ? 'QDM.exe' : 'QDM')));
const reportPath = path.resolve(argument('--report', path.join(__dirname, '..', 'reports', 'public-https.json')));
const hash = (bytes) => crypto.createHash('sha256').update(bytes).digest('hex');
const errorInfo = (error) => ({
  name: error?.name || 'Error',
  code: error?.code || error?.cause?.code || null,
  message: String(error?.message || error).slice(0, 2000),
});

function checkedUrl(value) {
  const url = new URL(value);
  assert.equal(url.protocol, 'https:', 'Public smoke tests require HTTPS at every redirect');
  assert.equal(url.username, '', 'URL credentials are not allowed');
  assert.equal(url.password, '', 'URL credentials are not allowed');
  return url;
}

function referenceRequest(url, timeoutMs) {
  return new Promise((resolve, reject) => {
    const request = https.get(url, {
      // Keep both certificate-chain and hostname verification enabled.
      rejectUnauthorized: true,
      signal: AbortSignal.timeout(timeoutMs),
      headers: { 'Accept-Encoding': 'identity', 'User-Agent': 'QDM-Public-QA/0.2' },
    }, (response) => {
      const socket = response.socket;
      const record = {
        url: url.href,
        status: response.statusCode,
        contentLength: response.headers['content-length'] || null,
        contentEncoding: response.headers['content-encoding'] || null,
        etag: response.headers.etag || null,
        location: response.headers.location || null,
        tls: {
          authorized: socket.authorized === true,
          authorizationError: socket.authorizationError || null,
          protocol: socket.getProtocol?.() || null,
          cipher: socket.getCipher?.()?.name || null,
        },
      };
      if (!record.tls.authorized) {
        response.destroy();
        reject(new Error('The independent reference did not have an authorized TLS connection'));
        return;
      }
      if ([301, 302, 303, 307, 308].includes(response.statusCode)) {
        response.destroy();
        resolve({ record, redirect: response.headers.location });
        return;
      }
      if (response.statusCode !== 200) {
        response.destroy();
        const error = new Error(`Independent HTTPS reference returned HTTP ${response.statusCode}`);
        error.response = record;
        reject(error);
        return;
      }
      if (record.contentEncoding && record.contentEncoding.toLowerCase() !== 'identity') {
        response.destroy();
        reject(new Error(`Reference ignored identity encoding: ${record.contentEncoding}`));
        return;
      }
      if (record.contentLength !== null && (!/^\d+$/.test(record.contentLength) || Number(record.contentLength) > MAX_BYTES)) {
        response.destroy();
        reject(new Error('Independent reference exceeds the 5 MiB cap or has an invalid length'));
        return;
      }
      const chunks = [];
      let size = 0;
      response.on('data', (chunk) => {
        size += chunk.length;
        if (size > MAX_BYTES) {
          response.destroy(new Error('Independent reference body exceeded the 5 MiB cap'));
          return;
        }
        chunks.push(chunk);
      });
      response.once('aborted', () => reject(new Error('Independent reference was truncated')));
      response.once('error', reject);
      response.once('end', () => {
        if (!response.complete) { reject(new Error('Independent reference did not complete')); return; }
        if (record.contentLength !== null && Number(record.contentLength) !== size) {
          reject(new Error('Reference length differed from Content-Length'));
          return;
        }
        resolve({ record, body: Buffer.concat(chunks, size) });
      });
    });
    request.once('error', reject);
  });
}

async function reference(source) {
  const deadline = Date.now() + REFERENCE_DEADLINE_MS;
  const hops = [];
  let url = checkedUrl(source.url);
  for (let hop = 0; hop <= MAX_REDIRECTS; hop++) {
    const remaining = deadline - Date.now();
    if (remaining <= 0) throw new Error('Independent reference exceeded its 30-second deadline');
    const result = await referenceRequest(url, Math.min(remaining, REQUEST_TIMEOUT_MS));
    hops.push(result.record);
    if ([301, 302, 303, 307, 308].includes(result.record.status)) {
      if (!result.redirect) throw new Error('HTTPS redirect omitted Location');
      if (hop === MAX_REDIRECTS) throw new Error('Independent reference exceeded five redirects');
      url = checkedUrl(new URL(result.redirect, url).href);
      continue;
    }
    return {
      body: result.body,
      evidence: {
        bytes: result.body.length,
        sha256: hash(result.body),
        effectiveUrl: url.href,
        redirects: hops.length - 1,
        hops,
      },
    };
  }
  throw new Error('Unexpected reference redirect loop');
}

async function boundedCompletion(client, id, expectedBytes) {
  const deadline = Date.now() + QDM_DEADLINE_MS;
  let last;
  while (Date.now() < deadline) {
    const snapshot = await client.command('snapshot', {}, Math.max(1, Math.min(1500, deadline - Date.now())));
    last = snapshot.downloads.find((task) => task.id === id);
    if (!last) throw new Error('The real QDM executable lost the test task');
    if (last.downloaded_bytes > MAX_BYTES || last.network_bytes > MAX_BYTES || (last.total_bytes !== null && last.total_bytes > MAX_BYTES)) {
      throw new Error('QDM reported a representation exceeding the 5 MiB test cap');
    }
    if (last.downloaded_bytes > expectedBytes) throw new Error('QDM received more file data than the bounded independent reference');
    if (last.total_bytes !== null && last.total_bytes !== expectedBytes) {
      throw new Error('The QDM representation length differs from the independent reference');
    }
    if (['error', 'failed'].includes(last.status)) throw new Error(last.error || `QDM ended as ${last.status}`);
    if (last.status === 'completed') return last;
    await delay(25);
  }
  throw new Error(`QDM exceeded its 45-second deadline; last status: ${last?.status || 'unknown'}`);
}

async function main() {
  const started = Date.now();
  const report = {
    schema: 1,
    suite: 'QDM Rust executable public HTTPS smoke',
    startedAt: new Date(started).toISOString(),
    binary,
    binarySha256: null,
    platform: process.platform,
    architecture: process.arch,
    node: process.version,
    limits: {
      maximumSourceAndOutputBytes: MAX_BYTES,
      referenceDeadlineMs: REFERENCE_DEADLINE_MS,
      requestTimeoutMs: REQUEST_TIMEOUT_MS,
      maximumRedirects: MAX_REDIRECTS,
      qdmDeadlinePerSourceMs: QDM_DEADLINE_MS,
    },
    tlsVerification: {
      reference: 'Node HTTPS certificate and hostname verification enabled; authorized socket recorded for every response.',
      qdm: 'Unmodified real Rust executable using its normal reqwest/native-TLS certificate verification. CLI does not expose per-socket TLS telemetry.',
    },
    cases: [],
    cleanup: { temporaryDirectoryRemoved: false },
    passed: 0,
    failed: 0,
    limitations: [
      'A correctness smoke test for these two public resources at this time; not a throughput or universal website-compatibility benchmark.',
      'The 5 MiB limit is the representation/output cap, not a claim that redirects, overlap requests, TLS overhead, and in-flight buffers consume no extra network bytes.',
      'Production QDM profiles, existing user downloads, IDM, and browsers are not used by this script.',
    ],
  };
  let root;
  try {
    assert.notEqual(process.env.NODE_TLS_REJECT_UNAUTHORIZED, '0', 'TLS verification must not be disabled in the test environment');
    report.binarySha256 = await fileHash(binary);
    root = await fs.mkdtemp(path.join(os.tmpdir(), 'qdm-rust-public-https-'));
    for (const [index, source] of SOURCES.entries()) {
      const result = { name: source.name, url: source.url, status: 'failed' };
      const sourceStarted = Date.now();
      let client;
      let id;
      try {
        const baseline = await reference(source);
        result.reference = baseline.evidence;
        const work = path.join(root, String(index));
        client = new CliClient(binary, path.join(work, 'state'), path.join(work, 'downloads'));
        // These tiny references are correctness checks. Limiting consumption
        // also bounds the data arriving between watchdog snapshots.
        await client.settings({ connections: 4, max_concurrent: 1, retries: 0, speed_limit_kib: 1024 });
        id = await client.add({ url: source.url, filename: source.filename, expected_sha256: baseline.evidence.sha256 });
        const task = await boundedCompletion(client, id, baseline.evidence.bytes);
        assert.ok(task.file_path, 'QDM did not return its final file path');
        const stat = await fs.stat(task.file_path);
        assert.ok(stat.size <= MAX_BYTES, 'Published output exceeds the 5 MiB cap');
        assert.equal(stat.size, baseline.evidence.bytes, 'Independent reference and output byte counts differ');
        const output = await fs.readFile(task.file_path);
        const actual = hash(output);
        assert.equal(actual, baseline.evidence.sha256, 'Independent reference and output SHA-256 differ');
        assert.equal(task.sha256, actual, 'QDM-reported SHA-256 differs from the independently read file');
        assert.ok(output.equals(baseline.body), 'Exact output bytes differ from the independent reference');
        assert.equal(task.integrity, 'sha256_verified', 'QDM did not enforce the supplied expected SHA-256');
        result.qdm = {
          status: task.status,
          bytes: output.length,
          sha256: actual,
          reportedSha256: task.sha256,
          expectedSha256: task.expected_sha256,
          exactBytesMatched: true,
          peakConnections: task.peak_connections,
          dynamicSplits: task.dynamic_splits,
          networkBytes: task.network_bytes,
          supportsRanges: task.supports_ranges,
          resumable: task.resumable,
          integrity: task.integrity,
          notice: task.notice,
        };
        result.status = 'passed';
      } catch (error) {
        result.error = errorInfo(error);
        if (error.response) result.response = error.response;
        if (client && id && !client.exited) {
          try { await client.command('pause', { task_id: id }, 3000); }
          catch { await client.crash(); }
        }
      } finally {
        if (client && !client.exited) {
          try { await client.shutdown(); }
          catch (error) {
            result.cleanupError = errorInfo(error);
            result.status = 'failed';
            await client.crash();
          }
        }
        if (client?.stderr) result.stderr = client.stderr.slice(-4000);
        result.elapsedMs = Date.now() - sourceStarted;
        report.cases.push(result);
        console.log(`${result.status === 'passed' ? 'PASS' : 'FAIL'} ${source.name} (${result.elapsedMs} ms)${result.error ? `: ${result.error.message}` : ''}`);
      }
    }
  } catch (error) {
    report.fatal = errorInfo(error);
  } finally {
    if (root) {
      try {
        await fs.rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
        report.cleanup.temporaryDirectoryRemoved = true;
      } catch (error) { report.cleanup.error = errorInfo(error); }
    }
    report.passed = report.cases.filter((result) => result.status === 'passed').length;
    report.failed = SOURCES.length - report.passed;
    report.ok = report.failed === 0 && !report.fatal && report.cleanup.temporaryDirectoryRemoved;
    report.finishedAt = new Date().toISOString();
    report.elapsedMs = Date.now() - started;
    await fs.mkdir(path.dirname(reportPath), { recursive: true });
    await fs.writeFile(reportPath, `${JSON.stringify(report, null, 2)}\n`, 'utf8');
  }
  console.log(`RESULT ${report.passed}/${SOURCES.length} passed; report: ${reportPath}`);
  if (!report.ok) process.exitCode = 1;
}

main().catch((error) => { console.error(JSON.stringify({ fatal: errorInfo(error) })); process.exitCode = 1; });
