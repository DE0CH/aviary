import { User } from '../types'

export const dummyUserStore = new Map<string, User>([
  [
    'admin',
    {
      id: 'admin',
      username: 'admin',
      displayName: process.env.ADMIN_USERNAME || 'admin',
      password: process.env.ADMIN_PASSWORD || 'admin',
      email: 'admin@example.org'
    }
  ],
  [
    'alice',
    {
      id: 'alice',
      username: 'alice',
      displayName: 'Alice Hansen',
      password: '1234',
      email: 'alice@example.org'
    }
  ],
  [
    'brian',
    {
      id: 'brian',
      username: 'brian',
      displayName: 'Brian Murphy',
      password: '1234',
      email: 'brian@example.org'
    }
  ],
  [
    'carol',
    {
      id: 'carol',
      username: 'carol',
      displayName: 'Carol King',
      password: '1234',
      email: 'carol@example.org'
    }
  ],
  [
    'david',
    {
      id: 'david',
      username: 'david',
      displayName: 'David Goodall',
      password: '1234',
      email: 'david@example.org'
    }
  ],
  [
    'edith',
    {
      id: 'edith',
      username: 'edith',
      displayName: 'Edith Anne Widder',
      password: '1234',
      email: 'edith@example.org'
    }
  ],
  [
    'max',
    {
      id: 'max',
      username: 'max',
      displayName: 'Max Testing',
      password: '12345678',
      email: 'maxtesting@opencloud.eu'
    }
  ]
])

export const createdUserStore = new Map<string, User>()

export const federatedUserStore = new Map<string, User>()
