'use strict';

// This server deliberately has no dependency on QDM's implementation. Every
// request, socket and emitted byte is observed from the origin side.
const http = require('node:http');
const { createHash } = require('node:crypto');
const { gzipSync } = require('node:zlib');

const KiB = 1024;
const MiB = 1024 * KiB;
const sha256 = (value) => createHash('sha256').update(value).digest('hex');

function payload(size, version = 1) {
  const bytes = Buffer.allocUnsafe(size);
  // Avalanche the absolute word position. Unlike a repeated block, this also
  // exposes a whole-segment offset error at a power-of-two boundary.
  for (let offset = 0; offset < size; offset += 4) {
    let word = ((offset >>> 2) ^ Math.imul(version, 0x9e3779b1)) >>> 0;
    word = Math.imul(word ^ (word >>> 16), 0x7feb352d) >>> 0;
    word = Math.imul(word ^ (word >>> 15), 0x846ca68b) >>> 0;
    word = (word ^ (word >>> 16)) >>> 0;
    for (let byte = 0; byte < 4 && offset + byte < size; byte++) {
      bytes[offset + byte] = (word >>> (byte * 8)) & 255;
    }
  }
  return bytes;
}

async function createHttpFixture(initial = {}) {
  const behavior = {
    sizeBytes: 8 * MiB + 37,
    version: 1,
    validators: 'etag',
    rangeMode: 'honor',
    chunkSize: 64 * KiB,
    delayMs: 1,
    sampleDelayMs: 0,
    barrierTarget: 0,
    barrierTimeoutMs: 6000,
    slowFirstRange: false,
    slowDelayMs: 35,
    truncateCount: 0,
    permanentTruncate: false,
    failures: [],
    retryAfterSeconds: 1,
    denied: false,
    unknownLength: false,
    requireHeader: null,
    contentDisposition: 'attachment; filename="fixture.bin"',
    ...initial,
  };
  const versions = new Map();
  const compressedVersions = new Map();
  const getPayload = (version = behavior.version) => {
    if (!versions.has(version)) versions.set(version, payload(behavior.sizeBytes, version));
    return versions.get(version);
  };
  // Materialize before accepting requests so server-side generation is not
  // confused with the downloader's latency or CPU use.
  getPayload();
  const getWirePayload = (version) => {
    if (behavior.contentEncoding !== 'gzip') return getPayload(version);
    if (!compressedVersions.has(version)) compressedVersions.set(version, gzipSync(getPayload(version)));
    return compressedVersions.get(version);
  };
  getWirePayload(behavior.version);
  const requests = [];
  const sockets = new Set();
  const timers = new Set();
  const activeRanges = new Map();
  const bodyRanges = new Map();
  const barrier = [];
  let nextSocket = 0;
  let nextRequest = 0;
  let barrierReleased = !behavior.barrierTarget;
  let barrierTimer;
  let firstSlowRequest;
  let closing = false;
  const metrics = {
    peakRangeRequests: 0,
    peakRangeSockets: 0,
    peakBodySockets: 0,
    barrierReached: false,
    bodyBytes: 0,
    completedRequests: 0,
    abortedRequests: 0,
  };

  function later(fn, delay) {
    const timer = setTimeout(() => { timers.delete(timer); fn(); }, delay);
    timers.add(timer);
    return timer;
  }
  function updatePeak() {
    metrics.peakRangeRequests = Math.max(metrics.peakRangeRequests, activeRanges.size);
    metrics.peakRangeSockets = Math.max(metrics.peakRangeSockets, new Set([...activeRanges.values()].map((r) => r.socketId)).size);
    metrics.peakBodySockets = Math.max(metrics.peakBodySockets, new Set([...bodyRanges.values()].map((r) => r.socketId)).size);
  }
  function releaseBarrier(reached) {
    if (barrierReleased) return;
    barrierReleased = true;
    metrics.barrierReached = reached;
    if (barrierTimer) { clearTimeout(barrierTimer); timers.delete(barrierTimer); }
    for (const start of barrier.splice(0)) start();
  }

  const server = http.createServer((req, res) => {
    const match = /^bytes=(\d+)-(\d*)$/.exec(req.headers.range || '');
    const version = behavior.mixedVersions && match && Number(match[1]) >= behavior.sizeBytes / 2 ? 2 : behavior.version;
    const bytes = getWirePayload(version);
    const etag = `${behavior.validators === 'weak' ? 'W/' : ''}"qdm-fixture-v${version}"`;
    const modified = version === 1 ? 'Sun, 25 Oct 2026 08:00:00 GMT' : 'Mon, 26 Oct 2026 08:00:00 GMT';
    const requestedStart = match ? Number(match[1]) : 0;
    const requestedEnd = match && match[2] ? Number(match[2]) : Math.max(0, bytes.length - 1);
    const requestedLength = match ? requestedEnd - requestedStart + 1 : bytes.length;
    const smallProbe = Boolean(match && requestedLength <= 64 * KiB);
    const record = {
      id: ++nextRequest, socketId: req.socket.qdmFixtureSocketId,
      at: Date.now(), method: req.method, url: req.url,
      range: req.headers.range || null, ifRange: req.headers['if-range'] || null,
      requestedStart, requestedEnd, requestedLength, version,
      start: 0, end: Math.max(-1, bytes.length - 1), status: 200,
      sentBytes: 0, bodyStartedAt: null, endedAt: null, aborted: false,
      tailOfUnfinished: [],
    };
    // Adjacent 32 KiB integrity overlaps do not count as dynamic splitting.
    // A newly requested interior range must cover >128 KiB of an unfinished
    // prior transfer. Cancellation followed by repartitioning is allowed.
    if (match && requestedLength > 128 * KiB) {
      record.tailOfUnfinished = requests.filter((prior) =>
        prior.status === 206 && prior.requestedLength > 256 * KiB &&
        requestedStart > prior.start + 128 * KiB && requestedEnd <= prior.end &&
        requestedEnd - requestedStart >= 128 * KiB &&
        prior.sentBytes < prior.end - prior.start + 1 - 128 * KiB &&
        (prior.endedAt === null || (prior.aborted && record.at - prior.endedAt < 2000))
      ).map((prior) => ({ requestId: prior.id, wasActive: prior.endedAt === null, sentBytes: prior.sentBytes, start: prior.start, end: prior.end }));
    }
    requests.push(record);
    let sendTimer;
    let finished = false;
    const endRecord = (aborted) => {
      if (record.endedAt !== null) return;
      record.endedAt = Date.now();
      record.aborted = aborted;
      activeRanges.delete(record.id);
      bodyRanges.delete(record.id);
      if (sendTimer) { clearTimeout(sendTimer); timers.delete(sendTimer); }
      if (aborted) metrics.abortedRequests++;
      else metrics.completedRequests++;
    };
    res.on('finish', () => { finished = true; endRecord(false); });
    res.on('close', () => endRecord(!finished));
    const fail = (status, extra = {}) => {
      record.status = status;
      res.writeHead(status, { 'Content-Length': 0, ...extra });
      res.end();
    };
    if (behavior.denied || (behavior.requireHeader && req.headers[behavior.requireHeader.name.toLowerCase()] !== behavior.requireHeader.value)) {
      fail(403); return;
    }
    if (req.method !== 'HEAD' && !smallProbe && behavior.failures.length) {
      fail(behavior.failures.shift(), { 'Retry-After': String(behavior.retryAfterSeconds) }); return;
    }

    const headers = {
      'Content-Type': 'application/octet-stream',
      'Content-Disposition': behavior.contentDisposition,
      'Accept-Ranges': 'bytes',
      Date: 'Mon, 26 Oct 2026 12:00:00 GMT',
      'Cache-Control': 'no-store',
    };
    if (behavior.contentEncoding) headers['Content-Encoding'] = behavior.contentEncoding;
    if (behavior.validators === 'etag' || behavior.validators === 'weak') headers.ETag = etag;
    if (behavior.validators === 'last-modified' || behavior.validators === 'weak') headers['Last-Modified'] = modified;
    const ifRangeMatches = !req.headers['if-range'] || req.headers['if-range'] === headers.ETag || req.headers['if-range'] === headers['Last-Modified'];
    const honorsRange = !behavior.unknownLength && behavior.rangeMode !== 'ignore' &&
      !(behavior.rangeMode === 'ignore-after-probe' && !smallProbe);
    if (match && honorsRange && ifRangeMatches) {
      if (bytes.length === 0 || requestedStart >= bytes.length || requestedEnd < requestedStart) {
        fail(416, { ...headers, 'Content-Range': `bytes */${bytes.length}` }); return;
      }
      record.start = requestedStart;
      record.end = Math.min(requestedEnd, bytes.length - 1);
      record.status = 206;
      const declaredStart = behavior.rangeMode === 'malformed' && !smallProbe ? record.start + 1 : record.start;
      headers['Content-Range'] = `bytes ${declaredStart}-${record.end}/${bytes.length}`;
    }
    const bodyLength = Math.max(0, record.end - record.start + 1);
    if (!behavior.unknownLength) headers['Content-Length'] = bodyLength;
    res.writeHead(record.status, headers);
    if (req.method === 'HEAD' || bodyLength === 0) { res.end(); return; }
    const largeRange = record.status === 206 && bodyLength > 128 * KiB;
    if (largeRange) { activeRanges.set(record.id, record); updatePeak(); }
    const truncate = !smallProbe && (behavior.permanentTruncate || behavior.truncateCount > 0);
    if (truncate && behavior.truncateCount > 0) behavior.truncateCount--;
    const limit = truncate ? record.start + Math.max(1, Math.floor(bodyLength / 2)) : record.end + 1;
    if (behavior.slowFirstRange && largeRange && firstSlowRequest === undefined) firstSlowRequest = record.id;
    const delay = smallProbe ? behavior.sampleDelayMs : record.id === firstSlowRequest ? behavior.slowDelayMs : behavior.delayMs;
    let position = record.start;
    const writeNext = () => {
      if (closing || res.destroyed || res.writableEnded) return;
      const next = Math.min(limit, position + behavior.chunkSize);
      const chunk = bytes.subarray(position, next);
      if (chunk.length) {
        if (record.bodyStartedAt === null) {
          record.bodyStartedAt = Date.now();
          if (largeRange) { bodyRanges.set(record.id, record); updatePeak(); }
        }
        record.sentBytes += chunk.length;
        metrics.bodyBytes += chunk.length;
        position = next;
        const accepted = res.write(chunk);
        if (!accepted && position < limit) { res.once('drain', () => { sendTimer = later(writeNext, delay); }); return; }
      }
      if (position >= limit) {
        if (truncate) sendTimer = later(() => res.destroy(), 5);
        else res.end();
      } else sendTimer = later(writeNext, delay);
    };
    res.flushHeaders();
    if (largeRange && !barrierReleased) {
      // A real downloader may open more connections only after valid body
      // progress. Supply enough prefix for a32KiB integrity overlap plus
      // actual owned payload, then hold the remainder until the requested
      // distinct sockets overlap. This still proves simultaneous body
      // transfers without requiring an unbounded initial SYN burst.
      const prefixEnd = Math.min(limit, position + 64 * KiB);
      const prefix = bytes.subarray(position, prefixEnd);
      if (prefix.length) {
        record.bodyStartedAt = Date.now();
        bodyRanges.set(record.id, record); updatePeak();
        record.sentBytes += prefix.length; metrics.bodyBytes += prefix.length;
        position = prefixEnd;
        res.write(prefix);
      }
      barrier.push(writeNext);
      if (!barrierTimer) barrierTimer = later(() => releaseBarrier(false), behavior.barrierTimeoutMs);
      if (new Set([...activeRanges.values()].map((r) => r.socketId)).size >= behavior.barrierTarget) releaseBarrier(true);
    } else writeNext();
  });
  server.on('connection', (socket) => {
    socket.qdmFixtureSocketId = ++nextSocket;
    sockets.add(socket);
    socket.on('close', () => sockets.delete(socket));
  });
  server.on('clientError', (_error, socket) => socket.destroy());
  server.keepAliveTimeout = 2000;
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const url = `http://127.0.0.1:${server.address().port}/fixture.bin`;
  return {
    url, behavior, requests, metrics,
    hash: (version = behavior.version) => sha256(getPayload(version)),
    getPayload,
    bytesSince: (requestIndex) => requests.slice(requestIndex).reduce((sum, req) => sum + req.sentBytes, 0),
    async close() {
      closing = true;
      for (const timer of timers) clearTimeout(timer);
      timers.clear();
      for (const socket of sockets) socket.destroy();
      await new Promise((resolve) => server.close(resolve));
      versions.clear();
      compressedVersions.clear();
    },
  };
}

module.exports = { createHttpFixture, payload, sha256, KiB, MiB };

