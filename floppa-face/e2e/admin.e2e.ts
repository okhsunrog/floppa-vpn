/// <reference lib="dom" />
import { test, expect, type Page } from '@playwright/test'
import type { Plan } from '../../floppa-web-shared/src/client/types.gen'

async function login(page: Page, account = 'dev-admin') {
  await page.addInitScript(() => localStorage.setItem('locale', 'en'))
  await page.goto('/login')
  await page.getByPlaceholder('Login', { exact: true }).fill(account)
  await page.getByPlaceholder('Password', { exact: true }).fill('floppa-local-2026')
  await page.getByPlaceholder('Password', { exact: true }).press('Enter')
  await expect(page).not.toHaveURL(/\/login/)
}

for (const mobile of [false, true]) {
  test(`real plan CRUD persists through reload (${mobile ? 'mobile' : 'desktop'})`, async ({
    page,
  }) => {
    if (mobile) await page.setViewportSize({ width: 390, height: 844 })
    await login(page)
    const token = await page.evaluate(() => localStorage.getItem('floppa-token'))
    const headers = { Authorization: `Bearer ${token}` }
    const name = `e2e_${Date.now()}_${mobile ? 'm' : 'd'}`
    let id: number | undefined
    try {
      await page.goto('/admin/plans')
      await page.getByRole('button', { name: 'Add Plan', exact: true }).click()
      const dialog = page.getByRole('dialog')
      await dialog.getByRole('textbox', { name: 'Internal Name', exact: true }).fill(name)
      await dialog.getByRole('textbox', { name: 'Display Name', exact: true }).fill(name)
      await dialog.getByRole('spinbutton', { name: 'Speed Limit (Mbps)', exact: true }).fill('73')
      await dialog.getByRole('spinbutton', { name: 'Max Peers', exact: true }).fill('4')
      await dialog.getByRole('spinbutton', { name: 'Price (⭐)', exact: true }).fill('250')
      await dialog.getByRole('spinbutton', { name: 'Period (days)', exact: true }).fill('45')
      await dialog.getByRole('spinbutton', { name: 'Trial duration', exact: true }).fill('2')
      await dialog.getByRole('checkbox', { name: /Singapore/ }).check()
      const created = page.waitForResponse(
        (r) => r.url().endsWith('/api/plans') && r.request().method() === 'POST',
      )
      await dialog.getByRole('button', { name: 'Create', exact: true }).click()
      const response = await created
      expect(response.status()).toBe(201)
      const plan: Plan = await response.json()
      id = plan.id
      expect(plan).toMatchObject({
        name,
        max_peers: 4,
        price_stars: 250,
        period_days: 45,
        trial_minutes: 2880,
        default_speed_limit_mbps: 73,
      })
      expect(plan.region_ids).toContain('singapore')
      await expect(dialog).not.toBeVisible()
      await page.reload()
      const row = mobile
        ? page
            .locator('.md\\:hidden > div')
            .filter({ has: page.locator('code', { hasText: name }) })
        : page.getByRole('row').filter({ has: page.locator('code', { hasText: name }) })
      await row.getByRole('button', { name: 'Edit Plan', exact: true }).click()
      await expect(dialog.getByRole('spinbutton', { name: 'Max Peers', exact: true })).toHaveValue(
        '4',
      )
      await dialog.getByRole('spinbutton', { name: 'Max Peers', exact: true }).fill('6')
      await dialog.getByRole('button', { name: 'Save', exact: true }).click()
      await expect(dialog).not.toBeVisible()
      const persisted: Plan[] = await (await page.request.get('/api/plans', { headers })).json()
      expect(persisted.find((p) => p.id === id)?.max_peers).toBe(6)
      await row.getByRole('button', { name: 'Delete', exact: true }).click()
      await page.getByRole('dialog').getByRole('button', { name: 'Delete', exact: true }).click()
      await expect(page.getByRole('dialog')).not.toBeVisible()
      const remaining: Plan[] = await (await page.request.get('/api/plans', { headers })).json()
      expect(remaining.some((p) => p.id === id)).toBe(false)
    } finally {
      // Clean up only the plan created by this test, even after a UI assertion fails.
      if (id !== undefined) await page.request.delete(`/api/plans/${id}`, { headers })
    }
  })
}

test('real authentication enforces the admin boundary', async ({ page, request }) => {
  expect((await request.get('/api/users')).status()).toBe(401)
  await login(page, 'dev-user')
  const token = await page.evaluate(() => localStorage.getItem('floppa-token'))
  expect(
    (await request.get('/api/users', { headers: { Authorization: `Bearer ${token}` } })).status(),
  ).toBe(403)
  await page.goto('/admin/plans')
  await expect(page).not.toHaveURL(/\/admin/)
})

for (const mobile of [false, true]) {
  test(`subscription filters survive reload and reset keeps unrelated query parameters (${mobile ? 'mobile' : 'desktop'})`, async ({
    page,
  }) => {
    if (mobile) await page.setViewportSize({ width: 390, height: 844 })
    await login(page)
    await page.goto('/admin/subscriptions?subscription=expired')
    await expect(page.getByRole('heading', { name: 'Subscriptions', exact: true })).toBeVisible()
    await expect(page.getByText('Expired', { exact: true }).first()).toBeVisible()
    await page.reload()
    await expect(page).toHaveURL(/subscription=expired/)
    await expect(page.getByText('Expired', { exact: true }).first()).toBeVisible()
    await page.goto('/admin/subscriptions?source=overview&subscription=expired&q=test-user')
    await page.getByRole('button', { name: 'Reset', exact: true }).click()
    await expect(page).toHaveURL(/\/admin\/subscriptions\?source=overview$/)
    await expect(page.getByRole('textbox', { name: 'Search users...' })).toHaveValue('')
    await page.screenshot({
      path: `test-results/subscriptions-${mobile ? 'mobile' : 'desktop'}.png`,
      fullPage: true,
    })
  })
}

test('admin navigation switches without duplication at the desktop breakpoint', async ({
  page,
}) => {
  await login(page)
  await page.goto('/admin')
  await page.setViewportSize({ width: 1279, height: 850 })
  const sections = page.getByRole('navigation', { name: 'Admin sections' })
  await expect(sections).not.toBeVisible()
  await page.getByRole('button', { name: 'Open menu' }).click()
  await expect(
    page.getByRole('dialog').getByRole('link', { name: 'Subscriptions', exact: true }),
  ).toBeVisible()
  await page.setViewportSize({ width: 1280, height: 850 })
  await expect(page.getByRole('dialog')).not.toBeVisible()
  await expect(page.getByRole('button', { name: 'Open menu' })).not.toBeVisible()
  await expect(sections).toBeVisible()
})

for (const mobile of [false, true]) {
  test(`peer regions appear in lists and user details (${mobile ? 'mobile' : 'desktop'})`, async ({
    page,
  }) => {
    if (mobile) await page.setViewportSize({ width: 390, height: 844 })
    await login(page)
    await page.goto('/admin/peers')
    await page
      .getByRole('textbox', { name: 'Search by IP, user, device, region...' })
      .fill('Frankfurt')
    const peer = mobile
      ? page.getByRole('button').filter({ hasText: 'Region: Frankfurt' }).first()
      : page.getByRole('table').getByRole('button').filter({ hasText: 'Frankfurt' }).first()
    await expect(peer).toBeVisible()
    await peer.click()
    await expect(page).toHaveURL(/\/admin\/users\/\d+/)
    await expect(page.getByText('Region: Frankfurt', { exact: true }).first()).toBeVisible()
  })
}
