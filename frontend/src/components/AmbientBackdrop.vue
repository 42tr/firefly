<template>
  <div class="ambient-backdrop" aria-hidden="true"></div>
</template>

<style scoped>
.ambient-backdrop {
  position: fixed;
  inset: 0;
  z-index: 0;
  pointer-events: none;
  background: var(--bg-body);
}
</style>
