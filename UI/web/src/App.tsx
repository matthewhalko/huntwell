import React from 'react'
import { Navigate, Route, Routes, useLocation, useParams } from 'react-router-dom'
import { useAuth } from './auth'
import Layout from './components/Layout'
import Landing from './pages/Landing'
import { Login, Signup } from './pages/Auth'
import Dashboard from './pages/Dashboard'
import Plans from './pages/Plans'
import PlanNew from './pages/PlanNew'
import PlanDetail from './pages/PlanDetail'
import Runs from './pages/Runs'
import RunView from './pages/RunView'
import Prospects from './pages/Prospects'
import Usage from './pages/Usage'
import ApiAccess from './pages/ApiAccess'
import ApiReference from './pages/ApiReference'
import Settings from './pages/Settings'
import Join from './pages/Join'
import { Privacy, Terms } from './pages/Legal'

function RequireAuth({ children }: { children: React.ReactElement }) {
  const { me, loading } = useAuth()
  const loc = useLocation()
  if (loading) return <div style={{ padding: '3rem', textAlign: 'center' }}>Loading…</div>
  if (!me) return <Navigate to="/login" state={{ from: loc.pathname }} replace />
  return children
}

/// Old run links keep working: /app/runs/12 → /app/executions/12.
function RunRedirect() {
  const { id } = useParams()
  return <Navigate to={`/app/executions/${id}`} replace />
}

export default function App() {
  const { me, loading } = useAuth()
  return (
    <Routes>
      {/* Always the landing page, signed in or not.
          It used to redirect a signed-in visitor to /app, which broke the back
          button for them permanently: Back from /app lands here, and here sent
          them straight forward again. The browser's Back looked dead, and the
          marketing site was unreachable to anyone with an account.
          Landing shows "Open Huntwell" instead of the sign-in buttons when it
          knows who you are, so the redirect bought nothing anyway. */}
      <Route path="/" element={<Landing />} />
      <Route path="/login" element={<Login />} />
      <Route path="/signup" element={<Signup />} />
      {/* An invite link: readable signed out, acceptable only as the invitee. */}
      <Route path="/join/:token" element={<Join />} />
      <Route path="/terms" element={<Terms />} />
      <Route path="/privacy" element={<Privacy />} />
      <Route
        path="/app"
        element={
          <RequireAuth>
            <Layout />
          </RequireAuth>
        }
      >
        <Route index element={<Dashboard />} />
        <Route path="plans" element={<Plans />} />
        <Route path="plans/new" element={<PlanNew />} />
        <Route path="plans/:id" element={<PlanDetail />} />
        <Route path="executions" element={<Runs />} />
        <Route path="executions/:id" element={<RunView />} />
        {/* Anything already pointing at the old word still lands. */}
        <Route path="runs" element={<Navigate to="/app/executions" replace />} />
        <Route path="runs/:id" element={<RunRedirect />} />
        <Route path="prospects" element={<Prospects />} />
        <Route path="usage" element={<Usage />} />
        <Route path="api-access" element={<ApiAccess />} />
        <Route path="api-docs" element={<ApiReference />} />
        <Route path="settings" element={<Settings />} />
      </Route>
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  )
}
