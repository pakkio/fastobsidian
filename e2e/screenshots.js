// Regenerates the README images in docs/: screenshots of the sample vault and the frames of the editor GIF.
//
//   cargo build --release && cd e2e && PW_CHANNEL=msedge node screenshots.js
//   ffmpeg (see the end of this file) turns the frames into docs/editor.gif
const { chromium } = require('@playwright/test');
const { spawn } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const root = path.resolve(__dirname, '..');
const docs = path.join(root, 'docs');
const frames = path.join(os.tmpdir(), 'fo-gif-frames');
const exe = path.join(root, 'target', 'release', process.platform === 'win32' ? 'fastobsidian.exe' : 'fastobsidian');
const port = 8791;
const sleep = ms => new Promise(r => setTimeout(r, ms));

(async () => {
  const vault = fs.mkdtempSync(path.join(os.tmpdir(), 'fo-shots-'));
  const named = path.join(vault, 'My Vault');
  fs.cpSync(path.join(root, 'sample-vault'), named, { recursive: true });
  fs.mkdirSync(docs, { recursive: true });
  fs.rmSync(frames, { recursive: true, force: true });
  fs.mkdirSync(frames, { recursive: true });
  const server = spawn(exe, [named, '--serve', '--port', String(port), '--no-cache'], { stdio: 'ignore' });
  try {
    await sleep(800);
    const browser = await chromium.launch({ channel: process.env.PW_CHANNEL || undefined });
    const page = await browser.newPage({ viewport: { width: 1280, height: 760 }, colorScheme: 'dark', deviceScaleFactor: 1 });
    const base = `http://127.0.0.1:${port}/`;

    await page.goto(base);
    await sleep(600);
    await page.screenshot({ path: path.join(docs, 'index.png') });

    await page.goto(base + '#n/Ideas/Zettelkasten');
    await sleep(500);
    await page.screenshot({ path: path.join(docs, 'note.png') });

    await page.goto(base + '#index');
    await page.click('nav button[data-v="graph"]');
    await sleep(2500);   // let the layout settle
    await page.screenshot({ path: path.join(docs, 'graph.png') });

    // the editor GIF: open Roadmap, add tasks with Enter, tick one, show the rendered result
    let n = 0;
    const shot = async (repeat = 1) => {
      const buf = await page.screenshot();
      for (let i = 0; i < repeat; i++) fs.writeFileSync(path.join(frames, `f${String(n++).padStart(4, '0')}.png`), buf);
    };
    await page.goto(base + '#e/Projects/Roadmap');
    await sleep(700);
    await shot(8);
    const last = page.locator('#live .lv-b', { hasText: 'Publish ideas' });
    await last.click({ position: { x: 400, y: 10 } });
    await sleep(150);
    await page.keyboard.press('End');
    await shot(6);
    await page.keyboard.press('Enter');
    await sleep(120);
    await shot(4);
    for (const ch of 'Ship the live editor') { await page.keyboard.type(ch); await shot(1); }
    await page.keyboard.press('Enter');
    await sleep(120);
    await shot(3);
    for (const ch of 'Paste images with **Ctrl+V**') { await page.keyboard.type(ch); await shot(1); }
    await page.keyboard.press('Escape');
    await sleep(200);
    await shot(10);
    await page.locator('#live b.m').nth(2).click();   // tick "Canvas / embeds support"
    await sleep(200);
    await shot(14);
    await browser.close();
  } finally {
    server.kill();
  }
  console.log(`screenshots in ${docs}, GIF frames in ${frames}`);
  console.log(`ffmpeg -y -framerate 12 -i "${path.join(frames, 'f%04d.png')}" -vf "scale=960:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=96[p];[b][p]paletteuse=dither=bayer:bayer_scale=4" "${path.join(docs, 'editor.gif')}"`);
})();
