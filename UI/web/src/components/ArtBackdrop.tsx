import React, { useEffect, useMemo, useState } from 'react'
import { ARTWORKS } from '../art'

/// Fine art behind the sign-in screens, alive the way Multi-Dev's wallpaper
/// is: each painting drifts and breathes for a while, then the next one fades
/// up underneath. Two layers are enough — the one showing and the one
/// arriving — and the next image is fetched a beat before it is needed, so
/// the fade never waits on the network.
///
/// The painting is picked at random on load, so the sign-in page is not the
/// same picture every morning; the order in `ARTWORKS` decides what follows.
const SHOW_MS = 14000
const FADE_MS = 2600

/// Fetch an image and have the browser decode it off the main thread, so
/// putting it on screen later costs a texture upload and nothing more.
function load(src: string): Promise<void> {
  const img = new Image()
  img.src = src
  const decoded = typeof img.decode === 'function' ? img.decode() : new Promise<void>((res) => (img.onload = () => res()))
  return decoded.catch(() => undefined)
}

export function ArtBackdrop() {
  const start = useMemo(() => Math.floor(Math.random() * ARTWORKS.length), [])
  const [index, setIndex] = useState(start)
  // Which of the two layers holds the current painting; they alternate.
  const [front, setFront] = useState<0 | 1>(0)
  const [ready, setReady] = useState(false)
  const reduced = useMemo(
    () => typeof window !== 'undefined' && window.matchMedia?.('(prefers-reduced-motion: reduce)').matches,
    [],
  )

  useEffect(() => {
    // The first one is shown only once it has loaded *and decoded*, so the
    // page does not open on a blank ground that then jumps to a painting.
    let live = true
    load(ARTWORKS[start].src).then(() => live && setReady(true))
    return () => {
      live = false
    }
  }, [start])

  useEffect(() => {
    if (!ready || ARTWORKS.length < 2) return
    let cancelled = false
    const next = (index + 1) % ARTWORKS.length
    // Fetched and decoded well before it is needed: decoding an 1800px JPEG
    // on the main thread at the moment of the swap is a visible hitch.
    const loaded = load(ARTWORKS[next].src)
    const t = setTimeout(() => {
      loaded.then(() => {
        if (cancelled) return
        setIndex(next)
        setFront((f) => (f === 0 ? 1 : 0))
      })
    }, SHOW_MS)
    return () => {
      cancelled = true
      clearTimeout(t)
    }
  }, [ready, index])

  const current = ARTWORKS[index]
  const previous = ARTWORKS[(index - 1 + ARTWORKS.length) % ARTWORKS.length]
  // Layer `front` shows the current painting; the other keeps the previous one
  // while it fades out beneath, so a crossfade rather than a cut.
  const layers = [0, 1].map((slot) => {
    const isFront = slot === front
    const art = isFront ? current : previous
    return (
      <div
        key={slot}
        className={'art-layer' + (isFront && ready ? ' on' : '') + (reduced ? ' still' : '')}
        style={{ backgroundImage: `url(${art.src})` }}
        aria-hidden
      />
    )
  })

  return (
    <div className="art-backdrop" aria-hidden style={{ ['--art-fade' as any]: `${FADE_MS}ms` }}>
      {layers}
      <div className={'art-breath' + (reduced ? ' still' : '')} />
      <div className="art-vignette" />
      <div className={'art-caption' + (ready ? ' on' : '')} key={current.src}>
        <span className="art-title">{current.title}</span>
        <span className="art-artist">
          {current.artist}, {current.date}
        </span>
      </div>
    </div>
  )
}
