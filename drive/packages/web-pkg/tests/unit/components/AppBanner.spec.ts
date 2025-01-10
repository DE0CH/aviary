import { defaultComponentMocks, defaultPlugins, shallowMount } from '@opencloud-eu/web-test-helpers'
import AppBanner from '../../../src/components/AppBanner.vue'
import { createMemoryHistory, createRouter } from 'vue-router'
import { useLocalStorage, useSessionStorage } from '@vueuse/core'
import { ref } from 'vue'

vi.mock('@vueuse/core')

describe('AppBanner', () => {
  it('generates app url with correct app scheme', () => {
    const baseElement = document.createElement('base')
    baseElement.href = '/'
    document.getElementsByTagName('head')[0].appendChild(baseElement)
    delete window.location
    window.location = new URL('https://localhost') as any

    const { wrapper } = getWrapper({
      fileId: '1337',
      sessionStorageReturnValue: null
    })
    expect(wrapper.find('.app-banner-cta').attributes().href).toBe('opencloud://localhost/f/1337')
  })
  it('does not show when banner was closed', () => {
    const { wrapper } = getWrapper({
      fileId: '1337',
      sessionStorageReturnValue: '1'
    })
    expect(wrapper.find('.app-banner').attributes().hidden).toBe('')
  })

  it('shows when banner was not yet closed', () => {
    const { wrapper } = getWrapper({
      fileId: '1337',
      sessionStorageReturnValue: null
    })
    expect(wrapper.find('.app-banner').attributes().hidden).toBe(undefined)
  })
})

function getWrapper({
  fileId,
  sessionStorageReturnValue
}: {
  fileId: string
  sessionStorageReturnValue: string
}) {
  const router = createRouter({
    routes: [
      {
        path: '/f',
        component: {}
      }
    ],
    history: createMemoryHistory('/')
  })

  vi.mocked(useSessionStorage<string>).mockImplementation(() => {
    return ref(sessionStorageReturnValue)
  })
  vi.mocked(useLocalStorage<string>).mockImplementation(() => {
    return ref('')
  })

  const mocks = { ...defaultComponentMocks(), $router: router }

  return {
    wrapper: shallowMount(AppBanner, {
      props: {
        fileId
      },
      global: {
        plugins: [
          ...defaultPlugins({
            piniaOptions: {
              themeState: {
                currentTheme: {
                  appBanner: {
                    title: 'OpenCloud',
                    publisher: 'OpenCloud GmbH',
                    additionalInformation: '',
                    ctaText: 'OPEN',
                    icon: 'themes/opencloud/assets/opencloud-app-icon.png',
                    appScheme: 'opencloud'
                  }
                }
              }
            }
          })
        ],
        mocks,
        provide: mocks
      }
    })
  }
}
