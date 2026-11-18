/// Screenshots for the support chat, as in Alpha Tech's terminal: this tab is
/// captured, then an area of it is dragged out (`components/SnapshotRegion`).
/// Screenshots are the only thing the chat sends besides text: there is no
/// file upload and no paste.
///
/// Each becomes a data URL scaled to at most `MAX_EDGE` on its long side, so
/// a 5K screen is sent at a size anyone can read and the server's 4 MB cap
/// is never the limit in practice.

const MAX_EDGE = 2000
/// PNG keeps a screenshot's text sharp; past this, JPEG is the better trade.
const PNG_BUDGET = 1_500_000
export const MAX_SHOTS = 4

function encode(canvas: HTMLCanvasElement): string {
  const png = canvas.toDataURL('image/png')
  return png.length <= PNG_BUDGET * 1.37 ? png : canvas.toDataURL('image/jpeg', 0.85)
}

/// Screen capture exists on desktop browsers, not on phones, and only on
/// https or localhost.
export function canSnapshot(): boolean {
  return typeof navigator !== 'undefined' && !!navigator.mediaDevices?.getDisplayMedia
}

/// One full-resolution frame of this browser tab, or, in a browser that
/// cannot preselect the current tab, whatever the person picks in its share
/// dialog. The share stops as soon as the frame is taken. Rejects when they
/// cancel.
export async function captureTab(): Promise<HTMLCanvasElement> {
  // Chrome and Edge offer this tab first, so sharing it is one click. The
  // options are hints: a browser that does not know them ignores them.
  const options = {
    video: { displaySurface: 'browser' },
    audio: false,
    preferCurrentTab: true,
    selfBrowserSurface: 'include',
    surfaceSwitching: 'exclude',
  } as DisplayMediaStreamOptions
  const stream = await navigator.mediaDevices.getDisplayMedia(options)
  try {
    const video = document.createElement('video')
    video.muted = true
    video.playsInline = true
    video.srcObject = stream
    await video.play()
    // The first frame can be blank, and the page needs a moment to repaint
    // without the chat panel in it.
    await new Promise((r) => setTimeout(r, 350))
    const canvas = document.createElement('canvas')
    canvas.width = video.videoWidth
    canvas.height = video.videoHeight
    const ctx = canvas.getContext('2d')
    if (!ctx) throw new Error('This browser cannot prepare screenshots')
    ctx.drawImage(video, 0, 0)
    return canvas
  } finally {
    stream.getTracks().forEach((t) => t.stop())
  }
}

/// A rectangle of `frame`, in its own pixels, scaled and encoded.
export function cropToDataUrl(frame: HTMLCanvasElement, x: number, y: number, width: number, height: number): string {
  const scale = Math.min(1, MAX_EDGE / Math.max(width, height))
  const out = document.createElement('canvas')
  out.width = Math.max(1, Math.round(width * scale))
  out.height = Math.max(1, Math.round(height * scale))
  const ctx = out.getContext('2d')
  if (!ctx) throw new Error('This browser cannot prepare screenshots')
  ctx.drawImage(frame, x, y, width, height, 0, 0, out.width, out.height)
  return encode(out)
}
