# Color Palette

<script setup lang="ts">
import { computed } from 'vue'
import designTokens from '../../src/assets/tokens/ods.json'

const tokens = computed(() => {
	return Object.values(designTokens).filter((token) => token.name.startsWith('oc-color-'))
})
</script>

The color palette comes with 5 different weights for each hue. These hues should be used purposefully to communicate how things function in the interface. Keep in mind that `vermilion` is only used in special cases like destructive actions and error messages.

<div class="colors">
	<div v-for="(prop, index) in tokens" :key="index" class="color" :class="prop.category">
		<div class="swatch" :style="{ backgroundColor: prop.value }" />
		<h4>{{ prop.name }}</h4>
		<span v-for="(v, k) in prop.info" :key="k">
			<em>{{ k }}:</em>
			{{ v }}
		</span>
		<span>
			<em>scss:</em>
			{{ prop.name }}
		</span>
		<span>
			<em>css:</em>
			var(--{{ prop.name }})
		</span>
	</div>
</div>

<style lang="scss">
.colors {
  margin-top: 16px;
  display: block;
  width: 100%;
  @supports (display: grid) {
    display: grid;
    max-width: 1200px;
    align-content: stretch;
    justify-content: left;
    grid-template-columns:
      calc(20% - 8px)
      calc(20% - 8px)
      calc(20% - 8px)
      calc(20% - 8px)
      calc(20% - 8px);
    grid-column-gap: 8px;
    @media (max-width: 1300px) {
      grid-template-columns:
        calc(25% - 8px)
        calc(25% - 8px)
        calc(25% - 8px)
        calc(25% - 8px);
    }
    @media (max-width: 1100px) {
      grid-template-columns:
        calc(33.333% - 8px)
        calc(33.333% - 8px)
        calc(33.333% - 8px);
    }
    @media (max-width: 900px) {
      grid-template-columns:
        calc(50% - 8px)
        calc(50% - 8px);
    }
    @media (max-width: 400px) {
      grid-template-columns: 100%;
    }
  }
}

.swatch {
  border-bottom: 1px solid rgba(0, 0, 0, 0.08);
  height: 48px;
  margin-left: 4px;
  margin-top: 4px;
  width: calc(100% + 16px);
  float: left;
}

h3 {
  text-transform: capitalize;
  line-height: 1.2;
  width: 100%;
  float: left;
}

.color {
  -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale;
  margin-bottom: 8px;
  box-shadow:
    0 0 0 1px rgba(63, 63, 68, 0.05),
    0 1px 3px 0 rgba(63, 63, 68, 0.15);
  overflow: hidden;
  text-align: left;
  @supports (display: grid) {
    width: 100%;
    float: left;
  }
  @media (max-width: 400px) {
    margin-bottom: 8px;
  }

  span {
    margin-bottom: 4px;
    line-height: 1.3;
    width: 100%;
    float: left;

    em {
      user-select: none;
      font-style: normal;
    }
  }
}
</style>
