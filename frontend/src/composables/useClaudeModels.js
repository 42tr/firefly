import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { apiFetch } from '@/utils/http'

export function useClaudeModels() {
  const model = ref('')
  const provider = ref('')
  const reasoningLevel = ref('')
  const providers = ref([])
  const cliModels = ref([])
  const cliDefaultModel = ref('')
  const defaultProvider = ref('cli')
  const cliDefaultReasoningLevel = ref('')
  const cliReasoningOptions = ref(['low', 'medium', 'high', 'xhigh', 'max'])
  const usingCli = computed(() => (provider.value || defaultProvider.value) === 'cli')
  const currentProvider = computed(() => providers.value.find(item => item.name === (provider.value || defaultProvider.value)))
  const modelOptions = computed(() => currentProvider.value?.models || (usingCli.value ? cliModels.value : []))
  const defaultModel = computed(() => currentProvider.value
    ? currentProvider.value.default_model || 'default' : usingCli.value ? cliDefaultModel.value : '')
  const defaultReasoningLevel = computed(() => currentProvider.value
    ? currentProvider.value.default_reasoning_level || '' : usingCli.value ? cliDefaultReasoningLevel.value : '')
  const currentModelReasoning = computed(() => currentProvider.value?.model_reasoning?.find(item => item.model === model.value))
  const reasoningOptions = computed(() => currentProvider.value?.source === 'ui'
    ? (currentModelReasoning.value?.reasoning_levels || []).filter(level => cliReasoningOptions.value.includes(level))
    : cliReasoningOptions.value)
  const modelsLoading = ref(false)
  const modelsError = ref('')
  let request = 0
  const cancelModelLoad = () => { request += 1; modelsLoading.value = false }
  onBeforeUnmount(cancelModelLoad)
  const loadModels = async () => {
    const current = ++request
    modelsLoading.value = true
    modelsError.value = ''
    try {
      const response = await apiFetch('/api/assistant/claude/models', { credentials: 'include' })
      const data = await response.json()
      if (!response.ok) throw new Error(data.error || '读取 Claude 配置失败')
      if (current !== request) return
      providers.value = Array.isArray(data.providers) ? data.providers : []
      const canonical = providers.value.find(item => item.aliases?.includes(provider.value))
      if (canonical) provider.value = canonical.name
      defaultProvider.value = data.default || 'cli'
      cliModels.value = Array.isArray(data.models) ? data.models : []
      cliDefaultModel.value = data.cli_default_model || data.default_model || ''
      cliDefaultReasoningLevel.value = data.default_reasoning_level || ''
      modelsError.value = data.models_error || ''
      cliReasoningOptions.value = data.reasoning_levels || ['low', 'medium', 'high', 'xhigh', 'max']
      if (!provider.value) provider.value = defaultProvider.value
      if (!model.value || model.value === 'default') selectModel()
      selectReasoningLevel()
      return true
    } catch (error) {
      if (current !== request) return
      providers.value = []
      cliModels.value = []
      cliDefaultModel.value = ''
      modelsError.value = `${error.message || '读取配置失败'}；可重试读取，或保留当前平台并输入模型名称`
      return false
    } finally {
      if (current === request) modelsLoading.value = false
    }
  }
  const selectModel = () => {
    const configured = defaultModel.value
    model.value = configured && configured !== 'default' ? configured : modelOptions.value[0] || ''
  }
  const selectReasoningLevel = () => {
    const levels = reasoningOptions.value
    if (levels.includes(reasoningLevel.value)) return
    reasoningLevel.value = [currentModelReasoning.value?.default_reasoning_level, defaultReasoningLevel.value, 'medium']
      .find(level => levels.includes(level)) || levels[0] || ''
  }
  const changeProvider = () => {
    selectModel()
    reasoningLevel.value = ''
    selectReasoningLevel()
  }
  watch([model, provider], () => {
    if (!modelsLoading.value) selectReasoningLevel()
  })
  return { model, provider, reasoningLevel, providers, defaultProvider, defaultReasoningLevel,
    reasoningOptions, modelOptions, defaultModel, modelsLoading, modelsError, loadModels, cancelModelLoad, changeProvider }
}
