import { z } from 'zod'

const AppTokenSchema = z.object({
  token: z.string(),
  expiration_date: z.string(),
  created_date: z.string(),
  label: z.string().optional()
})

export const AppTokenListSchema = z.array(AppTokenSchema)
export type AppToken = z.infer<typeof AppTokenSchema>
