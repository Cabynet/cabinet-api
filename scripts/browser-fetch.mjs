import { createRequire } from "node:module"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

const here = dirname(fileURLToPath(import.meta.url))
// Playwright must be resolvable from this package (or set PLAYWRIGHT_MODULE to an absolute path).
const { chromium } = process.env.PLAYWRIGHT_MODULE
  ? await import(process.env.PLAYWRIGHT_MODULE)
  : createRequire(join(here, "../package.json"))("@playwright/test")

const targetUrl = process.argv[2]
if (!targetUrl) {
  process.exit(1)
}

let browser
try {
  browser = await chromium.launch({
    headless: true,
    args: ["--disable-blink-features=AutomationControlled"],
  })
  const context = await browser.newContext({
    userAgent:
      "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    viewport: { width: 1280, height: 800 },
    locale: "en-US",
  })
  await context.addInitScript(() => {
    Object.defineProperty(navigator, "webdriver", { get: () => undefined })
  })
  const page = await context.newPage()
  const response = await page.goto(targetUrl, {
    waitUntil: "domcontentloaded",
    timeout: 12000,
  })
  if (!response || response.status() >= 400) {
    process.exit(2)
  }
  const title = (await page.title()).trim()
  if (/^(just a moment|attention required|access denied)/i.test(title)) {
    process.exit(3)
  }
  const html = await page.content()
  if (!html || Buffer.byteLength(html, "utf8") > 5 * 1024 * 1024) {
    process.exit(4)
  }
  process.stdout.write(JSON.stringify({ finalUrl: page.url(), html }))
} catch {
  process.exit(1)
} finally {
  await browser?.close().catch(() => undefined)
}
