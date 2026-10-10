/* The whiteboard in a real Chrome, against the real learning server (offline demo tutor) and under its real CSP.
   Needs Google Chrome installed and the bridge's Python (bridge/.venv, else python3). Run: npm run e2e */
import { spawn, spawnSync } from 'node:child_process'
import { existsSync, mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'
import { chromium } from 'playwright-core'

const bridge = join(dirname(fileURLToPath(import.meta.url)), '../..')
const venv = join(bridge, '.venv/bin/python')
const python = existsSync(venv) ? venv : 'python3'
const env = { ...process.env, PYTHONPATH: join(bridge, 'src'), PYTHONUNBUFFERED: '1' }
const dataDir = mkdtempSync(join(tmpdir(), 'eve-board-e2e-'))

/* A lesson as it was saved before tldraw: pen strokes, an eraser stroke, and the flattened picture of them. */
const seed = spawnSync(python, ['-c', `
import base64, struct, sys, zlib
from eve_bridge.learning.store import Store
def png(w, h):
    ink, paper = bytes((31, 58, 120)), bytes((255, 255, 255))
    rows = b"".join(b"\\x00" + b"".join(ink if 20 < x < 60 and 20 < y < 40 else paper for x in range(w)) for y in range(h))
    chunk = lambda kind, data: struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    return b"\\x89PNG\\r\\n\\x1a\\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")
store = Store(sys.argv[1])
s = store.create("learn", "Old board", "Grade 4", True)
s.update(problem="Solve 2x + 3 = 11.", stage="working", board_image="data:image/png;base64," + base64.b64encode(png(120, 78)).decode(),
         strokes=[{"tool": "pen", "points": [[.1, .2], [.3, .4]]}, {"tool": "eraser", "points": [[.2, .3]]}])
s.pop("board", None)
print(store.save(s, expected=s["revision"])["id"])
`, dataDir], { env, encoding: 'utf8' })
assert.equal(seed.status, 0, seed.stderr)
const legacyId = seed.stdout.trim()

const server = spawn(python, ['-m', 'eve_bridge.learning', '--demo', '--port', '0', '--no-open', '--data-dir', dataDir], { env })
let browser
try {
	const origin = await new Promise((resolve, reject) => {
		let out = ''
		server.stdout.on('data', d => { out += d; const m = out.match(/http:\/\/127\.0\.0\.1:\d+/); if (m) resolve(m[0]) })
		server.stderr.on('data', d => { out += d })
		server.on('exit', code => reject(new Error(`learning server exited ${code}: ${out}`)))
		setTimeout(() => reject(new Error('learning server did not start: ' + out)), 20000)
	})

	browser = await chromium.launch({ channel: 'chrome', headless: true })
	const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } }) // no bypassCSP: the real policy applies
	const page = await context.newPage()
	const problems = [], offOrigin = [], badResponses = []
	globalThis.e2eProblems = problems
	globalThis.e2eBad = badResponses
	setInterval(async () => { const t = await page.locator('#error:not(.hidden)').textContent({ timeout: 100 }).catch(() => ''); if (t && !problems.includes('shown: ' + t)) problems.push('shown: ' + t) }, 500).unref()
	await page.exposeFunction('reportCspViolation', v => problems.push('CSP: ' + v))
	await page.addInitScript(() => document.addEventListener('securitypolicyviolation', e => window.reportCspViolation(`${e.violatedDirective} blocked ${e.blockedURI} (${e.sourceFile}:${e.lineNumber})`)))
	page.on('pageerror', e => problems.push('pageerror: ' + e.message))
	/* The server has never had a favicon; Chrome asks for one anyway. */
	page.on('console', m => { if (m.type() === 'error' && !m.location().url.endsWith('/favicon.ico')) problems.push(`console: ${m.text()} (${m.location().url})`) })
	page.on('request', r => { const u = r.url(); if (!u.startsWith(origin) && !u.startsWith('data:') && !u.startsWith('blob:')) offOrigin.push(u) })
	page.on('response', r => { if (r.status() >= 400) badResponses.push(`${r.status()} ${r.request().method()} ${r.url()}`) })
	page.on('requestfailed', r => badResponses.push(`failed ${r.method()} ${r.url()} ${r.failure()?.errorText} body=${(r.postData() || '').length}`))

	const lessonOf = id => page.evaluate(async id => (await fetch('/api/lessons/' + id)).json(), id)
	const shapesOf = lesson => Object.values(lesson.board?.store ?? {}).filter(r => r.typeName === 'shape')
	const until = async (what, check, ms = 15000) => {
		const end = Date.now() + ms
		for (;;) { const value = await check(); if (value) return value; if (Date.now() > end) throw new Error('timed out: ' + what); await page.waitForTimeout(150) }
	}
	const scribble = async (dx = 0) => {
		const box = await page.locator('#board .tl-canvas').boundingBox()
		const x = box.x + box.width / 2 + dx, y = box.y + box.height / 2
		await page.mouse.move(x - 60, y - 40); await page.mouse.down()
		await page.mouse.move(x, y + 30, { steps: 8 }); await page.mouse.move(x + 60, y - 40, { steps: 8 }); await page.mouse.up()
	}

	// 1. A new lesson: the board mounts, and opening it saves nothing.
	await page.goto(origin)
	await page.locator('#new-main').click()
	await page.locator('#topic').fill('Algebra')
	await page.locator('#level').fill('Grades 6-8')
	await page.getByRole('button', { name: "Let's begin" }).click()
	await page.waitForFunction(() => document.querySelector('.problem-strip strong')?.textContent === 'Solve 2x + 3 = 11.')
	await page.locator('#board .tl-canvas').waitFor()
	/* Wait for the actual SDK watermark so absence cannot pass just because the board is still mounting. */
	const watermark = page.locator('#board [data-testid="tl-watermark-unlicensed"]')
	await watermark.waitFor({ state: 'attached' })
	assert.equal(await watermark.isVisible(), false, 'lesson watermark is hidden')
	/* Positive control: removing only our override must expose the same watermark. */
	const watermarkRule = await page.evaluate(() => {
		for (const [sheetIndex, sheet] of Array.from(document.styleSheets).entries()) {
			for (const [ruleIndex, rule] of Array.from(sheet.cssRules).entries()) {
				if (rule.selectorText === '#board .tl-watermark_SEE-LICENSE') {
					const cssText = rule.cssText
					sheet.deleteRule(ruleIndex)
					return { sheetIndex, ruleIndex, cssText }
				}
			}
		}
		throw new Error('watermark override missing from the shipped CSS')
	})
	assert.equal(await watermark.isVisible(), true, 'positive control exposes the watermark')
	await page.evaluate(({ sheetIndex, ruleIndex, cssText }) => document.styleSheets[sheetIndex].insertRule(cssText, ruleIndex), watermarkRule)
	assert.equal(await watermark.isVisible(), false, 'restoring the override hides the watermark')
	await page.setViewportSize({ width: 390, height: 844 })
	await page.waitForFunction(() => document.querySelector('[data-testid="tl-watermark-unlicensed"]')?.dataset.mobile === 'true')
	assert.equal(await watermark.isVisible(), false, 'mobile watermark is hidden')
	await page.setViewportSize({ width: 1440, height: 1000 })
	const id = page.url().split('#lesson/')[1]
	const opened = (await lessonOf(id)).revision
	await page.waitForTimeout(1500)
	assert.equal((await lessonOf(id)).revision, opened, 'opening a lesson must not save a new revision')

	// 2. Pen drawing is saved as a tldraw document, with a real picture for the tutor.
	await scribble()
	const drawn = await until('the first stroke to be saved', async () => { const l = await lessonOf(id); return shapesOf(l).length === 1 && l.board_image && l })
	assert.equal(shapesOf(drawn)[0].type, 'draw')
	assert.ok(drawn.board_image.startsWith('data:image/png;base64,'), 'board image is a PNG data URL')
	const ink = await page.evaluate(async src => {
		const image = new Image(); image.src = src; await image.decode()
		const c = document.createElement('canvas'); c.width = image.width; c.height = image.height
		const ctx = c.getContext('2d'); ctx.drawImage(image, 0, 0)
		const px = ctx.getImageData(0, 0, c.width, c.height).data
		const base = [px[0], px[1], px[2]]; let different = 0
		for (let i = 0; i < px.length; i += 4) if (Math.abs(px[i] - base[0]) + Math.abs(px[i + 1] - base[1]) + Math.abs(px[i + 2] - base[2]) > 60) different++
		return { width: image.width, height: image.height, different }
	}, drawn.board_image)
	assert.ok(ink.width > 50 && ink.height > 50 && ink.different > 100, 'board image shows the stroke: ' + JSON.stringify(ink))

	// 3. It survives a reload, and the reload saves nothing.
	await page.reload()
	await page.locator('#board .tl-shape').first().waitFor()
	assert.equal(await page.locator('#board .tl-shape').count(), 1)
	await watermark.waitFor({ state: 'attached' })
	assert.equal(await watermark.isVisible(), false, 'watermark stays hidden after reload')
	await page.waitForTimeout(1500)
	assert.equal((await lessonOf(id)).revision, drawn.revision, 'reloading must not save a new revision')

	// 4. The board locks while eve answers, then gives the pen back.
	await page.locator('#hint').click()
	await page.waitForFunction(() => document.querySelector('#feed')?.textContent.includes('subtract'))
	await until('the page to be idle again', () => page.evaluate(() => !document.querySelector('#hint').disabled))
	await scribble(160)
	await until('a second stroke after the hint', async () => shapesOf(await lessonOf(id)).length === 2)

	// 5. An ended lesson is read-only.
	await page.locator('#finish').click()
	await page.getByRole('button', { name: 'Resume lesson' }).waitFor()
	await until('the page to be idle again', () => page.evaluate(() => !document.querySelector('#finish').disabled))
	const ended = await lessonOf(id)
	await scribble(-160)
	await page.waitForTimeout(1500)
	const after = await lessonOf(id)
	assert.equal(after.revision, ended.revision, 'drawing on an ended lesson must not save')
	assert.equal(await page.locator('#board .tl-shape').count(), 2)

	// 6. A lesson from before tldraw shows its saved board, untouched until the learner adds to it.
	const legacyBefore = await lessonOf(legacyId)
	await page.goto(origin + '/#lesson/' + legacyId)
	await page.locator('#board .tl-shape[data-shape-type="image"]').waitFor()
	await page.waitForTimeout(1500)
	assert.equal((await lessonOf(legacyId)).revision, legacyBefore.revision, 'showing an old board must not save')
	await scribble()
	const legacyAfter = await until('new work on the old board', async () => { const l = await lessonOf(legacyId); return shapesOf(l).length === 2 && l })
	assert.deepEqual(legacyAfter.strokes, legacyBefore.strokes, 'old strokes are kept')
	assert.deepEqual(shapesOf(legacyAfter).map(s => s.type).sort(), ['draw', 'image'])

	assert.deepEqual(offOrigin, [], 'no request may leave this computer')
	assert.deepEqual(badResponses, [], 'every request must succeed')
	assert.deepEqual(problems, [], 'no CSP violation, page error or console error')
	console.log('board e2e passed')
} catch (error) {
	console.error('page problems so far:', globalThis.e2eProblems, globalThis.e2eBad)
	throw error
} finally {
	await browser?.close()
	server.kill()
	rmSync(dataDir, { recursive: true, force: true })
}
