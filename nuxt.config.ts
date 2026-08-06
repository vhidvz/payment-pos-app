import tailwindcss from '@tailwindcss/vite'

// Tauri expects a fully static frontend; SSR is disabled and `nuxt generate`
// produces .output/public which tauri.conf.json points at via frontendDist.
export default defineNuxtConfig({
  compatibilityDate: '2026-08-01',
  ssr: false,
  devtools: { enabled: false },
  css: [
    '@fontsource-variable/fraunces',
    '@fontsource-variable/space-grotesk',
    '@fontsource-variable/jetbrains-mono',
    '@fontsource-variable/vazirmatn',
    '~/assets/css/main.css',
  ],
  vite: {
    plugins: [tailwindcss()],
    // Tauri CLI watches this output; keep the screen clean and ports stable.
    clearScreen: false,
    server: { strictPort: true },
    envPrefix: ['VITE_', 'TAURI_'],
  },
  devServer: { host: '127.0.0.1', port: 14373 },
  app: {
    pageTransition: { name: 'page', mode: 'out-in' },
    head: {
      title: 'Ledger — Payment POS Console',
      meta: [
        { name: 'viewport', content: 'width=device-width, initial-scale=1' },
        { name: 'color-scheme', content: 'dark' },
      ],
    },
  },
  telemetry: false,
})
