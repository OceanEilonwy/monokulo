import { readFileSync, writeFileSync, readdirSync } from 'node:fs';
import { resolve, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import postcss from 'postcss';
import { parseFragment } from 'parse5';
import ts from 'typescript';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const baselinePath = new URL('./lint-baseline.json', import.meta.url);
const controls = new Set(['button', 'input', 'textarea', 'select']);
const visual = /^(color|background(?:-.+)?|border(?:-.+)?|font(?:-.+)?|box-shadow|padding(?:-.+)?)$/;
const literalColor = /#[\da-f]{3,8}\b|\b(?:rgb|rgba|hsl|hsla)\(/i;

// Findings retain normalized source, so editing/moving a new violation does not
// silently inherit a file-wide exemption. Counts catch copy/pasted violations.
export function inspect(file, source) {
  const findings = [];
  const report = (rule, snippet, line = 1) => findings.push({ file, rule, line, snippet: snippet.replace(/\s+/g, ' ').trim() });
  const checkControl = (tag, attrs, snippet, line) => {
    if (!controls.has(tag) || /\btype\s*=\s*["'](?:hidden|checkbox|radio)["']/.test(attrs)) return;
    // A component's own controls (the Select's button) are what the
    // library defines; everyone else uses a component or a shared class.
    if (!file.startsWith('crates/ui/src/') && !/\bmk-(?:btn|input)\b/.test(attrs)) report('shared-control', snippet, line);
  };
  if (file.endsWith('.css')) {
    if (file.endsWith('/tokens.css')) return findings;
    const ast = postcss.parse(source, { from: file });
    ast.walkDecls(decl => {
      if (literalColor.test(decl.value)) report('color-token', `${decl.prop}: ${decl.value}`, decl.source.start.line);
      const selector = decl.parent.selector || '';
      if (!file.startsWith('crates/ui/') && /\.mk-[\w-]+/.test(selector) && visual.test(decl.prop)) {
        report('component-override', `${selector} { ${decl.prop}: ${decl.value} }`, decl.source.start.line);
      }
      if (decl.prop === 'font-size' && /[\d.]+(?:px|rem|em)\b/.test(decl.value)) {
        report('type-token', `${selector} { font-size: ${decl.value} }`, decl.source.start.line);
      }
    });
  } else if (file.endsWith('.html')) {
    const visit = node => {
      const attrs = Object.fromEntries((node.attrs || []).map(attr => [attr.name, attr.value]));
      const location = node.sourceCodeLocation;
      const snippet = location?.startTag ? source.slice(location.startTag.startOffset, location.startTag.endOffset) : '';
      if (node.tagName) {
        checkControl(node.tagName, snippet, snippet, location?.startLine);
        if ('data-icon-only' in attrs && !attrs['aria-label'] && !attrs['aria-labelledby']) report('icon-name', snippet, location?.startLine);
        if (attrs.class?.split(/\s+/).includes('mk-btn') && node.tagName === 'a' && !attrs.href) report('link-destination', snippet, location?.startLine);
        // Every dropdown is `ui::Select`, rendered from Rust: hand-written
        // HTML includes its rendering rather than writing a select.
        if (node.tagName === 'select') report('native-select-wrapper', snippet, location?.startLine);
      }
      (node.childNodes || []).forEach(visit);
    };
    visit(parseFragment(source, { sourceCodeLocationInfo: true }));
  } else if (/\.[jt]sx?$/.test(file)) {
    const ast = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
    const visit = node => {
      if (ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node)) {
        checkControl(node.tagName.getText(ast), node.attributes.getText(ast), node.getText(ast), ast.getLineAndCharacterOfPosition(node.getStart(ast)).line + 1);
      }
      if (!file.startsWith('crates/ui/') && ts.isCallExpression(node)
        && ts.isPropertyAccessExpression(node.expression) && node.expression.name.text === 'createElement'
        && node.arguments[0] && ts.isStringLiteral(node.arguments[0]) && controls.has(node.arguments[0].text)) {
        report('js-native-control', node.getText(ast), ast.getLineAndCharacterOfPosition(node.getStart(ast)).line + 1);
      }
      ts.forEachChild(node, visit);
    };
    visit(ast);
  } else if (file.endsWith('.rs')) {
    // Maud has no HTML AST: scan native control declarations conservatively.
    // Runtime contract tests cover the actual rendered DOM (labels, IDs, ARIA).
    for (const match of source.matchAll(/^\s*(button|input|textarea|select)\b([^;{]*)(?:;|\{)/gm)) {
      checkControl(match[1], match[2], match[0], source.slice(0, match.index).split('\n').length);
    }
  }
  return findings;
}
export function key(finding) {
  return `${finding.file}:${finding.rule}:${createHash('sha256').update(finding.snippet).digest('hex').slice(0, 16)}`;
}
export function unaccepted(findings, baseline) {
  const counts = new Map();
  return findings.filter(finding => {
    const id = key(finding);
    const count = (counts.get(id) || 0) + 1;
    counts.set(id, count);
    return count > (baseline[id]?.count || 0);
  });
}
/** How many findings the baseline accepts that are no longer there. */
export function obsolete(findings, baseline) {
  const counts = new Map();
  for (const finding of findings) counts.set(key(finding), (counts.get(key(finding)) || 0) + 1);
  return Object.entries(baseline).reduce((sum, [id, { count }]) => sum + Math.max(0, count - (counts.get(id) || 0)), 0);
}
function files(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap(entry => {
    // `generated`: what `cargo xtask ui` renders from crates/ui.
    if (['node_modules', 'dist', 'generated', 'fixi.js', 'ssexi.js', 'jsQR.js'].includes(entry.name)) return [];
    const path = resolve(dir, entry.name);
    return entry.isDirectory() ? files(path) : /\.(css|html|[jt]sx?|rs)$/.test(entry.name) ? [path] : [];
  });
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const roots = ['crates/ui/web', 'crates/ui/src', 'apps/stylebook', 'crates/monokulo/pos-ui/src', 'crates/monokulo/src/views', 'crates/monokulo/static'];
  const findings = roots.flatMap(dir => files(resolve(root, dir))).flatMap(path => inspect(relative(root, path), readFileSync(path, 'utf8')));
  const record = list => {
    const baseline = {};
    for (const finding of list) {
      const id = key(finding);
      baseline[id] ??= { count: 0, snippet: finding.snippet };
      baseline[id].count++;
    }
    writeFileSync(baselinePath, JSON.stringify(baseline, null, 2) + '\n');
  };
  if (process.argv.includes('--write-baseline')) {
    record(findings);
    console.log(`Recorded ${findings.length} existing findings. Review baseline changes; never refresh automatically in CI.`);
  } else {
    const baseline = JSON.parse(readFileSync(baselinePath, 'utf8'));
    const added = unaccepted(findings, baseline);
    const accepted = findings.filter(finding => !added.includes(finding));
    const stale = obsolete(findings, baseline);
    if (process.argv.includes('--prune-baseline')) {
      // Only ever removes: what's fixed leaves the baseline, nothing joins it.
      record(accepted);
      console.log(`Pruned ${stale} fixed findings; ${accepted.length} remain.`);
    } else {
      for (const finding of added) console.error(`${finding.file}:${finding.line} [${finding.rule}] ${finding.snippet}`);
      if (stale) console.error(`${stale} baseline findings are fixed: run \`node crates/ui/tools/lint.mjs --prune-baseline\` to drop them.`);
      console.log(`${added.length} new violations; ${accepted.length} known legacy findings. Runtime contracts are checked by test:ui.`);
      process.exitCode = added.length || stale ? 1 : 0;
    }
  }
}
