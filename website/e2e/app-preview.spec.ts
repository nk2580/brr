import {mkdir} from 'node:fs/promises'
import {expect, test} from '@playwright/test'

const evidence = 'projects/desktop-agent/evidence/actual-app-preview'
test.beforeEach(async () => { await mkdir(evidence, {recursive: true}) })

test('renders the real app shell and composer inside the marketing page', async ({page}) => {
  await page.goto('./#possibilities')
  const preview = page.frameLocator('iframe[title="Brr app preview"]')
  await expect(preview.locator('.app-shell')).toBeVisible()
  await expect(preview.getByRole('textbox', {name: 'Message', exact: true})).toBeVisible()
  await expect(preview.getByRole('button', {name: 'Agents', exact: true})).toBeVisible()
  await expect(preview.getByRole('button', {name: 'Tickets', exact: true})).toBeVisible()
  await expect(preview.getByRole('button', {name: 'New tab', exact: true})).toBeVisible()
  await expect(preview.getByText('Help me write a warm welcome note for new contributors.', {exact: true})).toBeVisible()
  await expect(preview.getByRole('button', {name: 'Chat options', exact: true})).toBeVisible()
  expect(await page.evaluate(() => '__TAURI_INTERNALS__' in window)).toBe(false)
  await page.locator('.workflow-tour').screenshot({path: `${evidence}/embedded-desktop.png`})
})

test('keeps the real editor draft through menus and navigation, then resets sample state', async ({page}) => {
  await page.goto('./#possibilities')
  const preview = page.frameLocator('iframe[title="Brr app preview"]')
  const editor = preview.getByRole('textbox', {name: 'Message', exact: true})
  await expect(preview.getByRole('button', {name: 'Chat options', exact: true})).toBeEnabled()
  await editor.fill('Keep this preview draft')
  await preview.getByRole('button', {name: 'Chat options', exact: true}).click()
  await expect(preview.getByRole('combobox', {name: 'Reasoning effort'})).toBeVisible()
  await page.keyboard.press('Escape')
  await preview.getByRole('button', {name: 'Agents', exact: true}).click()
  await expect(preview.getByText('Research partner', {exact: true}).first()).toBeVisible()
  await preview.getByRole('button', {name: 'Home', exact: true}).click()
  await expect(editor).toHaveText('Keep this preview draft')
  await page.getByRole('button', {name: 'Reset preview', exact: true}).click()
  await expect(editor).toHaveText('')
  await expect(preview.getByText('Help me write a warm welcome note for new contributors.', {exact: true})).toBeVisible()
})

test('shows real workspace files without native access and blocks preview connections', async ({page}) => {
  await page.goto('app-preview.html')
  await expect(page.getByRole('textbox', {name: 'Message', exact: true})).toBeVisible()
  await page.getByRole('button', {name: 'New tab', exact: true}).click()
  await page.getByRole('button', {name: 'Files', exact: true}).click()
  await expect(page.getByRole('button', {name: /README.md/}).first()).toBeVisible()
  await page.getByRole('button', {name: 'src', exact: true}).click()
  await expect(page.getByRole('button', {name: /App.tsx/})).toBeVisible()
  const policy = await page.locator('meta[http-equiv="Content-Security-Policy"]').getAttribute('content')
  expect(policy).toContain("connect-src 'none'")
  expect(await page.evaluate(async () => {
    try { await fetch('./preview-connection-probe'); return false } catch { return true }
  })).toBe(true)
})

test('contains the desktop viewport at mobile width without changing the app UI', async ({page}) => {
  await page.setViewportSize({width: 390, height: 844})
  await page.goto('./#possibilities')
  const iframe = page.locator('iframe[title="Brr app preview"]')
  await expect(page.frameLocator('iframe[title="Brr app preview"]').locator('.app-shell')).toBeVisible()
  expect((await iframe.boundingBox())!.width).toBeGreaterThanOrEqual(800)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  const region = page.getByRole('region', {name: 'Desktop app preview', exact: true})
  await region.focus()
  await page.keyboard.press('ArrowRight')
  await expect.poll(() => region.evaluate(element => element.scrollLeft)).toBeGreaterThan(0)
  await page.locator('.workflow-tour').screenshot({path: `${evidence}/embedded-mobile.png`})
})
