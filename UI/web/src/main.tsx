import React from 'react'
import ReactDOM from 'react-dom/client'
import { BrowserRouter } from 'react-router-dom'
import App from './App'
import { ThemeProvider } from './theme'
import { AuthProvider } from './auth'
import { ConfirmProvider, ToastProvider } from './components/ui'
import './styles.css'

// public/skeleton.js filled #root with a skeleton and marked it busy; React's
// first render replaces the skeleton, and the page is no longer loading.
const root = document.getElementById('root')!
root.removeAttribute('aria-busy')
ReactDOM.createRoot(root).render(
  <React.StrictMode>
    <ThemeProvider>
      <AuthProvider>
        <ToastProvider>
          <ConfirmProvider>
            <BrowserRouter>
              <App />
            </BrowserRouter>
          </ConfirmProvider>
        </ToastProvider>
      </AuthProvider>
    </ThemeProvider>
  </React.StrictMode>,
)
