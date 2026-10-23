<script setup>
// Additional icons use official Feather (MIT) and Lucide (ISC) assets, matching the existing set.
import chevron_down from '../assets/icons/chevron-down.svg?raw'
import chevron_left from '../assets/icons/chevron-left.svg?raw'
import more_horizontal from '../assets/icons/more-horizontal.svg?raw'
import sliders from '../assets/icons/sliders.svg?raw'
import activity from '../assets/icons/activity.svg?raw'
import cloud from '../assets/icons/cloud.svg?raw'
import bar_chart_2 from '../assets/icons/bar-chart-2.svg?raw'
import check_square from '../assets/icons/check-square.svg?raw'
import settings from '../assets/icons/settings.svg?raw'
import puzzle from '../assets/icons/puzzle.svg?raw'
defineProps({ name: String })
const assets = { 'chevron-down': chevron_down, 'chevron-left': chevron_left, 'more-horizontal': more_horizontal, sliders, activity, cloud, 'bar-chart-2': bar_chart_2, 'check-square': check_square, settings, puzzle }
const iconMarkup = Object.fromEntries(Object.entries(assets).map(([name, svg]) => [name, svg.replace(/^.*?<svg[^>]*>/s, '').replace(/<\/svg>.*$/s, '')]))
const paths = {
  umbrella: 'M3 12a9 9 0 0 1 18 0H3Zm9-11v2m0 9v7a2 2 0 0 0 4 0',
  calendar: 'M8 2v4m8-4v4M3 10h18M5 4h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2Z',
  logout: 'M9 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h4M16 7l5 5-5 5M21 12H9',
  terminal: 'm5 6 6 6-6 6m8 0h6',
  compose: 'M10 3H6a3 3 0 0 0-3 3v12a3 3 0 0 0 3 3h12a3 3 0 0 0 3-3v-4M17 3a2.12 2.12 0 0 1 3 3L10 16l-4 1 1-4Z',
  sidebar: 'M6 3h12a3 3 0 0 1 3 3v12a3 3 0 0 1-3 3H6a3 3 0 0 1-3-3V6a3 3 0 0 1 3-3Zm3 0v18',
  search: 'M21 21l-4.35-4.35M19 10.5a8.5 8.5 0 1 1-17 0 8.5 8.5 0 0 1 17 0Z',
  chat: 'M21 11.5a8.38 8.38 0 0 1-.9 3.8 8.5 8.5 0 0 1-7.6 4.7 8.38 8.38 0 0 1-3.8-.9L3 21l1.9-5.7a8.38 8.38 0 0 1-.9-3.8 8.5 8.5 0 0 1 4.7-7.6 8.38 8.38 0 0 1 3.8-.9h.5a8.48 8.48 0 0 1 8 8Z',
  video: 'M4 3h16a1 1 0 0 1 1 1v16a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1Zm5 4 7 5-7 5Z',
  folder: 'M2 6a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2Z',
  file: 'M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8Zm0 0v6h6M8 13h8M8 17h5',
  attachment: 'm8 13 7-7a3 3 0 0 1 4 4l-9 9a5 5 0 0 1-7-7l9-9M6 15l8-8',
  tools: 'm8 7-5 5 5 5m8-10 5 5-5 5M14 4l-4 16',
  down: 'M12 4v16m-6-6 6 6 6-6',
  archive: 'M3 3h18v5H3ZM5 8v13h14V8M10 12h4',
  pin: 'M12 17v5M5 17h14l-1.5-1.5c-1-1-1.5-2.5-1.5-4V7l2-2V3H6v2l2 2v4.5c0 1.5-.5 3-1.5 4Z',
  target: 'M12 3v3m0 12v3M3 12h3m12 0h3M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8Zm0-5a9 9 0 1 0 0 18 9 9 0 0 0 0-18Z',
  pause: 'M8 5v14M16 5v14',
  play: 'm9 5 10 7-10 7V5Z',
  check: 'm5 12 4 4L19 6',
  x: 'M6 6l12 12M18 6 6 18',
  info: 'M12 16v-4m0-4h.01M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0Z',
  loader: 'M12 3a9 9 0 0 1 9 9',
}
</script>

<template>
  <svg v-if="iconMarkup[name]" class="ui-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" v-html="iconMarkup[name]"></svg>
  <svg v-else class="ui-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path :d="paths[name] || paths.file" /></svg>
</template>
