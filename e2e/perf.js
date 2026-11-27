// Where the page spends its time on a big vault: load milestones, then each start-up step timed again.
//   fastobsidian BIGVAULT --serve --port 8811   (in another terminal)
//   cd e2e && PW_CHANNEL=msedge node perf.js [url]
const { chromium } = require('@playwright/test');

(async () => {
  const url = process.argv[2] || 'http://127.0.0.1:8811/';
  const browser = await chromium.launch({ channel: process.env.PW_CHANNEL || undefined });
  const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
  const t0 = Date.now();
  await page.goto(url, { waitUntil: 'load' });
  await page.waitForFunction(() => document.getElementById('vname').textContent.length > 0);
  const wall = Date.now() - t0;
  const r = await page.evaluate(async () => {
    const nav = performance.getEntriesByType('navigation')[0];
    const time = f => { const t = performance.now(); f(); return Math.round(performance.now() - t); };
    const embedded = document.getElementById('data').textContent.trim();
    const raw = embedded || await fetch('/api/data').then(r => r.text());
    return {
      responseEnd: Math.round(nav.responseEnd),
      domContentLoaded: Math.round(nav.domContentLoadedEventEnd),
      load: Math.round(nav.loadEventEnd),
      dataMB: (raw.length / 1e6).toFixed(1),
      jsonParse: time(() => JSON.parse(raw)),
      ingest: typeof ingest === 'function' ? time(() => ingest(JSON.parse(raw))) : null,
      refreshSide: typeof refreshSide === 'function' ? time(() => refreshSide()) : null,
      renderIndex: typeof renderIndex === 'function' ? time(() => renderIndex()) : null,
      domNodes: document.getElementsByTagName('*').length,
    };
  });
  console.log({ wallMs: wall, ...r });
  await browser.close();
})();
