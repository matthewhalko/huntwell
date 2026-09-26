import React, { useEffect, useRef, useState } from 'react'
import { ModalBackdrop } from './ui'

/// Watching the browser a run is driving.
///
/// Huntwell's own viewer: frames come from our API, and nothing on screen or
/// in the page says who is running the browser underneath. It is deliberately
/// read-only — watching is what this is for, and a view that could click would
/// be a way to take the browser away from the run, or to reach the sites it is
/// signed in to.
export default function BrowserView({ id, onClose }: { id: string; onClose: () => void }) {
  const [src, setSrc] = useState('')
  const [error, setError] = useState('')
  // Kept so a new frame replaces the old one only once it has loaded, which is
  // what stops the view flickering white between polls.
  const objectUrl = useRef('')

  useEffect(() => {
    let live = true
    let timer: number | undefined

    const tick = async () => {
      if (!live) return
      try {
        const res = await fetch(`/api/executions/${id}/browser/frame?t=${Date.now()}`, { credentials: 'include' })
        if (!res.ok) throw new Error(res.status === 503 ? 'The browser is not answering just now.' : 'That run has no browser to watch.')
        const blob = await res.blob()
        if (!live) return
        const next = URL.createObjectURL(blob)
        if (objectUrl.current) URL.revokeObjectURL(objectUrl.current)
        objectUrl.current = next
        setSrc(next)
        setError('')
      } catch (e: any) {
        if (live) setError(e.message || String(e))
      }
      // A run acts every few seconds, so this is as often as there is anything
      // new to see — and each frame is a round trip to the browser.
      if (live) timer = window.setTimeout(tick, 1500)
    }

    tick()
    return () => {
      live = false
      if (timer) window.clearTimeout(timer)
      if (objectUrl.current) URL.revokeObjectURL(objectUrl.current)
      objectUrl.current = ''
    }
  }, [id])

  // Escape closes, the way every other overlay in the app does.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose()
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])

  return (
    <ModalBackdrop onClick={onClose}>
      <div className="modal browser-view" onClick={(e) => e.stopPropagation()}>
        <div className="row browser-view-head">
          <div>
            <b>Live browser</b>
            <span className="muted"> — what this run is looking at right now</span>
          </div>
          <button className="btn sm" onClick={onClose}>
            Close
          </button>
        </div>
        <div className="browser-view-frame">
          {src ? (
            <img src={src} alt="The page this run is reading" />
          ) : (
            <div className="muted browser-view-waiting">{error || 'Connecting to the browser…'}</div>
          )}
          {src && error && <div className="browser-view-note">{error}</div>}
        </div>
      </div>
    </ModalBackdrop>
  )
}
