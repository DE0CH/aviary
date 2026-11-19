<template>
  <oc-datepicker v-model="selectedDate" :label="$gettext('Expiry date')" />
  <div>Selected date: {{ selectedDate }}</div>
</template>

<script setup lang="ts">
import { ref } from 'vue'
import { DateTime } from 'luxon'

const selectedDate = ref<DateTime>()
</script>
