// The live editor: typing, lists, quotes, tables, clicks, tasks, images, the source toggle and today's note.
const { test, expect } = require('@playwright/test');

const api = (request, method, url, data) =>
  request.fetch(url, { method, headers: { 'X-Explorer': '1', 'Content-Type': 'application/json' }, data });

/** Create a note through the API and open it in the editor. */
async function openNew(page, request, id, raw) {
  const r = await api(request, 'POST', '/api/note', { id, raw });
  expect(r.ok(), await r.text()).toBeTruthy();
  await page.goto('/#e/' + encodeURIComponent(id));
  await expect(page.locator('#ed')).toHaveClass(/lv/);
  await expect(page.locator('#live .lv-b').first()).toBeAttached();
}

/** The textarea being typed into: its text and caret. */
const active = page => page.evaluate(() => {
  const t = document.activeElement;
  return t && t.classList.contains('lv-ta') ? { value: t.value, caret: t.selectionStart } : null;
});
const activeValue = page => active(page).then(a => a && a.value);
const source = page => page.evaluate(() => document.querySelector('#ta').value);
const saved = async (request, id) => (await (await api(request, 'GET', '/api/note?id=' + encodeURIComponent(id))).json()).raw;

/** Press a key, then wait until the line being edited reads `expected`. */
async function press(page, key, expected) {
  await page.keyboard.press(key);
  await expect.poll(() => activeValue(page)).toBe(expected);
}

test('typing, list continuation, indenting and saving', async ({ page, request }) => {
  await openNew(page, request, 'E2E/lists', '');
  await expect.poll(() => activeValue(page)).toBe('');   // an empty note starts in edit mode
  await page.keyboard.type('# Plan');
  await press(page, 'Enter', '');
  await page.keyboard.type('- [ ] first');
  await press(page, 'Enter', '- [ ] ');                  // tasks continue
  await page.keyboard.type('second');
  await press(page, 'Enter', '- [ ] ');
  await press(page, 'Enter', '');                        // Enter on an empty item removes the marker, ending the list
  await page.keyboard.type('1. one');
  await press(page, 'Enter', '2. ');                     // numbers count up
  await page.keyboard.type('two');
  await press(page, 'Enter', '3. ');
  await press(page, 'Tab', '  3. ');                     // Tab indents a list item
  await page.keyboard.type('nested');
  await press(page, 'Shift+Tab', '3. nested');           // Shift+Tab outdents it
  await expect(page.locator('#live h1')).toHaveText('Plan');
  await page.keyboard.press('Control+s');
  await expect.poll(() => saved(request, 'E2E/lists')).toBe('# Plan\n- [ ] first\n- [ ] second\n1. one\n2. two\n3. nested');
});

test('quotes and callouts continue, and an empty quote line ends them', async ({ page, request }) => {
  await openNew(page, request, 'E2E/quote', '');
  await page.keyboard.type('> [!note] Title');
  await press(page, 'Enter', '> [!note] Title\n> ');
  await page.keyboard.type('body');
  await press(page, 'Enter', '> [!note] Title\n> body\n> ');
  await press(page, 'Enter', '');
  expect(await source(page)).toBe('> [!note] Title\n> body\n');
  await expect(page.locator('#live blockquote.co')).toContainText('body');
});

test('tables: Enter adds a row, Tab moves to the next cell', async ({ page, request }) => {
  await openNew(page, request, 'E2E/table', '| A | B |\n|---|---|\n| 1 | 2 |');
  await page.locator('#live td', { hasText: '1' }).click();
  await expect.poll(() => activeValue(page)).toBe('| A | B |\n|---|---|\n| 1 | 2 |');
  await page.keyboard.press('Enter');
  await expect.poll(() => activeValue(page)).toBe('| A | B |\n|---|---|\n| 1 | 2 |\n|  |  |');
  await page.keyboard.type('x');
  await page.keyboard.press('Tab');
  await page.keyboard.type('y');
  await expect.poll(() => activeValue(page)).toBe('| A | B |\n|---|---|\n| 1 | 2 |\n| x | y |');
});

test('a click on rendered text puts the caret at that spot in the markdown', async ({ page, request }) => {
  await openNew(page, request, 'E2E/click', 'Go to [[Home|the home page]] and **bold** text');
  // click on the "h" of "home" inside the link's display text
  const point = await page.evaluate(() => {
    const a = [...document.querySelectorAll('#live a')].find(x => x.textContent === 'the home page');
    const r = document.createRange(); r.setStart(a.firstChild, 4); r.setEnd(a.firstChild, 5);
    const b = r.getBoundingClientRect();
    return { x: b.left + 1, y: b.top + b.height / 2 };
  });
  await page.mouse.click(point.x, point.y);
  await expect.poll(() => active(page).then(a => a && a.value.slice(0, a.caret))).toBe('Go to [[Home|the ');
  // and on "o" in the bold word, after Escape
  await page.keyboard.press('Escape');
  const bold = await page.evaluate(() => {
    const s = [...document.querySelectorAll('#live strong, #live b')].find(x => x.textContent === 'bold');
    const r = document.createRange(); r.setStart(s.firstChild, 1); r.setEnd(s.firstChild, 2);
    const b = r.getBoundingClientRect();
    return { x: b.left + 1, y: b.top + b.height / 2 };
  });
  await page.mouse.click(bold.x, bold.y);
  await expect.poll(() => active(page).then(a => a && a.value.slice(0, a.caret))).toBe('Go to [[Home|the home page]] and **b');
});

test('clicking a task box ticks it without opening the line', async ({ page, request }) => {
  await openNew(page, request, 'E2E/tasks', '- [ ] buy milk');
  await page.locator('#live b.m').click();
  await expect.poll(() => source(page)).toBe('- [x] buy milk');
  expect(await active(page)).toBeNull();
});

test('pasting an image saves it in the vault and embeds it', async ({ page, request }) => {
  await openNew(page, request, 'E2E/img', '');
  await expect.poll(() => activeValue(page)).toBe('');
  await page.evaluate(async () => {
    const c = document.createElement('canvas'); c.width = 40; c.height = 20;
    c.getContext('2d').fillRect(0, 0, 40, 20);
    const blob = await new Promise(r => c.toBlob(r, 'image/png'));
    const dt = new DataTransfer(); dt.items.add(new File([blob], 'image.png', { type: 'image/png' }));
    document.activeElement.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }));
  });
  await expect.poll(() => source(page)).toMatch(/^!\[\[Pasted image \d{14}\.png\]\]\n$/);
  const img = page.locator('#live img');
  await expect(img).toBeVisible();
  await expect.poll(() => img.evaluate(i => i.naturalWidth)).toBe(40);
});

test('the Live / Source button switches editors and is remembered', async ({ page, request }) => {
  await openNew(page, request, 'E2E/mode', 'hello **world**');
  await page.locator('#tb button', { hasText: 'Live' }).click();
  await expect(page.locator('#ta')).toBeVisible();
  await expect(page.locator('#live')).toBeHidden();
  await expect(page.locator('#pv strong, #pv b').first()).toHaveText('world');
  await page.reload();
  await expect(page.locator('#ta')).toBeVisible();
  await page.locator('#tb button', { hasText: 'Source' }).click();
  await expect(page.locator('#live')).toBeVisible();
});

test("today's note opens in the editor and saves", async ({ page, request }) => {
  await page.goto('/');
  await page.click('#todaybtn');
  const d = new Date(), z = n => String(n).padStart(2, '0');
  const today = `${d.getFullYear()}-${z(d.getMonth() + 1)}-${z(d.getDate())}`;
  await expect(page).toHaveURL(new RegExp(`#e/${today}$`));
  await expect.poll(() => activeValue(page)).toBe('');
  await page.keyboard.type('- written today');
  await page.keyboard.press('Control+s');
  await expect.poll(() => saved(request, today)).toBe('- written today');
});
