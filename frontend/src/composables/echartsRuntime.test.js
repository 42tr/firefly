import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'
import test, { after } from 'node:test'
import { createServer } from 'vite'
import { SVGRenderer } from 'echarts/renderers.js'

const server = await createServer({ configFile: fileURLToPath(new URL('../../vite.config.ts', import.meta.url)), server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom' })
after(() => server.close())
const { default: echarts, ensureCharts } = await server.ssrLoadModule('/src/composables/echartsRuntime.js')
echarts.use(SVGRenderer)

test('upgraded runtime registers all supported charts for concurrent callers and preserves the v5 theme', async () => {
  for (const type of ['line', 'bar', 'pie', 'scatter']) {
    const option = {
      title: { text: `chart-${type}` },
      animation: false,
      ...(type === 'pie' ? {} : { xAxis: { type: 'category', data: ['a', 'b'] }, yAxis: {} }),
      ...(type === 'line' ? { dataZoom: [{ type: 'inside' }] } : {}),
      series: [{ type, data: [{ name: 'a', value: 1 }, { name: 'b', value: 2 }] }],
    }
    // The second caller must not finish before the first import registers.
    await Promise.all([ensureCharts(option), ensureCharts(option)])
    const chart = echarts.init(null, 'v5', { renderer: 'svg', ssr: true, width: 600, height: 300 })
    try {
      chart.setOption(option)
      const svg = chart.renderToSVGString()
      assert.match(svg, new RegExp(`chart-${type}`))
      assert.match(svg, /<path/)
      assert.equal(chart.getOption().color[0], '#5470c6')
    } finally { chart.dispose() }
  }
})
