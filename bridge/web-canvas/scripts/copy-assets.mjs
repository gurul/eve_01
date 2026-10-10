/* Copy tldraw's fonts, icons and translations next to the bundle, with their upstream names, and the licence they ship under.
   Stable names keep a rebuild's git diff down to what actually changed. */
import { cpSync, copyFileSync, mkdirSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const out = join(here, '../../src/eve_bridge/learning/web/canvas')
const assets = join(here, '../node_modules/@tldraw/assets')
mkdirSync(join(out, 'assets'), { recursive: true })
for (const folder of ['fonts', 'icons', 'translations', 'embed-icons']) {
	cpSync(join(assets, folder), join(out, 'assets', folder), { recursive: true })
}
/* The tldraw licence asks for a verbatim copy in any distribution of the software. The npm package carries only a link,
   so the full text is vendored here from the matching release tag. */
copyFileSync(join(here, '../TLDRAW-LICENSE.md'), join(out, 'TLDRAW-LICENSE.md'))
console.log('tldraw assets copied to', out)
