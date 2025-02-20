import DefaultTheme from 'vitepress/theme-without-fonts'
import { createGettext } from 'vue3-gettext'
import * as components from './../../../src/components'
import * as directives from './../../../src/directives'
import './custom.scss'
import LiveCodeBlock from '../LiveCodeBlock.vue'
import ComponentEmits from '../components/ComponentEmits.vue'
import ComponentProps from '../components/ComponentProps.vue'
import ComponentSlots from '../components/ComponentSlots.vue'

export default {
  extends: DefaultTheme,
  enhanceApp({ app }) {
    const gettext = createGettext()
    app.use(gettext)

    app.component('LiveCodeBlock', LiveCodeBlock)
    app.component('ComponentEmits', ComponentEmits)
    app.component('ComponentProps', ComponentProps)
    app.component('ComponentSlots', ComponentSlots)
    Object.values(components).forEach((c) => app.component(c.__name, c))
    Object.values(directives).forEach((d) => app.directive(d.name, d))
  }
}
