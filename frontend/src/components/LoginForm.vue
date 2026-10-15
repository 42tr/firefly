<script setup>
import { ref } from 'vue'
const props = defineProps({ checking: Boolean, submitting: Boolean, error: String })
const emit = defineEmits(['login', 'retry'])
const password = ref('')
const submit = () => {
  if (!password.value || props.submitting) return
  emit('login', password.value)
  password.value = ''
}
</script>

<template>
  <section class="login-card" aria-labelledby="login-title">
    <img class="login-brand" src="/assistant-icon.svg" alt="" />
    <h1 id="login-title">流萤</h1>
    <p v-if="checking" role="status">正在校验登录…</p>
    <form v-else @submit.prevent="submit">
      <p>输入访问密码，进入你的工作区</p>
      <label for="access-password">访问密码</label>
      <input id="access-password" v-model="password" type="password" name="password" autocomplete="current-password" required autofocus :disabled="submitting" :aria-invalid="!!error" :aria-describedby="error ? 'login-error' : undefined" />
      <p v-if="error" id="login-error" class="login-error" role="alert">{{ error }}</p>
      <button class="button primary" type="submit" :disabled="submitting || !password">{{ submitting ? '正在登录…' : '进入流萤' }}</button>
      <button v-if="error === '暂时无法连接服务，请重试'" class="button ghost" type="button" @click="emit('retry')">重新连接</button>
    </form>
  </section>
</template>

<style scoped>
.login-card { width: min(360px, 100%); margin: auto; padding: 32px; border: 1px solid var(--accent-border); border-radius: 20px; background: var(--bg-panel, #fff); box-shadow: 0 12px 48px rgba(23, 32, 51, .08); }
.login-brand { display: block; width: 48px; height: 48px; margin-bottom: 16px; }
h1 { margin: 0 0 8px; font-size: 26px; color: var(--ink); }
p { margin: 0; color: var(--muted); font-size: 13px; }
form { display: flex; flex-direction: column; gap: 16px; }
label { margin-bottom: -8px; font-size: 13px; color: var(--text-primary); }
input { width: 100%; min-height: 44px; padding: 10px 12px; border: 1px solid var(--accent-border); border-radius: 10px; background: var(--bg-body); font: inherit; }
input:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
.login-error { color: var(--danger, #b42318); }
button { min-height: 44px; justify-content: center; }
</style>
