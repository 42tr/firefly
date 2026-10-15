import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { apiFetch } from '@/utils/http'

// Shared selection rules for creation and existing conversations.
export function useCodexModels(agent = ref('codex')) {
  const providers = ref([])
  const officialModels = ref([])
  const modelReasoning = ref([])
  const modelsLoading = ref(false)
  const modelsError = ref('')
  const defaultProvider = ref('')
  const defaultModel = ref('')
  const defaultReasoningLevel = ref('')
  const provider = ref('')
  const model = ref('')
  const reasoningLevel = ref('')
  let providerRequest = 0
  const cancelModelLoad = () => { providerRequest += 1; modelsLoading.value = false }
  onBeforeUnmount(cancelModelLoad)

  const loadProviders = async (useDefault = true, preserveSelection = false) => {
    const request = ++providerRequest
    modelsLoading.value = true
    modelsError.value = ''
    try {
      const resp = await apiFetch('/api/assistant/codex/providers', { credentials: 'include' })
      if (!resp.ok) throw new Error('读取 API 平台失败')
      const data = await resp.json()
      if (request !== providerRequest) return
      providers.value = data.providers || []
      officialModels.value = data.official_models || []
      modelReasoning.value = data.model_reasoning || []
      modelsError.value = data.official_models_error || ''
      defaultProvider.value = data.default === 'openai' ? '' : data.default || ''
      defaultModel.value = data.default_model || ''
      defaultReasoningLevel.value = data.default_reasoning_level || ''
      if (useDefault && !provider.value) provider.value = defaultProvider.value
      if (!preserveSelection) {
        if (!model.value) selectModel()
        selectReasoningLevel()
      }
      return true
    } catch {
      if (request !== providerRequest) return
      providers.value = []
      officialModels.value = []
      modelReasoning.value = []
      modelsError.value = '读取模型列表失败，可手动输入模型名称或使用默认模型'
      defaultProvider.value = ''
      defaultModel.value = ''
      defaultReasoningLevel.value = ''
      if (!preserveSelection) selectReasoningLevel()
      return false
    } finally {
      if (request === providerRequest) modelsLoading.value = false
    }
  }

  const modelOptions = computed(() => {
    if (!provider.value) return officialModels.value
    const current = providers.value.find((item) => item.name === provider.value)
    return current && Array.isArray(current.models) ? current.models : []
  })

  // 平台模型列表读取失败时后端会返回 models_error；仅在选中该平台时暴露。
  const providerModelsError = computed(() => {
    if (!provider.value) return ''
    const current = providers.value.find((item) => item.name === provider.value)
    return current && typeof current.models_error === 'string' ? current.models_error.trim() : ''
  })

  const selectModel = () => {
    model.value = (provider.value === defaultProvider.value ? defaultModel.value : '')
      || modelOptions.value[0] || ''
  }

  const currentModelReasoning = computed(() => {
    const name = provider.value && model.value.startsWith(`${provider.value}/`)
      ? model.value.slice(provider.value.length + 1) : model.value
    return modelReasoning.value.find((item) => item.model === model.value || item.model === name)
  })
  const reasoningOptions = computed(() => {
    if (currentModelReasoning.value?.reasoning_levels) return currentModelReasoning.value.reasoning_levels
    // 未返回模型能力时，保留配置或原会话中的级别。
    return [...new Set(['low', 'medium', 'high', 'xhigh', defaultReasoningLevel.value, reasoningLevel.value].filter(Boolean))]
  })

  const selectReasoningLevel = () => {
    const options = reasoningOptions.value
    if (!options.length) {
      reasoningLevel.value = ''
      return
    }
    if (options.includes(reasoningLevel.value)) return
    const configured = provider.value === defaultProvider.value ? defaultReasoningLevel.value : ''
    reasoningLevel.value = [configured, currentModelReasoning.value?.default_reasoning_level, 'medium']
      .find((level) => options.includes(level)) || options[0]
  }

  watch(model, () => {
    if (agent.value === 'codex' && !modelsLoading.value) selectReasoningLevel()
  })

  const changeProvider = () => {
    // 平台切换后模型列表变化，避免残留其他平台的模型
    selectModel()
    selectReasoningLevel()
  }

  const resolveDefaults = () => {
    if (!model.value) selectModel()
    if (!reasoningLevel.value) selectReasoningLevel()
  }

  return {
    providers, modelsLoading, modelsError, defaultProvider, defaultModel,
    provider, model, reasoningLevel, loadProviders, modelOptions, providerModelsError,
    reasoningOptions, changeProvider, cancelModelLoad, resolveDefaults,
  }
}
