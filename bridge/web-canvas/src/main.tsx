/* eve's whiteboard: tldraw, mounted as an island inside the plain-JS lesson page (../src/eve_bridge/learning/web/app.js).
   The page owns lessons, saving and the tutor; this file owns only the board. They meet at window.EveCanvas. */
import { getAssetUrls } from '@tldraw/assets/selfHosted'
import { createRoot } from 'react-dom/client'
import {
	AssetRecordType,
	Editor,
	TLComponents,
	TLEditorSnapshot,
	Tldraw,
	getSnapshot,
	loadSnapshot,
} from 'tldraw'
import './index.css'

type BoardDocument = TLEditorSnapshot['document']

/** The flattened picture of a board saved before tldraw: pen, eraser and text strokes exactly as the learner left them. */
interface LegacyBoard {
	image: string
	width: number
	height: number
}

interface MountOptions {
	licenseKey?: string
	/** The page's CSP nonce. tldraw stamps it on the <style> elements it adds (licence watermark, fonts in a board picture). */
	nonce?: string
	/** Called after the learner changes the board. Never called for a load. */
	onChange(): void
}

export interface BoardHandle {
	load(board: BoardDocument | null, legacy?: LegacyBoard | null): void
	snapshot(): BoardDocument
	image(): Promise<string>
	shapeCount(): number
	setReadOnly(readOnly: boolean): void
	isInteracting(): boolean
	unmount(): void
}

/* Fonts, icons and translations are served by eve itself (the page CSP is same-origin only). */
const assetUrls = getAssetUrls({ baseUrl: '/canvas/assets' })

/* One board per lesson, so no page menu. The debug and help panels point at tldraw, not at the lesson. */
const components: TLComponents = { PageMenu: null, HelpMenu: null, DebugPanel: null, DebugMenu: null, SharePanel: null }

/* The tutor reads this image; larger boards are scaled down to this, never up past 2x. */
const MAX_IMAGE_SIDE = 1600

function blobToDataUrl(blob: Blob): Promise<string> {
	return new Promise((resolve, reject) => {
		const reader = new FileReader()
		reader.onload = () => resolve(String(reader.result))
		reader.onerror = () => reject(reader.error)
		reader.readAsDataURL(blob)
	})
}

function handleFor(editor: Editor, options: MountOptions, unmount: () => void): BoardHandle {
	const empty = getSnapshot(editor.store).document
	/* A load changes the document too. Its store events arrive after load() returns, so they are counted out by frame. */
	let loading = 0
	/* Read-only mode puts tldraw on its hand tool. The learner's tool comes back when the board unlocks (after eve answers). */
	let toolBeforeLock: string | null = null
	const stopListening = editor.store.listen(() => { if (!loading) options.onChange() }, { source: 'user', scope: 'document' })
	const settle = () => {
		loading++
		requestAnimationFrame(() => requestAnimationFrame(() => { loading-- }))
	}
	return {
		load(board, legacy) {
			settle()
			/* The last lesson may have left the editor read-only, and a read-only editor creates no shapes. The page locks it again after. */
			editor.updateInstanceState({ isReadonly: false })
			toolBeforeLock = null
			editor.run(() => {
				loadSnapshot(editor.store, { document: board ?? empty })
				if (!board && legacy?.image) {
					const assetId = AssetRecordType.createId()
					editor.createAssets([{
						id: assetId, typeName: 'asset', type: 'image', meta: {},
						props: { name: 'saved-board.png', src: legacy.image, w: legacy.width, h: legacy.height, mimeType: 'image/png', isAnimated: false },
					}])
					editor.createShape({ type: 'image', x: 0, y: 0, isLocked: true, props: { assetId, w: legacy.width, h: legacy.height } })
				}
			}, { history: 'ignore', ignoreShapeLock: true })
			editor.clearHistory()
			editor.setCurrentTool('draw')
			if (editor.getCurrentPageShapeIds().size) editor.zoomToFit({ immediate: true })
			else editor.resetZoom()
		},
		snapshot: () => getSnapshot(editor.store).document,
		async image() {
			const ids = [...editor.getCurrentPageShapeIds()]
			const bounds = editor.getCurrentPageBounds()
			if (!ids.length || !bounds) return ''
			const scale = Math.min(2, MAX_IMAGE_SIDE / Math.max(bounds.w, bounds.h, 1))
			const { blob } = await editor.toImage(ids, { format: 'png', background: true, padding: 32, scale, pixelRatio: 1, darkMode: false })
			return blobToDataUrl(blob)
		},
		shapeCount: () => editor.getCurrentPageShapeIds().size,
		setReadOnly(readOnly) {
			if (editor.getInstanceState().isReadonly === readOnly) return
			if (readOnly) toolBeforeLock = editor.getCurrentToolId()
			editor.updateInstanceState({ isReadonly: readOnly })
			if (!readOnly && toolBeforeLock) editor.setCurrentTool(toolBeforeLock)
		},
		isInteracting: () => editor.inputs.getIsPointing() || editor.getEditingShapeId() !== null,
		unmount() { stopListening(); unmount() },
	}
}

function mount(host: HTMLElement, options: MountOptions): Promise<BoardHandle> {
	return new Promise(resolve => {
		const root = createRoot(host)
		root.render(
			<Tldraw
				assetUrls={assetUrls}
				components={components}
				licenseKey={options.licenseKey || undefined}
				options={{ nonce: options.nonce || undefined }}
				autoFocus={false}
				colorScheme="light"
				maxAssetSize={2 * 1024 * 1024}
				maxImageDimension={MAX_IMAGE_SIDE}
				onMount={editor => { resolve(handleFor(editor, options, () => root.unmount())) }}
			/>
		)
	})
}

declare global {
	interface Window { EveCanvas: { mount: typeof mount } }
}
window.EveCanvas = { mount }
