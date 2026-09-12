import React, { useEffect, useState } from 'react'
import { api, Run } from '../api'
import { RunsTab } from './PlanDetail'

export default function Runs() {
  const [runs, setRuns] = useState<Run[]>([])
  const load = () => api.get<Run[]>('/api/executions?limit=200').then(setRuns)
  useEffect(() => {
    load()
    const t = setInterval(load, 8000)
    return () => clearInterval(t)
  }, [])
  return (
    <>
      <div className="page-head">
        <div>
          <h1>History</h1>
          <div className="sub">Every execution, across all your search plans.</div>
        </div>
      </div>
      <RunsTab runs={runs} />
    </>
  )
}
