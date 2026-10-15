import * as echarts from 'echarts/core'
import { GridComponent, TooltipComponent, LegendComponent } from 'echarts/components'
import { CanvasRenderer } from 'echarts/renderers'
import 'echarts/theme/v5'

// Common infrastructure is shared by every chart. Chart types are loaded from
// their individual ECharts entry points only when an option actually uses them.
echarts.use([GridComponent, TooltipComponent, LegendComponent, CanvasRenderer])

const chartLoaders = {
  line: () => import('echarts/lib/chart/line'),
  bar: () => import('echarts/lib/chart/bar'),
  pie: () => import('echarts/lib/chart/pie'),
  scatter: () => import('echarts/lib/chart/scatter'),
}
const modulePromises = new Map()
const componentLoaders = {
  title: () => import('echarts/lib/component/title'),
  dataZoom: () => import('echarts/lib/component/dataZoom'),
}

export async function ensureCharts(option) {
  const series = Array.isArray(option?.series) ? option.series : [option?.series]
  const types = new Set(series.filter(Boolean).map((item) => item.type || 'line'))
  const chartTasks = [...types].map((type) => {
    const load = chartLoaders[type] || chartLoaders.line
    return { type, load }
  })
  const componentTasks = []
  if (option?.title) componentTasks.push({ type: 'title', load: componentLoaders.title })
  if (option?.dataZoom) componentTasks.push({ type: 'dataZoom', load: componentLoaders.dataZoom })
  await Promise.all([...chartTasks, ...componentTasks].map(({ type, load }) => {
    // Concurrent charts must await registration, not just see a "loaded" flag
    // set before the dynamic import finishes.
    if (!modulePromises.has(type)) {
      modulePromises.set(type, load().catch((error) => {
        modulePromises.delete(type)
        throw error
      }))
    }
    return modulePromises.get(type)
  }))
}

export default echarts
