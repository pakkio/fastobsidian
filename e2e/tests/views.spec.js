// The read-only views: index, sidebar search, note view, graph, theme.
const { test, expect } = require('@playwright/test');

test.beforeEach(async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('#vname')).toHaveText(/fo-e2e-/);
});

test('index shows the vault stats and tags', async ({ page }) => {
  const body = page.locator('#indexBody');
  await expect(body).toContainText('NOTES', { ignoreCase: true });
  await expect(body).toContainText('#project');
  await expect(body).toContainText('#idea');
});

test('sidebar search finds a note by name and by text', async ({ page }) => {
  await page.fill('#q', 'zettel');
  await expect(page.locator('#list')).toContainText('Zettelkasten');
  await page.fill('#q', 'atomic notes');
  await expect(page.locator('#list')).toContainText('Zettelkasten');
});

test('a note renders with links, a callout and backlinks', async ({ page }) => {
  await page.goto('/#n/Ideas/Zettelkasten');
  const note = page.locator('#noteBody');
  await expect(note.locator('h1').first()).toHaveText('Zettelkasten');
  await expect(note.locator('blockquote.co')).toContainText('Keep notes small');
  await expect(note.locator('a.wl', { hasText: 'Note Linking' }).first()).toBeVisible();
  await expect(note).toContainText('Home');   // Home links here: listed as a backlink
  await note.locator('a.wl', { hasText: 'Note Linking' }).first().click();
  await expect(page).toHaveURL(/#n\/Ideas(%2F|\/)Note(%20| )Linking$/);
});

test('the graph draws on its canvas', async ({ page }) => {
  await page.click('nav button[data-v="graph"]');
  const cv = page.locator('#cv');
  await expect(cv).toBeVisible();
  const box = await cv.boundingBox();
  expect(box.width).toBeGreaterThan(100);
  expect(box.height).toBeGreaterThan(100);
  // something has been drawn: not every pixel is transparent
  await expect.poll(() => cv.evaluate(c => {
    const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
    for (let i = 3; i < d.length; i += 4) if (d[i]) return true;
    return false;
  })).toBe(true);
});

test('the theme toggle switches light and dark', async ({ page }) => {
  const before = await page.evaluate(() => document.documentElement.dataset.theme || '');
  await page.click('#theme');
  await expect.poll(() => page.evaluate(() => document.documentElement.dataset.theme || '')).not.toBe(before);
});
