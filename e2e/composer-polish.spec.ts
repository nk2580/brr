import { mkdir } from 'node:fs/promises'
import { expect, test, type Locator, type Page } from '@playwright/test'
import { installChatExperienceFixture } from './fixtures/chat-experience'

const evidence = 'projects/desktop-agent/evidence/composer-polish'

async function expectOneFooter(page: Page) {
  const composer = (await page.locator('.composer-area').boundingBox())!
  const actions: Locator[] = [
    page.getByRole('button', { name: 'Add context', exact: true }),
    page.getByRole('combobox', { name: /^(Project|Move chat to project)$/ }),
    page.getByRole('button', { name: 'Plan mode', exact: true }),
    page.getByRole('combobox', { name: 'Model', exact: true }),
    page.getByRole('button', { name: 'Chat options', exact: true }),
    page.getByRole('button', { name: /^(Send message|Queue message)$/ }),
  ]
  const stop = page.getByRole('button', { name: 'Stop', exact: true })
  if (await stop.isVisible()) actions.push(page.locator('summary[aria-label="More send options"]'), stop)
  const boxes = []
  for (const action of actions) {
    await expect(action).toBeVisible()
    const box = (await action.boundingBox())!
    expect(box.x).toBeGreaterThanOrEqual(composer.x)
    expect(box.x + box.width).toBeLessThanOrEqual(composer.x + composer.width)
    boxes.push(box)
  }
  const centers = boxes.map(box => box.y + box.height / 2)
  expect(Math.max(...centers) - Math.min(...centers), 'all footer controls share one baseline').toBeLessThan(3)
  const ordered = boxes.toSorted((a, b) => a.x - b.x)
  for (let index = 1; index < ordered.length; index += 1) {
    expect(ordered[index].x, 'footer controls must not overlap').toBeGreaterThanOrEqual(ordered[index - 1].x + ordered[index - 1].width - 1)
  }
}

test.beforeEach(async ({ page }) => {
  await mkdir(evidence, { recursive: true })
  await installChatExperienceFixture(page)
  await page.goto('/')
  await expect(page.getByRole('combobox', { name: 'Model', exact: true })).toBeEnabled()
})

test('compact composer has one aligned footer and an upward keyboard-accessible Add menu', async ({ page }) => {
  for (const viewport of [{ width: 1280, height: 860 }, { width: 800, height: 600 }]) {
    await page.setViewportSize(viewport)
    await expectOneFooter(page)
    expect((await page.locator('.composer-area').boundingBox())!.height, 'empty composer stays compact').toBeLessThanOrEqual(125)
    await page.locator('.composer-area').screenshot({ path: `${evidence}/composer-${viewport.width}.png` })
    const add = page.getByRole('button', { name: 'Add context', exact: true })
    await add.focus()
    await page.locator('.chat-editor-input').click({ position: { x: 24, y: 50 } })
    await expect(page.getByRole('textbox', { name: 'Message', exact: true }), 'the full empty writing area accepts focus').toBeFocused()
    await add.focus()
    await page.keyboard.press('Enter')
    const menu = page.getByRole('menu', { name: 'Add context', exact: true })
    await expect(menu).toBeVisible()
    const box = (await menu.boundingBox())!
    const trigger = (await add.boundingBox())!
    expect(box.y).toBeGreaterThanOrEqual(0)
    expect(box.y + box.height).toBeLessThanOrEqual(trigger.y)
    await page.screenshot({ path: `${evidence}/context-menu-${viewport.width}.png` })
    await page.keyboard.press('Escape')
    await expect(menu).toHaveCount(0)
    await expect(add).toBeFocused()
  }
})

test('attachments sit above the footer and the editor grows without moving controls apart', async ({ page }) => {
  await page.setViewportSize({ width: 800, height: 600 })
  const editor = page.getByRole('textbox', { name: 'Message', exact: true })
  await editor.fill('A short draft')
  const initial = (await page.locator('.chat-editor-input').boundingBox())!.height
  await editor.fill(Array.from({ length: 14 }, (_, index) => `Draft line ${index + 1}`).join('\n'))
  const grown = (await page.locator('.chat-editor-input').boundingBox())!.height
  expect(grown).toBeGreaterThan(initial)
  expect(grown).toBeLessThanOrEqual(200)
  await editor.evaluate(element => {
    const transfer = new DataTransfer()
    transfer.items.add(new File(['Project notes'], 'notes.txt', { type: 'text/plain' }))
    element.dispatchEvent(new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: transfer }))
  })
  const cards = page.getByRole('list', { name: 'Attached context' })
  await expect(cards.getByRole('button', { name: 'Preview notes.txt' })).toBeVisible()
  const cardBox = (await cards.boundingBox())!
  const addBox = (await page.getByRole('button', { name: 'Add context', exact: true }).boundingBox())!
  expect(cardBox.y + cardBox.height, 'attachments precede the action footer').toBeLessThanOrEqual(addBox.y)
  await expectOneFooter(page)
  await page.screenshot({ path: `${evidence}/attachments-and-growing-editor-800.png` })
  await cards.getByRole('button', { name: 'Remove notes.txt' }).click()
  await expect(cards).toHaveCount(0)
  await expect(editor).toContainText('Draft line 14')
  await page.getByRole('button', { name: 'Active work', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Stop', exact: true })).toBeVisible()
  await expectOneFooter(page)
  await page.screenshot({ path: `${evidence}/running-composer-800.png` })
})

test('running action menu opens above the footer without shifting Send or Stop and Escape restores focus', async ({ page }) => {
  await page.setViewportSize({ width: 800, height: 600 })
  await page.getByRole('button', { name: 'Active work', exact: true }).click()
  const stop = page.getByRole('button', { name: 'Stop', exact: true })
  await expect(stop).toBeVisible()
  const before = (await stop.boundingBox())!
  const trigger = page.locator('summary[aria-label="More send options"]')
  await trigger.focus()
  await page.keyboard.press('Enter')
  const menu = page.locator('.send-more [role="menu"]')
  await expect(menu).toBeVisible()
  const box = (await menu.boundingBox())!
  const triggerBox = (await trigger.boundingBox())!
  expect(box.y + box.height, 'running action menu opens above the footer').toBeLessThanOrEqual(triggerBox.y)
  expect(box.y).toBeGreaterThanOrEqual(0)
  expect(box.x + box.width).toBeLessThanOrEqual(800)
  const after = (await stop.boundingBox())!
  expect(after.x).toBe(before.x)
  expect(after.y).toBe(before.y)
  await page.screenshot({ path: `${evidence}/running-action-menu-800.png` })
  await page.keyboard.press('Escape')
  await expect(menu).toBeHidden()
  await expect(trigger).toBeFocused()
})
