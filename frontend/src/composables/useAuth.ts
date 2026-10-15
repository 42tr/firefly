import { ref, onMounted } from 'vue'

export interface UseAuthOptions {
  pingUrl?: string
  autoCheck?: boolean
}

export function useAuth(options: UseAuthOptions = {}) {
  const isReady = ref(false)
  const isChecking = ref(true)
  const isSubmitting = ref(false)
  const error = ref('')

  async function check() {
    isChecking.value = true
    error.value = ''
    try {
      const response = await fetch(options.pingUrl ?? '/api/assistant/ping', { credentials: 'include' })
      isReady.value = response.ok
      if (!response.ok && response.status !== 401) error.value = '暂时无法连接服务，请重试'
    } catch {
      isReady.value = false
      error.value = '暂时无法连接服务，请重试'
    } finally {
      isChecking.value = false
    }
  }

  async function login(password: string): Promise<boolean> {
    if (isSubmitting.value) return false
    isSubmitting.value = true
    error.value = ''
    try {
      const response = await fetch('/auth/', {
        method: 'POST', credentials: 'include', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ password }),
      })
      if (!response.ok) {
        if (response.status === 429) {
          const seconds = Number(response.headers.get('Retry-After'))
          const minutes = Number.isFinite(seconds) && seconds > 0 ? Math.ceil(seconds / 60) : 60
          error.value = `登录失败次数过多，该 IP 已封禁，请在 ${minutes} 分钟后重试`
        } else {
          error.value = response.status === 401 ? '访问密码错误' : '登录失败，请稍后重试'
        }
        return false
      }
      isReady.value = true
      return true
    } catch {
      error.value = '暂时无法连接服务，请重试'
      return false
    } finally {
      isSubmitting.value = false
    }
  }

  async function logout(): Promise<boolean> {
    try {
      const response = await fetch('/auth/logout', { method: 'POST', credentials: 'include' })
      if (!response.ok && response.status !== 401) {
        error.value = '退出失败，请重试'
        return false
      }
      isReady.value = false
      window.location.replace('/')
      return true
    } catch {
      error.value = '暂时无法连接服务，请重试'
      return false
    }
  }

  if (options.autoCheck) onMounted(check)
  return { isReady, isChecking, isSubmitting, error, check, login, logout }
}

/** Keep return paths inside this instance, including when the URL was edited by hand. */
export function loginReturnPath(candidate: string | null): string {
  if (!candidate || candidate.startsWith('//') || /[\\\u0000-\u001f\u007f]/.test(candidate)) return '/'
  try {
    const target = new URL(candidate, window.location.origin)
    if (target.origin !== window.location.origin || target.username || target.password || /^\/auth(?:\/|$)/.test(target.pathname)) return '/'
    return target.pathname + target.search + target.hash
  } catch { return '/' }
}
