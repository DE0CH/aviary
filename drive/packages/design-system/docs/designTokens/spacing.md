# Spacing

<script setup lang="ts">
import { computed } from 'vue'
import designTokens from '../../src/assets/tokens/ods.json'

const tokens = computed(() => {
	return Object.values(designTokens).filter((token) => token.name.startsWith('oc-space'))
})
</script>

A framework for creating a predictable and harmonious spacing system. These tokens are used for padding, margins, and position.

<div class="spacing">
	<div
		v-for="(prop, index) in tokens"
		:key="index"
		class="space"
		:style="{ lineHeight: prop.value, height: prop.value }"
	>
		{{ prop.name.replace(/_/g, '-') }}
		<span>({{ prop.value }})</span>
	</div>
</div>

<style lang="scss">
.spacing {
  overflow: hidden;
  max-width: 1176px;
  width: 100%;
}
.space {
  -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale;
  background: #c4cdd5;
  text-align: center;
  position: relative;
  float: left;
  width: 100%;
  margin: 8px 0;
  span {
    margin-left: 5px;
    user-select: none;
    font-style: normal;
  }
}
</style>
