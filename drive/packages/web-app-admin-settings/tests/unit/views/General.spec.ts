import General from '../../../src/views/General.vue'
import { defaultPlugins, shallowMount } from '@opencloud-eu/web-test-helpers'

describe('General view', () => {
  it('renders component', () => {
    const { wrapper } = getWrapper()
    expect(wrapper.html()).toMatchSnapshot()
  })
})

function getWrapper() {
  return {
    wrapper: shallowMount(General, {
      global: {
        plugins: [...defaultPlugins()]
      }
    })
  }
}
