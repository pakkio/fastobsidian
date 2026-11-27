// Mermaid diagrams, LaTeX math (rendering + live editor preview) and Slides presentation mode.
const { test, expect } = require('@playwright/test');

const api = (request, method, url, data) =>
  request.fetch(url, { method, headers: { 'X-Explorer': '1', 'Content-Type': 'application/json' }, data });

async function createNote(request, id, raw) {
  const r = await api(request, 'POST', '/api/note', { id, raw });
  expect(r.ok(), await r.text()).toBeTruthy();
}

test('Mermaid diagrams render as SVG across diagram types and fall back safely', async ({ page, request }) => {
  const md = [
    '# Diagrams',
    '```mermaid',
    'flowchart LR',
    '  A[Start] -->|ok| B{Check}',
    '  B -->|yes| C((Done))',
    '```',
    '```mermaid',
    'sequenceDiagram',
    '  participant web-app as Web App',
    '  participant api-srv as API',
    '  web-app->>api-srv: GET /notes',
    '  api-srv-->>web-app: 200 OK',
    '```',
    '```mermaid',
    'stateDiagram-v2',
    '  [*] --> Idle',
    '  Idle --> Busy : start',
    '  Busy --> [*]',
    '```',
    '```mermaid',
    'pie showData',
    '  title Time',
    '  "Code" : 70',
    '  "Docs" : 30',
    '```',
    '```mermaid',
    'classDiagram',
    '  class Animal {',
    '    +String name',
    '    +speak()',
    '  }',
    '  Dog --|> Animal',
    '```',
    '```mermaid',
    'mindmap',
    '  root((Vault))',
    '    Notes',
    '    Graph',
    '```',
    '```mermaid',
    'unsupportedDiagramType',
    '  foo bar',
    '```',
  ].join('\n');
  await createNote(request, 'E2E/mermaid', md);
  await page.goto('/#n/' + encodeURIComponent('E2E/mermaid'));
  const svgs = page.locator('#noteBody .mmd svg');
  await expect(svgs).toHaveCount(6);
  for (let i = 0; i < 6; i++) {
    const box = await svgs.nth(i).boundingBox();
    expect(box.width).toBeGreaterThan(80);
    expect(box.height).toBeGreaterThan(40);
  }
  await expect(page.locator('#noteBody .mmd').nth(0)).toContainText('Start');
  await expect(page.locator('#noteBody .mmd').nth(1)).toContainText('GET /notes');
  await expect(page.locator('#noteBody .mmd').nth(3)).toContainText('70%');
  // unsupported diagram falls back cleanly to a code block
  await expect(page.locator('#noteBody pre code')).toContainText('unsupportedDiagramType');
});

test('Mermaid live editor previews changes as you type and inserts from toolbar', async ({ page, request }) => {
  await createNote(request, 'E2E/mmd-edit', '```mermaid\nflowchart LR\n  A[Alpha] --> B[Beta]\n```');
  await page.goto('/#e/' + encodeURIComponent('E2E/mmd-edit'));
  await expect(page.locator('#live .mmd svg')).toHaveCount(1);
  // click rendered diagram to edit it
  await page.locator('#live .mmd').click();
  await expect(page.locator('#live .lv-ta')).toBeVisible();
  await expect(page.locator('#live .lv-pv .mmd svg')).toBeVisible();
  await expect(page.locator('#live .lv-pv')).toContainText('Alpha');
  // edit diagram text and verify live preview updates immediately
  await page.locator('#live .lv-ta').fill('```mermaid\nflowchart LR\n  A[Alpha] --> C[Gamma]\n```');
  await expect(page.locator('#live .lv-pv')).toContainText('Gamma');
  await page.keyboard.press('Escape');
  await expect(page.locator('#live .mmd')).toContainText('Gamma');

  // toolbar button inserts a working mermaid diagram into a blank note
  await createNote(request, 'E2E/mmd-tb', '');
  await page.goto('/#e/' + encodeURIComponent('E2E/mmd-tb'));
  await expect(page.locator('#live .lv-ta')).toBeVisible();
  await page.locator('#tb button', { hasText: 'mermaid' }).click();
  await expect(page.locator('#live .lv-pv .mmd svg')).toBeVisible();
  await expect(page.locator('#live .lv-pv')).toContainText('Start');
});

test('LaTeX renders inline and display MathML and updates live in the editor', async ({ page, request }) => {
  const md = [
    'Inline $E = mc^2$ and $\\alpha_1 + \\beta_2$, price $5 and $10, code `$not_math$`.',
    '',
    '$$',
    'x = \\frac{-b \\pm \\sqrt{b^2 - 4ac}}{2a}',
    '$$',
    '',
    '$$',
    '\\begin{pmatrix} 1 & 2 \\\\ 3 & 4 \\end{pmatrix}',
    '$$',
  ].join('\n');
  await createNote(request, 'E2E/latex', md);
  await page.goto('/#n/' + encodeURIComponent('E2E/latex'));
  await expect(page.locator('#noteBody math')).toHaveCount(4);
  await expect(page.locator('#noteBody .math-blk mfrac')).toHaveCount(1);
  await expect(page.locator('#noteBody .math-blk msqrt')).toHaveCount(1);
  await expect(page.locator('#noteBody .math-blk mtable')).toHaveCount(1);
  await expect(page.locator('#noteBody')).toContainText('price $5 and $10');
  await expect(page.locator('#noteBody code')).toHaveText('$not_math$');

  // open in live editor: clicking display math edits the whole $$ block with a live MathML preview
  await page.goto('/#e/' + encodeURIComponent('E2E/latex'));
  await expect(page.locator('#live .math-blk math')).toHaveCount(2);
  await page.locator('#live .math-blk').first().click();
  await expect(page.locator('#live .lv-ta')).toBeVisible();
  await expect(page.locator('#live .lv-pv math mfrac')).toHaveCount(1);
  await page.locator('#live .lv-ta').fill('$$\n\\sum_{i=1}^{n} i = \\frac{n(n+1)}{2}\n$$');
  await expect(page.locator('#live .lv-pv math munderover')).toHaveCount(1);
  await page.keyboard.press('Escape');
  await expect(page.locator('#live .math-blk munderover')).toHaveCount(1);
});

test('Slides presentation splits on ---, renders Mermaid and LaTeX, and navigates by keyboard', async ({ page, request }) => {
  const deck = [
    '# Welcome',
    'First slide with $a^2 + b^2 = c^2$',
    '',
    '---',
    '',
    '## Architecture',
    '```mermaid',
    'flowchart LR',
    '  UI --> Server',
    '```',
    '',
    '---',
    '',
    '## Done',
    'See [[Home]]',
  ].join('\n');
  await createNote(request, 'E2E/slides', deck);
  await page.goto('/#n/' + encodeURIComponent('E2E/slides'));
  await page.click('#bSlides');
  await expect(page.locator('#slides')).toHaveClass(/open/);
  await expect(page.locator('#slCnt')).toHaveText('1 / 3');
  await expect(page.locator('#slCard h1')).toHaveText('Welcome');
  await expect(page.locator('#slCard math')).toHaveCount(1);

  await page.keyboard.press('ArrowRight');
  await expect(page.locator('#slCnt')).toHaveText('2 / 3');
  await expect(page.locator('#slCard .mmd svg')).toHaveCount(1);

  await page.keyboard.press('Space');
  await expect(page.locator('#slCnt')).toHaveText('3 / 3');
  await expect(page.locator('#slCard h2')).toHaveText('Done');

  await page.keyboard.press('Home');
  await expect(page.locator('#slCnt')).toHaveText('1 / 3');

  await page.keyboard.press('Escape');
  await expect(page.locator('#slides')).not.toHaveClass(/open/);
});
