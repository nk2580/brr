import { mkdir } from 'node:fs/promises'
import { expect, test, type Page } from '@playwright/test'
import { installChatExperienceFixture } from './fixtures/chat-experience'

const evidence = 'projects/desktop-agent/evidence/branch-notices'
const interrupted = { id: 'branch-notice-one', sourceThreadId: 'chat-history', phase: 'needs_attention', cwd: '/workspace/current-files-branch', error: 'thread not found: missing-fixture-chat' }

async function setPending(page: Page, operations: typeof interrupted[]) {
  await page.evaluate(values => {
    const key = 'chatExperience.fixture.v1'
    const state = JSON.parse(localStorage.getItem(key)!)
    state.pendingBranches = values
    localStorage.setItem(key, JSON.stringify(state))
  }, operations)
  await page.reload()
  await expect(page.getByRole('combobox', { name: 'Model', exact: true })).toBeEnabled()
}

test.beforeEach(async ({ page }) => {
  await mkdir(evidence, { recursive: true })
  await installChatExperienceFixture(page)
  await page.setViewportSize({ width: 800, height: 600 })
  await page.goto('/')
  await expect(page.getByRole('combobox', { name: 'Model', exact: true })).toBeEnabled()
  await setPending(page, [interrupted])
})

test('dismisses durably without altering the draft or branch and reopens through Saved messages', async ({ page }) => {
  const notice = page.getByRole('region', { name: 'Interrupted branch setup' })
  await expect(notice).toBeVisible()
  const editor = page.getByRole('textbox', { name: 'Message', exact: true })
  await editor.fill('Keep this draft while dismissing the notice')
  const dismiss = page.getByRole('button', { name: 'Dismiss interrupted setup notices' })
  await dismiss.focus()
  await page.keyboard.press('Enter')
  await expect(notice).toHaveCount(0)
  await expect(editor).toHaveText('Keep this draft while dismissing the notice')
  await page.screenshot({ path: `${evidence}/dismissed-800.png` })
  await page.reload()
  await expect(page.getByRole('button', { name: 'Draft Keep this draft while dismissing the notice' })).toBeVisible()
  await expect(notice).toHaveCount(0)
  await page.getByRole('button', { name: 'Draft Keep this draft while dismissing the notice' }).click()
  await page.getByRole('button', { name: 'Saved messages', exact: true }).click()
  const review = page.getByRole('button', { name: /^Review interrupted setups/ })
  await expect(review).toBeVisible()
  await review.click()
  await expect(notice).toBeVisible()
  await expect(notice.getByRole('button', { name: 'Recover setup', exact: true })).toBeEnabled()
  await expect(editor).toHaveText('Keep this draft while dismissing the notice')
  expect(await page.evaluate(() => {
    const state = (window as any).__chatExperience.inspect()
    return state.requests.filter((request: any) => ['branches.recover', 'branches.create', 'foreground.submit'].includes(request.method))
  })).toEqual([])
  expect(await page.evaluate(() => (window as any).__chatExperience.inspect().pendingBranches)).toEqual([interrupted])
  await page.screenshot({ path: `${evidence}/reviewed-800.png` })
})

test('shows new operations and changed recovery failures after an earlier dismissal', async ({ page }) => {
  await page.getByRole('button', { name: 'Dismiss interrupted setup notices' }).click()
  const changed = { ...interrupted, phase: 'workspace_prepared', error: 'Destination setup needs approval' }
  const newOperation = { ...interrupted, id: 'branch-notice-two', cwd: '/workspace/second-branch' }
  await setPending(page, [changed, newOperation])
  const notice = page.getByRole('region', { name: 'Interrupted branch setup' })
  await expect(notice).toBeVisible()
  await expect(notice.getByRole('button', { name: 'Recover setup', exact: true })).toHaveCount(2)
  await expect(notice).toContainText('Destination setup needs approval')
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({ path: `${evidence}/new-attention-800.png` })
})
