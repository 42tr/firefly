<script setup>
import UiIcon from './UiIcon.vue'
defineProps({ indicator: Object })
</script>

<template>
  <span v-if="indicator" :class="['module-indicator', indicator.tone]" role="img" :aria-label="indicator.label" :title="indicator.label" :data-module-id="indicator.module_id"><UiIcon :name="indicator.icon" /></span>
</template>

<style scoped>
.module-indicator { display:inline-flex; flex:0 0 auto; align-items:center; justify-content:center; width:19px; height:19px; border-radius:5px; color:#2362a2; background:#e9f2fc; vertical-align:middle; }
.module-indicator :deep(svg) { width:14px; height:14px; }
.warning { color:#956211; background:#fff4db; } .error { color:#b73b44; background:#fff0f1; } .success { color:#23764a; background:#e7f5ec; }
</style>
