import React, { createContext, useContext, useEffect, useMemo, useState } from 'react'
import { MoonIcon, SunIcon } from './components/icons'

export type Theme = 'light' | 'dark' | 'system'

interface ThemeCtx {
  theme: Theme
  resolved: 'light' | 'dark'
  setTheme: (t: Theme) => void
}

const Ctx = createContext<ThemeCtx>({ theme: 'system', resolved: 'light', setTheme: () => {} })
const KEY = 'huntwell.theme'

function systemDark(): boolean {
  return typeof window !== 'undefined' && window.matchMedia('(prefers-color-scheme: dark)').matches
}

export function ThemeProvider({ children }: { children: React.ReactNode }) {
  const [theme, setThemeState] = useState<Theme>(() => {
    try {
      const t = localStorage.getItem(KEY) as Theme | null
      return t === 'light' || t === 'dark' || t === 'system' ? t : 'system'
    } catch {
      return 'system'
    }
  })
  const [sys, setSys] = useState(systemDark())

  useEffect(() => {
    const mq = window.matchMedia('(prefers-color-scheme: dark)')
    const fn = () => setSys(mq.matches)
    mq.addEventListener('change', fn)
    return () => mq.removeEventListener('change', fn)
  }, [])

  const resolved: 'light' | 'dark' = theme === 'system' ? (sys ? 'dark' : 'light') : theme
  useEffect(() => {
    document.documentElement.dataset.theme = resolved
  }, [resolved])

  const setTheme = (t: Theme) => {
    setThemeState(t)
    try {
      localStorage.setItem(KEY, t)
    } catch {}
  }

  const value = useMemo(() => ({ theme, resolved, setTheme }), [theme, resolved])
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>
}

export function useTheme() {
  return useContext(Ctx)
}

// Flips light/dark. Also saved to the account when signed in, so the choice
// follows the user; a 401 (landing page) is simply ignored.
export function ThemeToggle() {
  const { resolved, setTheme } = useTheme()
  const flip = () => {
    const next = resolved === 'dark' ? 'light' : 'dark'
    setTheme(next)
    fetch('/api/auth/me', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ theme: next }),
      credentials: 'same-origin',
    }).catch(() => {})
  }
  return (
    <button
      className="iconbtn"
      title={resolved === 'dark' ? 'Switch to light mode' : 'Switch to dark mode'}
      onClick={flip}
      aria-label="Toggle theme"
    >
      {resolved === 'dark' ? <SunIcon size={17} /> : <MoonIcon size={17} />}
    </button>
  )
}
