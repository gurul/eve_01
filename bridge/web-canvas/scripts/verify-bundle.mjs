/* After a build: is everything the learning server must serve really there, and can its /canvas/ route name every file? */
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs'
import { dirname, extname, join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'

const here = dirname(fileURLToPath(import.meta.url))
const out = join(here, '../../src/eve_bridge/learning/web/canvas')
const server = readFileSync(join(here, '../../src/eve_bridge/learning/server.py'), 'utf8')

for (const name of ['canvas.js', 'canvas.css', 'TLDRAW-LICENSE.md', 'assets/translations/en.json', 'assets/icons/icon/0_merged.svg'])
	assert.ok(existsSync(join(out, name)) && statSync(join(out, name)).size > 0, name + ' is missing from the bundle')
assert.ok(readdirSync(join(out, 'assets/fonts')).filter(f => f.endsWith('.woff2')).length >= 12, 'fonts are missing')
assert.equal(readFileSync(join(out, 'TLDRAW-LICENSE.md'), 'utf8'), readFileSync(join(here, '../TLDRAW-LICENSE.md'), 'utf8'), 'licence copy differs')
assert.match(readFileSync(join(out, 'TLDRAW-LICENSE.md'), 'utf8'), /^# tldraw license/, 'licence copy is not the full text')

/* The server's own rules, read from its source so the two cannot drift: the path pattern and the extension table. */
const pattern = new RegExp('^(?:' + server.match(/CANVAS_FILE = re\.compile\(r"(.+)"\)/)[1] + ')$')
const table = server.slice(server.indexOf('CANVAS_MIME = {'), server.indexOf('MAX_BOARD'))
const walk = dir => readdirSync(dir, { withFileTypes: true }).flatMap(e => e.isDirectory() ? walk(join(dir, e.name)) : [join(dir, e.name)])
const files = walk(out).map(f => relative(out, f).split('\\').join('/'))
const unservable = files.filter(f => !pattern.test(f) || !table.includes(`"${extname(f)}"`))
assert.deepEqual(unservable, [], 'the /canvas/ route would refuse these files')
assert.ok(!pattern.test('../server.py') && !pattern.test('assets/../../app.js') && !pattern.test('.hidden'), 'path pattern control')

/* The page must not reach for another origin. tldraw's default CDN host may appear in the script as dead code; a stylesheet
   @import or url() to another host would be live. */
const remote = /(?:@import|url\()\s*['"]?(?:https?:)?\/\//
assert.match("@import url('https://fonts.googleapis.com/css2?family=Inter');", remote, 'remote-load control') // the template shipped this line
assert.doesNotMatch(readFileSync(join(out, 'canvas.css'), 'utf8'), remote, 'canvas.css loads from another origin')
console.log(`bundle verification passed: ${files.length} files, all servable`)
