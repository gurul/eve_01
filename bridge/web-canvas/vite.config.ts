import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

/* One script and one stylesheet with fixed names, written into the Python package that serves them.
   eve's learning server has no build step of its own: it serves whatever this build leaves in web/canvas. */
export default defineConfig({
	plugins: [react()],
	base: '/canvas/',
	publicDir: false,
	build: {
		outDir: '../src/eve_bridge/learning/web/canvas',
		emptyOutDir: true,
		cssCodeSplit: false,
		rollupOptions: {
			input: 'src/main.tsx',
			output: {
				inlineDynamicImports: true,
				entryFileNames: 'canvas.js',
				assetFileNames: 'canvas.[ext]',
			},
		},
	},
})
