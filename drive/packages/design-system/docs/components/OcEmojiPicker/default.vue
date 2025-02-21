<template>
  <oc-emoji-picker @emoji-select="updateSelectedEmoji" />
  <p>Selected emoji: {{ selectedEmoji }}</p>
</template>

<script setup lang="ts">
import { Emoji } from 'emoji-mart'
import { ref } from 'vue'

const selectedEmoji = ref<Emoji>()

const updateSelectedEmoji = (emoji: Emoji) => {
  selectedEmoji.value = emoji
}
</script>
