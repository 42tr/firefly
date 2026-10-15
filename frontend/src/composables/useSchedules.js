import { ref } from 'vue'
import { apiFetch } from '@/utils/http'
import { getAppTimezone } from './appTimezone.js'

export async function scheduleRequest(path = '', options = {}) {
  const response = await apiFetch(`/api/assistant/schedules${path}`, options)
  const data = await response.json()
  if (!response.ok) throw new Error(data.error || '定时任务请求失败')
  return data
}

export function useSchedules() {
  const schedules = ref([]), loading = ref(false), saving = ref(false), error = ref('')
  const timezone = ref(getAppTimezone())
  let revision = 0
  const load = async () => {
    if (saving.value) return
    const request = ++revision
    loading.value = true
    try {
      const data = await scheduleRequest()
      if (request !== revision) return
      schedules.value = data.schedules || []
      timezone.value = data.timezone || timezone.value
      error.value = ''
    } catch (err) {
      if (request === revision) error.value = err.message || '读取定时任务失败'
    } finally {
      if (request === revision) loading.value = false
    }
  }
  const mutate = async (id, method, body) => {
    if (saving.value) return null
    revision += 1
    loading.value = false
    saving.value = true
    error.value = ''
    try {
      const result = await scheduleRequest(id == null ? '' : `/${id}`, {
        method, headers: { 'Content-Type': 'application/json' },
        ...(body ? { body: JSON.stringify(body) } : {}),
      })
      if (method === 'DELETE') schedules.value = schedules.value.filter(item => item.id !== id)
      else schedules.value = [result, ...schedules.value.filter(item => item.id !== result.id)].sort((a, b) => b.id - a.id)
      return result
    } catch (err) {
      error.value = err.message || '保存定时任务失败'
      return null
    } finally { saving.value = false }
  }
  return { schedules, timezone, loading, saving, error, load, mutate }
}
