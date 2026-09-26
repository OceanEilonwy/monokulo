const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

const output = process.env.COVERAGE_OUTPUT;
const enabled = process.env.COVERAGE_SCREENSHOTS === '1' && output;
const gallery = output && path.join(output, '..', 'screenshots');
const images = gallery && path.join(gallery, 'images');
const escapeHtml = value => String(value).replace(/&/g, '&amp;').replace(/</g, '&lt;')
  .replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#39;');

class CoverageGalleryReporter {
  constructor() { this.entries = []; }
  onBegin() { if (enabled) fs.mkdirSync(images, { recursive: true }); }
  onTestEnd(test, result) {
    if (!enabled) return;
    const sourceFile = path.basename(test.location.file);
    const group = sourceFile.includes('challenge') ? 'challenge'
      : sourceFile.includes('pos') || sourceFile.includes('fit') ? 'pos' : 'checkout';
    let sequence = 0;
    for (const attachment of result.attachments) {
      const stage = attachment.name.startsWith('coverage-stage:')
        ? attachment.name.slice('coverage-stage:'.length)
        : attachment.name === 'screenshot' ? 'failure' : null;
      if (!stage || attachment.contentType !== 'image/png') continue;
      const id = crypto.createHash('sha256').update(`${test.id}:${result.retry}:${result.workerIndex}:${sequence}:${stage}`)
        .digest('hex').slice(0, 16);
      const filename = `${group}-${stage}-r${result.retry}-${id}.png`;
      fs.writeFileSync(path.join(images, filename), attachment.body || fs.readFileSync(attachment.path));
      this.entries.push({ group, test: test.title, test_id: test.id, retry: result.retry,
        worker: result.workerIndex, stage, sequence, status: result.status, image: `images/${filename}` });
      sequence++;
    }
  }
  onEnd(result) {
    if (!enabled) return result;
    this.entries.sort((a, b) => a.group.localeCompare(b.group) || a.test.localeCompare(b.test)
      || a.retry - b.retry || a.sequence - b.sequence);
    fs.writeFileSync(path.join(gallery, 'manifest.json'), JSON.stringify(this.entries, null, 2));
    const groups = new Set(this.entries.filter(e => e.stage !== 'failure').map(e => e.group));
    let html = '<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Monokulo UI stages</title><style>body{font:16px/1.5 system-ui,sans-serif;max-width:1100px;margin:2rem auto;padding:0 1rem;color:#17212b}a{color:#164e8a}.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(240px,1fr));gap:1rem}.card{border:1px solid #ccd3db;border-radius:8px;padding:.7rem}.card img{width:100%;height:180px;object-fit:contain;background:#eee}.card p{margin:.3rem 0}small{color:#52606d}</style><h1>UI stages</h1><p><a href="../index.html">Coverage summary</a> · <a href="../browser/playwright-report/index.html">Playwright test report</a></p>';
    for (const group of ['checkout', 'pos', 'challenge']) {
      html += `<h2>${group}</h2><div class="grid">`;
      for (const entry of this.entries.filter(e => e.group === group)) {
        html += `<article class="card"><a href="${escapeHtml(entry.image)}"><img src="${escapeHtml(entry.image)}" alt="${escapeHtml(entry.stage)}"></a><p><strong>${escapeHtml(entry.stage)}</strong></p><p>${escapeHtml(entry.test)}</p><p><a href="../browser/playwright-report/index.html#?testId=${encodeURIComponent(entry.test_id)}">Test result</a></p><small>Retry ${entry.retry}; ${escapeHtml(entry.status)}</small></article>`;
      }
      html += '</div>';
    }
    html += '</html>';
    fs.writeFileSync(path.join(gallery, 'index.html'), html);
    if (!['checkout', 'pos', 'challenge'].every(group => groups.has(group))) {
      console.error(`coverage screenshots missing required stage groups: ${['checkout', 'pos', 'challenge'].filter(g => !groups.has(g)).join(', ')}`);
      return { status: 'failed' };
    }
    return result;
  }
}

module.exports = CoverageGalleryReporter;
