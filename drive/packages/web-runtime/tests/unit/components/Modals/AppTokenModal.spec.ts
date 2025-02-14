import AppTokenModal from '../../../../src/components/Modals/AppTokenModal.vue'
import {
  defaultComponentMocks,
  defaultPlugins,
  mockAxiosResolve,
  shallowMount
} from '@opencloud-eu/web-test-helpers'
import { mockDeep } from 'vitest-mock-extended'
import { ClientService } from '@opencloud-eu/web-pkg'
import { OcButton, OcDatepicker } from '@opencloud-eu/design-system/components'
import { DateTime } from 'luxon'

const copyMock = vi.fn()
vi.mock('@vueuse/core', () => ({
  useClipboard: vi.fn(() => ({ copy: copyMock, copied: false }))
}))

describe('AppTokenModal component', () => {
  it('should display an input for the expiration date', () => {
    const { wrapper } = getWrapper()
    expect(wrapper.find('oc-datepicker-stub').exists()).toBeTruthy()
  })
  describe('confirm button', () => {
    it('should be disabled when no date has been entered', () => {
      const { wrapper } = getWrapper()
      const btn = wrapper.findComponent<typeof OcButton>('.oc-modal-body-actions-confirm')
      expect(btn.props('disabled')).toBeTruthy()
    })
    it('should not be disabled when a date has been entered', async () => {
      const { wrapper } = getWrapper()
      wrapper
        .findComponent<typeof OcDatepicker>('oc-datepicker-stub')
        .vm.$emit('dateChanged', { date: DateTime.now(), error: null })
      const btn = wrapper.findComponent<typeof OcButton>('.oc-modal-body-actions-confirm')
      await wrapper.vm.$nextTick()
      expect(btn.props('disabled')).toBeFalsy()
    })
    it('should create a token on submit', async () => {
      const { wrapper, mocks } = getWrapper()
      wrapper
        .findComponent<typeof OcDatepicker>('oc-datepicker-stub')
        .vm.$emit('dateChanged', { date: DateTime.now(), error: null })
      const btn = wrapper.findComponent<typeof OcButton>('.oc-modal-body-actions-confirm')
      await wrapper.vm.$nextTick()
      await btn.trigger('click')
      expect(mocks.$clientService.httpAuthenticated.post).toHaveBeenCalled()
    })
  })
  it('should display the created token', async () => {
    const { wrapper } = getWrapper()
    wrapper
      .findComponent<typeof OcDatepicker>('oc-datepicker-stub')
      .vm.$emit('dateChanged', { date: DateTime.now(), error: null })
    const btn = wrapper.findComponent<typeof OcButton>('.oc-modal-body-actions-confirm')
    await wrapper.vm.$nextTick()
    await btn.trigger('click')
    expect(wrapper.find('.created-token').exists()).toBeTruthy()
  })
  it('the created token can be copied', async () => {
    const { wrapper } = getWrapper()
    wrapper
      .findComponent<typeof OcDatepicker>('oc-datepicker-stub')
      .vm.$emit('dateChanged', { date: DateTime.now(), error: null })
    const btn = wrapper.findComponent<typeof OcButton>('.oc-modal-body-actions-confirm')
    await wrapper.vm.$nextTick()
    await btn.trigger('click')
    await wrapper.find('.copy-app-token-btn').trigger('click')
    expect(copyMock).toHaveBeenCalled()
  })
})

const getWrapper = () => {
  const clientService = mockDeep<ClientService>()
  clientService.httpAuthenticated.post.mockResolvedValue(mockAxiosResolve({ token: 'token' }))
  const mocks = { ...defaultComponentMocks(), $clientService: clientService }

  return {
    mocks,
    wrapper: shallowMount(AppTokenModal, {
      global: {
        mocks,
        provide: mocks,
        plugins: [...defaultPlugins()]
      }
    })
  }
}
