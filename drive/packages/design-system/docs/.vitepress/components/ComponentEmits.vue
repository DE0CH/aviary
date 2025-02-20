<template>
  <div v-if="isLoading" class="oc-flex oc-flex-center">
    <oc-spinner aria-label="Loading emits" />
  </div>
  <div v-else>
    <oc-table :fields="fields" :data="emits" class="emits-table">
      <template #type="{ item }">
        <code> {{ item.type }}</code>
      </template>
      <template #description="{ item }">
        <span v-html="item.description.replace(/`([^`]+)`/g, '<code>$1</code>')" />
      </template>
    </oc-table>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, Ref, unref } from 'vue'
import { Emit } from '../generateJsonMetaData'
import { useComponentName } from '../composables/useComponentName'
import { useLoadMetaData } from '../composables'

const { component } = defineProps<{ component?: string }>()

const { componentName } = useComponentName()
const { loadEmits } = useLoadMetaData()

const fields = [
  {
    name: 'name',
    title: 'Name'
  },
  {
    name: 'type',
    title: 'Type',
    type: 'slot'
  },
  {
    name: 'description',
    title: 'Description',
    type: 'slot'
  }
]

const emits: Ref<Emit[]> = ref([])
const isLoading = ref(true)

onMounted(async () => {
  emits.value = await loadEmits(component || unref(componentName))
  isLoading.value = false
})
</script>
