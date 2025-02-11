# Font Sizes

<script setup lang="ts">
import { computed } from 'vue'
import designTokens from '../../src/assets/tokens/ods.json'

const tokens = computed(() => {
	return Object.values(designTokens).filter((token) => token.name.startsWith('oc-font-size-'))
})
</script>

This typographic scale makes it easier to achieve visual harmony in the interface. It’s purposefully designed to keep the number of separate font sizes to a minimum.

<div class="font-sizes">
	<div
		v-for="(prop, index) in tokens"
		:key="index"
		class="font"
		:style="{ fontSize: prop.value }"
	>
		{{ prop.name }}, var(--{{ prop.name }})
		<span>({{ prop.value }})</span>
	</div>
</div>

<style lang="scss">
.font-sizes {
  margin-top: 16px;
  overflow: hidden;
  width: 100%;
}
.font {
  margin-bottom: 4px;
  font-style: normal;
  span {
    letter-spacing: -0.02em;
    margin-left: 10px;
    user-select: none;
    font-style: normal;
  }
}
</style>
