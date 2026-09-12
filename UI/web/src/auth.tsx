import React, { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react'
import { api, Me, setUnauthorizedHandler } from './api'
import { useTheme } from './theme'

interface AuthCtx {
  me: Me | null
  loading: boolean
  refresh: () => Promise<void>
}

const Ctx = createContext<AuthCtx>({ me: null, loading: true, refresh: async () => {} })

export function AuthProvider({ children }: { children: React.ReactNode }) {
  const [me, setMe] = useState<Me | null>(null)
  const [loading, setLoading] = useState(true)
  const { setTheme } = useTheme()
  const themeApplied = useRef(false)

  const refresh = useCallback(async () => {
    try {
      const m = await api.get<Me>('/api/auth/me')
      setMe(m)
      // The account's stored preference is applied once per page load; after
      // that the toggle (which also saves to the account) is what the user
      // last chose, and a refresh must not undo it.
      if (m.theme && !themeApplied.current) {
        themeApplied.current = true
        setTheme(m.theme)
      }
    } catch {
      setMe(null)
    } finally {
      setLoading(false)
    }
  }, [setTheme])

  useEffect(() => {
    refresh()
    setUnauthorizedHandler(() => setMe(null))
  }, [refresh])

  return <Ctx.Provider value={{ me, loading, refresh }}>{children}</Ctx.Provider>
}

export function useAuth() {
  return useContext(Ctx)
}
