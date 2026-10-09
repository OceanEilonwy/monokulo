const base = require('@playwright/test');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

async function installCoverageContext(context) {
  if (!process.env.COVERAGE_RAW_DIR) return;
  await context.addInitScript(() => {
    window.addEventListener('pagehide', () => {
      if (!window.__coverage__) return;
      try {
        const key = '__monokulo_coverage_snapshots__';
        const prior = JSON.parse(sessionStorage.getItem(key) || '[]');
        prior.push(window.__coverage__);
        sessionStorage.setItem(key, JSON.stringify(prior.slice(-20)));
      } catch (_) { /* A blocked storage API must not break the app. */ }
    });
  });
}

async function collectCoverageContext(context, testInfo) {
  if (!process.env.COVERAGE_RAW_DIR) return;
  const snapshots = [];
  for (const page of context.pages()) {
    for (const frame of page.frames()) {
      try {
        const captured = await frame.evaluate(() => {
          let previous = [];
          try { previous = JSON.parse(sessionStorage.getItem('__monokulo_coverage_snapshots__') || '[]'); }
          catch (_) { /* A browser may deny storage to a cross-site frame. */ }
          return [...previous, window.__coverage__ || {}];
        });
        snapshots.push(...captured.filter(Boolean));
      } catch (_) { /* A frame may detach during test teardown. */ }
    }
  }
  const directory = process.env.COVERAGE_RAW_DIR;
  fs.mkdirSync(directory, { recursive: true });
  const id = crypto.createHash('sha256').update(`${testInfo.testId}:${testInfo.retry}:${testInfo.workerIndex}`).digest('hex').slice(0, 24);
  const file = path.join(directory, `${id}.json`);
  const previous = fs.existsSync(file) ? JSON.parse(fs.readFileSync(file)) : { snapshots: [] };
  fs.writeFileSync(file, JSON.stringify({
    title: testInfo.title, id: testInfo.testId, retry: testInfo.retry,
    status: testInfo.status || 'passed', snapshots: [...previous.snapshots, ...snapshots],
  }));
}

// Debug builds of monokulo embed Solid's development build, which reports
// reactivity mistakes (a read that will not update, a write from inside a
// computation, ...) to the console as `[CODE] message`. Any of those fails
// the test that produced it.
const SOLID_DIAGNOSTIC = /^\[[A-Z][A-Z_]+\] /;

const test = base.test.extend({
  context: async ({ context }, use, testInfo) => {
    const diagnostics = [];
    context.on('console', message => {
      if (['warning', 'error'].includes(message.type()) && SOLID_DIAGNOSTIC.test(message.text())) diagnostics.push(message.text());
    });
    await installCoverageContext(context);
    await use(context);
    await collectCoverageContext(context, testInfo);
    if (diagnostics.length) throw new Error(`Solid reported ${diagnostics.length} diagnostic(s):\n${diagnostics.join('\n')}`);
  },
});

/**
 * Stops the page's time at `time`, before the page is opened: its timers
 * fire and its Date moves only when the test moves them (page.clock.runFor).
 * The clock is installed a minute earlier and paused at `time`: it flows
 * between the two calls, and pausing at a moment it has already passed
 * fails ("Cannot fast-forward to the past"), which installing and pausing
 * at the same moment does under load.
 */
async function pauseClockAt(page, time) {
  await page.clock.install({ time: new Date(time.getTime() - 60_000) });
  await page.clock.pauseAt(time);
}

/**
 * Records the page's EventSource streams, for a library (the embed, the
 * POS) that exposes none of its own: each one's URL, its readyState as it
 * is now, the error events it has handled (`errors`: a dropped stream the
 * browser will retry) and those with the stream closed for good
 * (`refused`). Install before the page is opened; read with
 * `eventSources(page)`.
 *
 * Safe: the record is a subclass that only adds a listener in its
 * constructor; the same arguments reach the browser's EventSource and
 * the page gets the same object. Exact: a listener added in the
 * constructor runs before any the page adds, in the same dispatch, so
 * once the test sees an error counted the page has handled it too, and
 * set whatever timer it sets on one.
 */
async function recordEventSources(page) {
  await page.addInitScript(() => {
    const Native = window.EventSource;
    const records = [];
    window.__eventSources = records;
    window.EventSource = class extends Native {
      constructor(...args) {
        super(...args);
        const record = { url: String(args[0]), source: this, errors: 0, refused: 0 };
        records.push(record);
        this.addEventListener('error', () => {
          record.errors++;
          if (this.readyState === Native.CLOSED) record.refused++;
        });
      }
    };
  });
}

/** The streams recordEventSources saw, in the order they were opened: `{ url, readyState, errors, refused }`. */
const eventSources = page => page.evaluate(() => window.__eventSources.map(({ url, source, errors, refused }) => ({ url, readyState: source.readyState, errors, refused })));

/** `EventSource.CLOSED`: over for good, by the server's refusal or the page's close(). */
const EVENT_SOURCE_CLOSED = 2;

/**
 * Records the page's fetch() calls, for a library that exposes nothing of
 * its own: each one's URL, and whether its response promise has settled.
 * Install before the page is opened; read with `fetches(page)`.
 *
 * Safe: fetch is called with the same arguments and the page gets the very
 * promise it returns; fetch is not among what the fake clock replaces.
 * Exact: `settled` is set by a continuation registered before the page's,
 * so it runs first in the same microtask checkpoint as the page's own
 * handling of the answer: a test reading it, in a later task, sees it only
 * once that handling (and any timer it sets) is done. Handling that reads
 * the body (`response.json()`) finishes in a later task; wait on what that
 * changes instead.
 */
async function recordFetches(page) {
  await page.addInitScript(() => {
    const records = [];
    window.__fetches = records;
    const fetch_ = window.fetch;
    window.fetch = function (resource, ...rest) {
      const record = { url: String(resource instanceof Request ? resource.url : resource), settled: false };
      records.push(record);
      const response = fetch_.call(this, resource, ...rest);
      const settle = () => { record.settled = true; };
      response.then(settle, settle);
      return response;
    };
  });
}

/** The calls recordFetches saw, in order: `{ url, settled }`. */
const fetches = page => page.evaluate(() => window.__fetches.map(({ url, settled }) => ({ url, settled })));

module.exports = {
  ...base, test, installCoverageContext, collectCoverageContext, pauseClockAt,
  recordEventSources, eventSources, EVENT_SOURCE_CLOSED, recordFetches, fetches,
};
