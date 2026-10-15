import { nextTick } from 'vue'

// ECharts 按需异步加载（模块级单例 Promise，首次使用时加载，后续复用）
let echartsPromise = null

const loadEcharts = () => {
  if (!echartsPromise) {
    echartsPromise = import('./echartsRuntime.js').then(module => ({
      api: module.default,
      ensureCharts: module.ensureCharts,
    }))
      .catch((err) => {
        echartsPromise = null
        throw err
      })
  }
  return echartsPromise
}

// 所有图表实例（模块级共享，便于统一 dispose / resize）
const instances = new Map()

const handleResize = () => {
  instances.forEach((chart) => chart.resize())
}

const bindResizeListener = () => {
  if (instances.size === 1) {
    window.addEventListener('resize', handleResize)
  }
}

const disposeAllCharts = () => {
  instances.forEach((chart) => chart.dispose())
  instances.clear()
  window.removeEventListener('resize', handleResize)
}

// 消息被分页替换、工具气泡被账单/图表气泡替换时，旧容器从 DOM 移除但实例仍
// 留在 Map 里。每次渲染前修剪掉已断开连接的实例，避免长时间会话内累积泄漏。
const pruneDisconnectedCharts = () => {
  instances.forEach((chart, container) => {
    if (!container.isConnected) {
      chart.dispose()
      instances.delete(container)
    }
  })
  if (instances.size === 0) {
    window.removeEventListener('resize', handleResize)
  }
}

export function useEcharts() {
  const renderChart = async (chartId, option) => {
    await nextTick()
    const container = document.getElementById(chartId)
    if (!container) return
    let echarts
    try {
      const runtime = await loadEcharts()
      await runtime.ensureCharts(option)
      echarts = runtime.api
    } catch (err) {
      console.warn('ECharts 加载失败', err)
      return
    }
    // 容器可能已不存在（等待加载期间切换了对话）
    if (!container.isConnected) return
    pruneDisconnectedCharts()
    let chart = instances.get(container) || echarts.getInstanceByDom(container)
    if (!chart) {
      chart = echarts.init(container, 'v5')
      instances.set(container, chart)
      bindResizeListener()
    }
    chart.setOption(option, true)
    chart.resize()
  }

  const pickEchartsPayload = (result) => {
    if (!result) return null
    if (result.type === 'echarts' && result.option) return result
    if (result.echarts_option) return { type: 'echarts', option: result.echarts_option, title: result.title }
    if (result.option && result.series) return { type: 'echarts', option: result, title: result.title }
    return null
  }

  const extractEchartsFromTool = (payload) => {
    if (!payload || !payload.result) return null
    return pickEchartsPayload(payload.result)
  }

  const extractEchartsFromText = (text) => {
    if (!text) return null
    const trimmed = text.trim()
    if (!trimmed) return null

    const tryParse = (raw) => {
      try {
        const parsed = JSON.parse(raw)
        return pickEchartsPayload(parsed)
      } catch (err) {
        return null
      }
    }

    let parsed = tryParse(trimmed)
    if (parsed) return parsed

    const fenceMatch = trimmed.match(/```(?:json)?\s*([\s\S]*?)\s*```/i)
    if (fenceMatch) {
      parsed = tryParse(fenceMatch[1].trim())
      if (parsed) return parsed
    }

    const firstBrace = trimmed.indexOf('{')
    const lastBrace = trimmed.lastIndexOf('}')
    if (firstBrace !== -1 && lastBrace > firstBrace) {
      parsed = tryParse(trimmed.slice(firstBrace, lastBrace + 1))
    }
    return parsed
  }

  const resolveChartTitle = (payload, fallback) => {
    if (payload && typeof payload.title === 'string' && payload.title.trim()) return payload.title.trim()
    const optionTitle = payload?.option?.title
    if (typeof optionTitle === 'string' && optionTitle.trim()) return optionTitle.trim()
    if (optionTitle && typeof optionTitle.text === 'string' && optionTitle.text.trim()) return optionTitle.text.trim()
    return fallback || '图表'
  }

  return {
    renderChart,
    disposeAllCharts,
    extractEchartsFromTool,
    extractEchartsFromText,
    resolveChartTitle,
  }
}
