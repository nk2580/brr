import { test, expect } from '@playwright/test'

test('browser preview renders and keeps native-only actions disabled', async ({page}) => {
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await page.goto('/')
  await expect(page.getByRole('heading', {name:'What are we building?'})).toBeVisible()
  await expect(page.getByRole('button', {name:'Send message'})).toBeDisabled()
  await expect(page.getByRole('combobox', {name:'Project'})).toBeDisabled()
  await expect(page.getByRole('alert')).toHaveCount(0)
  expect(errors).toEqual([])
  await page.getByRole('button', {name:'Profile', exact:true}).click()
  await expect(page.getByRole('region', {name:'User profile'}).getByRole('button', {name:'Sign in with ChatGPT'})).toBeDisabled()
  await page.getByRole('button', {name:'Close profile'}).click()
  await page.getByRole('textbox', {name:'Message'}).fill('Explore this project')
  await expect(page.getByRole('button', {name:'Send message'})).toBeDisabled()
})
