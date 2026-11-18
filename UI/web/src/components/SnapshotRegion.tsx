import React from 'react'
import { createPortal } from 'react-dom'
import { cropToDataUrl } from '../supportShots'

/// Pick part of a captured frame, the way ⌘⇧4 does on a Mac: the frame is
/// shown frozen and dimmed over the whole window, a drag draws the area to
/// keep (its size beside it), and letting go takes it. A click without a drag
/// takes everything; Esc cancels. Ported from Alpha Tech's terminal.
///
/// Pointer events, so a trackpad, mouse or finger all drag the same way.

type Props = {
  frame: HTMLCanvasElement
  /// The cropped screenshot, or null when cancelled.
  onDone: (image: string | null) => void
}

/// A drag shorter than this is a click.
const MIN_DRAG = 6

type Point = { x: number; y: number }

export function SnapshotRegion({ frame, onDone }: Props) {
  const [start, setStart] = React.useState<Point | null>(null)
  const [end, setEnd] = React.useState<Point | null>(null)
  const [frameUrl] = React.useState(() => frame.toDataURL('image/png'))
  const imgRef = React.useRef<HTMLImageElement>(null)

  React.useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault()
        e.stopPropagation()
        onDone(null)
      }
    }
    window.addEventListener('keydown', onKey, true)
    return () => window.removeEventListener('keydown', onKey, true)
  }, [onDone])

  /// The frame as drawn: fitted to the window, so a capture of another
  /// window or screen still maps onto it correctly.
  const shown = () => imgRef.current?.getBoundingClientRect() ?? null

  const finish = (a: Point, b: Point) => {
    const box = shown()
    if (!box) return onDone(null)
    const scaleX = frame.width / box.width
    const scaleY = frame.height / box.height
    if (Math.abs(b.x - a.x) < MIN_DRAG && Math.abs(b.y - a.y) < MIN_DRAG) {
      return onDone(cropToDataUrl(frame, 0, 0, frame.width, frame.height))
    }
    // Clamp to the picture, then into the frame's own pixels.
    const left = Math.max(box.left, Math.min(a.x, b.x))
    const top = Math.max(box.top, Math.min(a.y, b.y))
    const right = Math.min(box.right, Math.max(a.x, b.x))
    const bottom = Math.min(box.bottom, Math.max(a.y, b.y))
    if (right <= left || bottom <= top) return onDone(null)
    onDone(cropToDataUrl(frame, (left - box.left) * scaleX, (top - box.top) * scaleY, (right - left) * scaleX, (bottom - top) * scaleY))
  }

  const rect = start && end ? { left: Math.min(start.x, end.x), top: Math.min(start.y, end.y), width: Math.abs(end.x - start.x), height: Math.abs(end.y - start.y) } : null
  const box = shown()
  const pixels = rect && box ? `${Math.round(rect.width * (frame.width / box.width))} × ${Math.round(rect.height * (frame.height / box.height))}` : ''

  return createPortal(
    <div
      className="snap"
      role="dialog"
      aria-label="Select an area to send"
      onPointerDown={(e) => {
        if (e.button !== 0) return
        e.currentTarget.setPointerCapture(e.pointerId)
        setStart({ x: e.clientX, y: e.clientY })
        setEnd({ x: e.clientX, y: e.clientY })
      }}
      onPointerMove={(e) => {
        if (start) setEnd({ x: e.clientX, y: e.clientY })
      }}
      onPointerUp={(e) => {
        if (start) finish(start, { x: e.clientX, y: e.clientY })
      }}
      onContextMenu={(e) => {
        e.preventDefault()
        onDone(null)
      }}
    >
      <img ref={imgRef} className="snap-frame" src={frameUrl} alt="" draggable={false} />
      {/* The dim. With a selection, a box whose huge shadow is the dim, so the
          selected area alone shows at full brightness. */}
      {rect ? (
        <div className="snap-pick" style={{ left: rect.left, top: rect.top, width: rect.width, height: rect.height }}>
          <span className="snap-size">{pixels}</span>
        </div>
      ) : (
        <>
          <div className="snap-dim" />
          <div className="snap-hint">Drag to select an area · Click for everything · Esc to cancel</div>
        </>
      )}
    </div>,
    document.body,
  )
}
