import { OpenCloudServer } from './types'
import { ClientService } from '@opencloud-eu/web-pkg'
import { urlJoin } from '@opencloud-eu/web-client'

interface OpenCloudInstancesResponse {
  subject: string
  links: OpenCloudServer[]
}

const OPENCLOUD_REL = 'http://webfinger.opencloud/rel/server-instance'

export class WebfingerDiscovery {
  private serverUrl: string
  private clientService: ClientService

  constructor(serverUrl: string, clientService: ClientService) {
    this.serverUrl = serverUrl
    this.clientService = clientService
  }

  public async discoverOpenCloudServers(): Promise<OpenCloudServer[]> {
    const client = this.clientService.httpAuthenticated
    const url =
      urlJoin(this.serverUrl, '.well-known', 'webfinger') + `?resource=${encodeURI(this.serverUrl)}`
    const response = (await client.get<OpenCloudInstancesResponse>(url)).data
    return response.links.filter((o) => o.rel === OPENCLOUD_REL)
  }
}
