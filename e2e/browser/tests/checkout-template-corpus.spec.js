// What crates/checkout-template writes, read by a real browser's parser: it must
// come back exactly as written. A browser that builds anything else (moving a
// block out of a paragraph, closing something early) is what sanitisers are
// attacked through, and the crate's strict rules exist to rule that out. Needs
// no server: the file's answers are checked against the crate by its own test,
// `the_browser_corpus_is_what_this_crate_writes`.
const fs = require('node:fs');
const path = require('node:path');
const { test, expect } = require('../coverage-test');

const corpus = JSON.parse(fs.readFileSync(path.join(__dirname, '../fixtures/template-corpus.json'), 'utf8'));

test('a browser parses every written template to the tree it was written from', async ({ page }) => {
  await page.setContent('<!doctype html><div id="root"></div>');
  const read = await page.evaluate((writtens) => writtens.map((written) => {
    const root = document.getElementById('root');
    root.innerHTML = written;
    return root.innerHTML;
  }), corpus.map((entry) => entry.html));
  expect(read).toEqual(corpus.map((entry) => entry.html));
});

// What the crate writes for CSS, read by the browser: every rule must come back
// inside `#checkout-root`, going down from it, with nothing nested in a rule.
const cssCorpus = JSON.parse(fs.readFileSync(path.join(__dirname, '../fixtures/css-corpus.json'), 'utf8'));

test('a browser reads every written stylesheet as rules that stay inside the page', async ({ page }) => {
  await page.setContent('<!doctype html><body></body>');
  const problems = await page.evaluate((sheets) => {
    const found = [];
    const walk = (rules, css) => {
      for (const rule of rules) {
        if (rule instanceof CSSStyleRule) {
          if (rule.cssRules.length) found.push(`${css}: a rule nested in ${rule.selectorText}`);
          // Commas inside a selector's own brackets don't separate selectors.
          let depth = 0, start = 0;
          const pieces = [];
          for (let i = 0; i < rule.selectorText.length; i++) {
            const c = rule.selectorText[i];
            if (c === '(' || c === '[') depth++;
            if (c === ')' || c === ']') depth--;
            if (c === ',' && depth === 0) { pieces.push(rule.selectorText.slice(start, i)); start = i + 1; }
          }
          pieces.push(rule.selectorText.slice(start));
          for (const piece of pieces) {
            if (!/^#checkout-root [^~+>|]/.test(piece.trim())) found.push(`${css}: selector ${piece}`);
          }
        } else if (rule instanceof CSSMediaRule || rule instanceof CSSSupportsRule || rule instanceof CSSContainerRule) {
          walk(rule.cssRules, css);
        } else if (!(rule instanceof CSSKeyframesRule)) {
          found.push(`${css}: a ${rule.constructor.name}`);
        }
      }
    };
    for (const { css, written } of sheets) {
      const sheet = new CSSStyleSheet();
      sheet.replaceSync(written);
      walk(sheet.cssRules, css);
    }
    return found;
  }, cssCorpus);
  expect(problems).toEqual([]);
});

