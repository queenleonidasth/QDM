'use strict';

const { spawn } = require('node:child_process');
const readline = require('node:readline');
const fs = require('node:fs');
const { createHash } = require('node:crypto');

const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function waitFor(read, description, timeoutMs = 30000, intervalMs = 35) {
  const expires = Date.now() + timeoutMs;
  let last;
  while (Date.now() < expires) {
    last = await read();
    if (last) return last;
    await delay(intervalMs);
  }
  throw new Error(`Timed out after ${timeoutMs} ms: ${description}`);
}

class CliClient {
  constructor(binary, dataDir, downloadDir, extraArgs = []) {
    this.binary = binary;
    this.pending = new Map();
    this.nextId = 0;
    this.stderr = '';
    this.exited = false;
    this.child = spawn(binary, ['--cli', '--data-dir', dataDir, '--download-dir', downloadDir, ...extraArgs], {
      stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true,
      env: { ...process.env, QDM_TEST_MODE: '1' },
    });
    this.exit = new Promise((resolve) => {
      this.child.once('error', (error) => {
        this.exited = true;
        this.rejectAll(error);
        resolve({ error: error.message });
      });
      this.child.once('exit', (code, signal) => {
        this.exited = true;
        this.rejectAll(new Error(`QDM exited (${code ?? signal}). ${this.stderr.slice(-5000)}`));
        resolve({ code, signal });
      });
    });
    this.child.stderr.on('data', (bytes) => { this.stderr = (this.stderr + bytes.toString('utf8')).slice(-20000); });
    this.lines = readline.createInterface({ input: this.child.stdout });
    this.lines.on('line', (line) => {
      let reply;
      try { reply = JSON.parse(line); }
      catch { this.rejectAll(new Error(`Non-JSON CLI stdout: ${line.slice(0, 1000)}`)); return; }
      const pending = this.pending.get(reply.id);
      if (!pending) return;
      clearTimeout(pending.timer);
      this.pending.delete(reply.id);
      if (reply.ok) pending.resolve(reply.result);
      else pending.reject(new Error(reply.error || 'QDM command failed without an error'));
    });
    this.child.stdin.on('error', (error) => this.rejectAll(error));
  }
  rejectAll(error) {
    for (const pending of this.pending.values()) { clearTimeout(pending.timer); pending.reject(error); }
    this.pending.clear();
  }
  command(op, fields = {}, timeoutMs = 20000) {
    if (this.exited) return Promise.reject(new Error(`QDM process already exited. ${this.stderr}`));
    const id = ++this.nextId;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`CLI ${op} did not reply in ${timeoutMs} ms. ${this.stderr.slice(-5000)}`));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      this.child.stdin.write(`${JSON.stringify({ id, op, ...fields })}\n`);
    });
  }
  snapshot() { return this.command('snapshot'); }
  async settings(changes) {
    const current = await this.snapshot();
    return this.command('settings', { settings: { ...current.settings, ...changes } });
  }
  add(request) { return this.command('add', { request: { start: true, ...request } }); }
  limits(id, queue, speed_limit_kib) { return this.command('download_limits', { task_id: id, queue, speed_limit_kib }); }
  pause(id) { return this.command('pause', { task_id: id }); }
  resume(id) { return this.command('resume', { task_id: id }); }
  replaceUrl(id, url) { return this.command('replace_url', { task_id: id, url }); }
  async task(id) { return (await this.snapshot()).downloads.find((task) => task.id === id); }
  async terminal(id, timeoutMs = 45000) {
    let seen;
    try {
      return await waitFor(async () => {
        seen = await this.task(id);
        return seen && ['completed', 'error', 'failed'].includes(seen.status) ? seen : null;
      }, `download ${id} completion`, timeoutMs);
    } catch (error) {
      error.message += `\nLast task: ${JSON.stringify(seen)}\n${this.stderr.slice(-5000)}`;
      throw error;
    }
  }
  async shutdown() {
    if (this.exited) return this.exit;
    try { await this.command('shutdown', {}, 10000); } catch (error) { if (!this.exited) throw error; }
    let timer;
    const exited = await Promise.race([this.exit, new Promise((resolve) => { timer = setTimeout(() => resolve(null), 12000); })]);
    clearTimeout(timer);
    if (!exited && !this.exited) { this.child.kill('SIGKILL'); await this.exit; throw new Error('QDM did not exit after shutdown'); }
    return exited;
  }
  async crash() {
    if (!this.exited) this.child.kill('SIGKILL');
    return this.exit;
  }
}

async function fileHash(filePath) {
  const hash = createHash('sha256');
  for await (const chunk of fs.createReadStream(filePath)) hash.update(chunk);
  return hash.digest('hex');
}

module.exports = { CliClient, waitFor, delay, fileHash };
