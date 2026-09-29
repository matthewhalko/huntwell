import React from 'react'
import { CopyHover, Modal } from './ui'

/// The result detail dialog — a person, an artifact row, or any result on the
/// cross-plan Results page — so a click reads the same everywhere.
export function ResultDialog({
  title,
  fields,
  onClose,
  onDelete,
  extra,
  actions,
}: {
  title: string
  fields: [string, React.ReactNode][]
  onClose: () => void
  onDelete?: () => void
  extra?: React.ReactNode
  /** Buttons beside Delete, on the left: what to do with this result. */
  actions?: React.ReactNode
}) {
  return (
    <Modal title={title} onClose={onClose} className="result-dialog">
      <table className="result-fields">
        <tbody>
          {fields
            .filter(([, v]) => v !== null && v !== undefined && v !== '')
            .map(([k, v]) => (
              <FieldRow key={k} label={k} value={v} />
            ))}
        </tbody>
      </table>
      {extra}
      {/* Delete on the left, away from Close on the right: the destructive
          action is never where the hand goes to dismiss the dialog. */}
      <div className="row between" style={{ marginTop: '1rem' }}>
        <div className="row">
          {onDelete && (
            <button className="btn danger sm" onClick={onDelete}>
              Delete
            </button>
          )}
          {actions}
        </div>
        <button className="btn sm" onClick={onClose} autoFocus>
          Close
        </button>
      </div>
    </Modal>
  )
}

/// One labelled value. Hovering the row shows a copy icon beside the value; it
/// copies the value as it reads on screen (a link's address, a formatted
/// price), so what lands on the clipboard is what the person was looking at.
function FieldRow({ label, value }: { label: string; value: React.ReactNode }) {
  const cell = React.useRef<HTMLTableCellElement>(null)
  const isLink = typeof value === 'string' && /^https?:\/\//.test(value)
  return (
    <tr className="result-field">
      <td className="muted" style={{ width: 110 }}>
        {label}
      </td>
      <td ref={cell} className="copyable" style={{ wordBreak: 'break-word' }}>
        {isLink ? (
          <a href={value as string} target="_blank" rel="noreferrer">
            {value}
          </a>
        ) : (
          value
        )}
        <CopyHover what={label} text={() => (typeof value === 'string' || typeof value === 'number' ? String(value) : cell.current?.innerText || '')} />
      </td>
    </tr>
  )
}
