import { onBeforeUnmount, ref } from 'vue'
import { apiFetch } from '@/utils/http'

export function useClaudeModels() {
  const model = ref('')
  const modelOptions = ref([])
  const defaultModel = ref('')
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
      if (!response.ok) throw new Error('读取 Claude 模型失败')
      const data = await response.json()
      if (current !== request) return
      modelOptions.value = Array.isArray(data.models) ? data.models : []
      defaultModel.value = data.default_model || ''
      return true
    } catch {
      if (current !== request) return
      modelOptions.value = []
      defaultModel.value = ''
      modelsError.value = '读取模型选项失败，可输入模型名称或使用默认模型'
      return false
    } finally {
      if (current === request) modelsLoading.value = false
    }
  }
  return { model, modelOptions, defaultModel, modelsLoading, modelsError, loadModels, cancelModelLoad }
}
