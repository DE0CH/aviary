import DefaultTheme from 'vitepress/theme-without-fonts'
import * as components from './../../../src/components'
import * as directives from './../../../src/directives'
import './custom.scss'

export default {
  extends: DefaultTheme,
  enhanceApp({ app }) {
    import('./../../../src/utils/webFontLoader')

    // TODO: remove `|| c.name` when all components follow the script setup syntax
    Object.values(components).forEach((c) => app.component(c.__name || c.name, c))
    Object.values(directives).forEach((d) => app.directive(d.name, d))
  }
}
