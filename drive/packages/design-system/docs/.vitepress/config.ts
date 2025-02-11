import { defineConfig } from 'vitepress'
import { searchForWorkspaceRoot } from 'vite'

const projectRootDir = searchForWorkspaceRoot(process.cwd())
const stripScssMarker = '/* STYLES STRIP IMPORTS MARKER */'

export default defineConfig({
  title: 'OpenCloud Design System',
  description: 'Design System for OpenCloud',
  base: '/',
  vite: {
    css: {
      preprocessorOptions: {
        scss: {
          additionalData: `
                @use "sass:math";
                @import "${projectRootDir}/packages/design-system/src/styles/styles";${stripScssMarker};
            `,
          silenceDeprecations: ['legacy-js-api', 'import']
        }
      }
    },
    plugins: [
      // TODO: remove when doc blocks have been removed
      {
        name: '@opencloud-eu/vite-plugin-docs',
        transform(src, id) {
          if (id.includes('type=docs')) {
            return {
              code: 'export default {}',
              map: null
            }
          }
        }
      },
      {
        name: '@opencloud-eu/vite-plugin-strip-css',
        transform(src, id) {
          if (id.endsWith('.vue') && !id.includes('node_modules') && src.includes('@extend')) {
            console.warn(
              'You are using @extend in your component. This is likely not working in your styles. Please use mixins instead.',
              id.replace(`${projectRootDir}/`, '')
            )
          }
          if (id.includes('lang.scss')) {
            const split = src.split(stripScssMarker)
            const newSrc = split[split.length - 1]

            return {
              code: newSrc,
              map: null
            }
          }
        }
      }
    ]
  },

  themeConfig: {
    logo: '', // TODO: add logo
    search: {
      provider: 'local'
    },
    sidebar: {
      '/': [
        {
          text: 'Design Tokens',
          items: [
            {
              text: 'Color palette',
              link: '/designTokens/colorPalette'
            },
            {
              text: 'Font sizes',
              link: '/designTokens/fontSizes'
            },
            {
              text: 'Spacing',
              link: '/designTokens/spacings'
            }
          ]
        },
        {
          text: 'Components',
          items: [
            {
              text: 'OcButton',
              link: '/components/OcButton'
            }
          ]
        }
      ]
    },
    socialLinks: [
      {
        icon: 'github',
        link: 'https://github.com/opencloud-eu/web/tree/main/packages/design-system'
      },
      {
        icon: 'matrix',
        link: 'https://app.element.io/#/room/#opencloud:matrix.org'
      }
    ]
  }
})
