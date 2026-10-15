/** 跳转本站密码登录页，登录完成后回到当前页面。 */
export function redirectToLogin(): void {
  window.location.href = '/auth/?url=' + encodeURIComponent(window.location.href)
}

/**
 * 原生 fetch 的统一封装：自动携带 cookie，401 时跳转登录页并抛出错误。
 * SSE 流式接口同样可用。
 */
export async function apiFetch(input: RequestInfo | URL, init: RequestInit = {}): Promise<Response> {
  const response = await fetch(input, { credentials: 'include', ...init })
  if (response.status === 401) {
    redirectToLogin()
    throw new Error('登录已过期，正在跳转登录页')
  }
  return response
}
