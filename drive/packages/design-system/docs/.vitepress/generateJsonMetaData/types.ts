export type Emit = {
  name: string
  type: string
  description?: string
}

export type Prop = {
  name: string
  required: boolean
  type: string
  default?: string
  description?: string
}

export type Slot = {
  name: string
  type: string
  description?: string
}
