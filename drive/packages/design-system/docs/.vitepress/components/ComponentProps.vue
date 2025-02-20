<template>
  <div v-if="isLoading" class="oc-flex oc-flex-center">
    <oc-spinner aria-label="Loading properties" />
  </div>
  <div v-else>
    <oc-table :fields="fields" :data="props" class="props-table">
      <template #type="{ item }">
        <code> {{ item.type }}</code>
      </template>
      <template #description="{ item }">
        <span v-html="item.description.replace(/`([^`]+)`/g, '<code>$1</code>')" />
        <span v-if="item.default">
          Defaults to <code>{{ item.default }}</code
          >.</span
        >
      </template>
    </oc-table>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, Ref, unref } from 'vue'
import { Prop } from '../generateJsonMetaData'
import { useComponentName } from '../composables/useComponentName'
import { useLoadMetaData } from '../composables'

const { component } = defineProps<{ component?: string }>()

const { componentName } = useComponentName()
const { loadProps } = useLoadMetaData()

const fields = [
  {
    name: 'name',
    title: 'Name'
  },
  {
    name: 'required',
    title: 'Required'
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

const props: Ref<Prop[]> = ref([])
const isLoading = ref(true)

onMounted(async () => {
  props.value = await loadProps(component || unref(componentName))
  isLoading.value = false
})
</script>
