import React from 'react'
import { Modal } from './ui'

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
      <table>
        <tbody>
          {fields
            .filter(([, v]) => v !== null && v !== undefined && v !== '')
            .map(([k, v]) => (
              <tr key={k}>
                <td className="muted" style={{ width: 110 }}>
                  {k}
                </td>
                <td style={{ wordBreak: 'break-word' }}>
                  {typeof v === 'string' && /^https?:\/\//.test(v) ? (
                    <a href={v} target="_blank" rel="noreferrer">
                      {v}
                    </a>
                  ) : (
                    v
                  )}
                </td>
              </tr>
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
