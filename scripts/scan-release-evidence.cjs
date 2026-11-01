#!/usr/bin/env node
'use strict';

// Read-only scan of this project's docs/evidence tree. Never prints matched
// values and never edits, removes, renames or redacts source evidence.
const fs = require('node:fs/promises');
const path = require('node:path');

const TEXT = new Set(['.json', '.jsonl', '.ndjson', '.txt', '.log', '.md', '.csv', '.tsv', '.yaml', '.yml', '.html', '.xml']);
const IMAGE = new Set(['.png', '.jpg', '.jpeg', '.webp', '.bmp', '.gif', '.tif', '.tiff']);
const VIDEO = new Set(['.mp4', '.webm', '.mov', '.mkv', '.avi']);
const TOKEN_KEY = /^(?:(?:pairing|bridge)[_-]?)?token$|^authorization$/i;
const HEX_TOKEN = /^[a-f0-9]{64}$/i;
const MAX_TEXT_BYTES = 32 * 1024 * 1024;

function safePointerPart(key) {
  // A JSON key can itself contain sensitive text. Replace only embedded token
  // shapes so the diagnostic retains location without exposing its value.
  return String(key).replace(/[a-f0-9]{64}/ig, '[64hex-key-redacted]').replace(/~/g, '~0').replace(/\//g, '~1');
}

function inspectText(contents, extension) {
  const findings = [];
  const seen = new Set();
  const add = (kind, locator) => {
    const id = `${kind}:${JSON.stringify(locator)}`;
    if (!seen.has(id)) { seen.add(id); findings.push({ kind, ...locator }); }
  };
  const patterns = [
    ['authorization_bearer_literal', /\bBearer\s+[a-f0-9]{64}\b/ig],
    ['token_field_literal', /["'](?:(?:pairing|bridge)[_-]?)?token["']\s*:\s*["'][a-f0-9]{64}["']/ig],
    ['url_token_literal', /[?&](?:token|pairing_token|bridge_token)=[a-f0-9]{64}\b/ig],
    ['plain_token_literal', /\b(?:pairing[ _-]+token|bridge[ _-]+token)\s*[:=]\s*[a-f0-9]{64}\b/ig],
  ];
  for (const [kind, pattern] of patterns) {
    for (const match of contents.matchAll(pattern)) {
      add(kind, { line: contents.slice(0, match.index).split('\n').length });
    }
  }

  let jsonParseStatus = extension === '.json' ? 'not_parsed' : 'not_json';
  if (extension === '.json') {
    try {
      const value = JSON.parse(contents.replace(/^\uFEFF/, ''));
      jsonParseStatus = 'parsed';
      const walk = (node, pointer) => {
        if (!node || typeof node !== 'object') return;
        const tokenLabel = Object.entries(node).some(([key, val]) =>
          /^(name|label|title|automationId|helpText)$/i.test(key) && typeof val === 'string' && /\b(?:pairing|bridge)?\s*token\b/i.test(val));
        for (const [key, val] of Object.entries(node)) {
          const next = `${pointer}/${safePointerPart(key)}`;
          if (typeof val === 'string') {
            const trimmed = val.trim();
            if (TOKEN_KEY.test(key) && (HEX_TOKEN.test(trimmed) || /^Bearer\s+[a-f0-9]{64}$/i.test(trimmed))) {
              add('credential_field_requires_exclusion_or_redaction', { jsonPointer: next });
            } else if (tokenLabel && HEX_TOKEN.test(trimmed)) {
              add('token_labelled_ui_value_requires_review', { jsonPointer: next });
            } else if (/^(name|value|text|accessibleName|accessibleValue)$/i.test(key) && HEX_TOKEN.test(trimmed)) {
              add('unlabelled_64hex_visible_text_may_be_token_or_file_hash', { jsonPointer: next });
            }
          } else if (val && typeof val === 'object') {
            walk(val, next);
          }
        }
      };
      walk(value, '');
    } catch (_) {
      // A malformed/partial report is still scanned as text. Never include a
      // parser exception, which can contain an excerpt of sensitive input.
      jsonParseStatus = 'parse_failed_text_scan_only';
    }
  }
  return { findings, jsonParseStatus };
}

async function scanEvidenceTree(evidenceRoot) {
  const report = {
    schema: 1,
    scope: 'Exact QDM native/docs/evidence tree; no browser profiles or pairing files read.',
    startedAt: new Date().toISOString(),
    behavior: 'Read-only evidence scan. Only the separately requested scan report is written. Source evidence is unchanged.',
    textFiles: [], mediaFiles: [], skippedPaths: [],
    limits: [
      'Pattern matches are review candidates, not proof that a credential is live.',
      'Unlabelled 64-hex UI text may be a file hash. Matched values are never emitted.',
      'Image and video pixels are not inspected. Review Browser connection panes and related UIA dumps separately.',
      'No pattern match does not rule out encoded, unlabelled or differently formatted credentials.',
      'Scan the exact final exported bundle again if reports or media are subsequently changed.',
    ],
  };
  const visit = async (directory) => {
    const entries = await fs.readdir(directory, { withFileTypes: true });
    entries.sort((a, b) => a.name.localeCompare(b.name));
    for (const entry of entries) {
      const absolute = path.join(directory, entry.name);
      const relative = path.relative(evidenceRoot, absolute).split(path.sep).join('/');
      if (entry.isSymbolicLink()) { report.skippedPaths.push({ path: relative, reason: 'symlink_not_followed' }); continue; }
      if (entry.isDirectory()) { await visit(absolute); continue; }
      if (!entry.isFile()) continue;
      const extension = path.extname(entry.name).toLowerCase();
      if (IMAGE.has(extension) || VIDEO.has(extension)) {
        report.mediaFiles.push({ path: relative, mediaType: IMAGE.has(extension) ? 'image' : 'video', pixelInspection: 'PENDING_MANUAL_REVIEW', filenameRisk: /browser|pair|integration|extension|options/i.test(entry.name) ? 'browser_or_pairing_related_filename' : 'filename_alone_cannot_rule_out_token' });
        continue;
      }
      if (!TEXT.has(extension)) continue;
      try {
        const stat = await fs.stat(absolute);
        if (stat.size > MAX_TEXT_BYTES) { report.skippedPaths.push({ path: relative, reason: 'text_larger_than_32MiB_requires_separate_review' }); continue; }
        const contents = await fs.readFile(absolute, 'utf8');
        report.textFiles.push({ path: relative, ...inspectText(contents, extension) });
      } catch (error) {
        report.skippedPaths.push({ path: relative, reason: 'read_error', code: error.code || 'UNKNOWN' });
      }
    }
  };
  await visit(evidenceRoot);
  report.finishedAt = new Date().toISOString();
  report.summary = {
    scannedTextFiles: report.textFiles.length,
    filesWithReviewCandidates: report.textFiles.filter(x => x.findings.length).length,
    mediaRequiringPixelReview: report.mediaFiles.length,
    skippedPaths: report.skippedPaths.length,
    status: 'SCAN_COMPLETE_FINAL_DISTRIBUTION_REVIEW_STILL_REQUIRED',
  };
  return report;
}

async function main() {
  const args = process.argv.slice(2);
  if (args.some((arg, i) => !(i === 0 && arg === '--report') && !(i === 1 && args[0] === '--report')) || args.length === 1 || args.length > 2) {
    throw new Error('Usage: node scripts/scan-release-evidence.cjs [--report docs/release-artifact-privacy-scan-0.3.0.json]');
  }
  const nativeRoot = path.resolve(__dirname, '..');
  const evidenceRoot = path.join(nativeRoot, 'docs', 'evidence');
  const output = args.length ? path.resolve(nativeRoot, args[1]) : path.join(nativeRoot, 'docs', 'release-artifact-privacy-scan-0.3.0.json');
  const insideEvidence = path.relative(evidenceRoot, output);
  if (insideEvidence === '' || (!insideEvidence.startsWith(`..${path.sep}`) && insideEvidence !== '..' && !path.isAbsolute(insideEvidence))) {
    throw new Error('Write the scan report outside docs/evidence to preserve the original evidence tree.');
  }
  const report = await scanEvidenceTree(evidenceRoot);
  await fs.mkdir(path.dirname(output), { recursive: true });
  await fs.writeFile(output, JSON.stringify(report, null, 2) + '\n', 'utf8');
  console.log(JSON.stringify({ reportPath: output, ...report.summary }));
  for (const item of report.textFiles.filter(x => x.findings.length)) console.log(JSON.stringify({ path: item.path, findings: item.findings }));
  for (const item of report.mediaFiles) console.log(JSON.stringify(item));
}

module.exports = { inspectText, scanEvidenceTree };
if (require.main === module) main().catch(error => { console.error(error.message); process.exitCode = 1; });
