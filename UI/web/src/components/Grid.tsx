import React, { useEffect, useMemo, useState } from 'react'
import { AgGridReact } from 'ag-grid-react'
import type { ColDef, GridOptions } from 'ag-grid-community'
import 'ag-grid-community/styles/ag-grid.css'
import 'ag-grid-community/styles/ag-theme-quartz.css'
import { Spinner } from './ui'

/// Every data table in the app.
///
/// One wrapper rather than an AG Grid in each page: the look is Huntwell's —
/// driven from the same CSS tokens as the rest of the app, in `styles.css`
/// under `.hw-grid` — and the pages stay declarative, a column list each.
///
/// Bundled rather than loaded from a CDN. The app's Content Security Policy
/// allows scripts from itself, Turnstile and Stripe, and nowhere else; a CDN
/// tag would simply be blocked, which is how the card form broke once already.
export interface GridProps<T> {
  rows: T[]
  columns: ColDef<T>[]
  /// Row identity, so sorting and re-fetching do not lose the selection.
  getRowId?: (row: T) => string
  onRowClick?: (row: T) => void
  /// Roughly how many rows of height to take before scrolling inside.
  visibleRows?: number
  /// Fill the parent's height instead of sizing to a fixed number of rows.
  fill?: boolean
  /// Hide the rows (or the whole grid) until the first fetch lands.
  loading?: boolean
  options?: GridOptions<T>
}

/// Phone-width layout, where a grid keeps only the columns that name the row
/// and leaves the rest to its dialog.
export function useNarrow() {
  const q = '(max-width: 860px)'
  const [on, setOn] = useState(() => typeof window !== 'undefined' && window.matchMedia(q).matches)
  useEffect(() => {
    const mq = window.matchMedia(q)
    const fn = () => setOn(mq.matches)
    mq.addEventListener('change', fn)
    return () => mq.removeEventListener('change', fn)
  }, [])
  return on
}

/// A column that only ever holds a short link ("open ↗"): kept narrow and out
/// of the share-the-width arithmetic, so the columns with words in them get
/// the room.
export const LINK_COLUMN = {
  flex: 0,
  width: 92,
  minWidth: 72,
  maxWidth: 120,
  suppressSizeToFit: true,
  sortable: false,
} as const

/// A column's value for one row, read the way AG Grid would: its valueGetter,
/// else its field.
function valueOf<T>(col: ColDef<T>, row: T): unknown {
  if (typeof col.valueGetter === 'function') {
    try {
      return (col.valueGetter as (p: { data: T }) => unknown)({ data: row })
    } catch {
      return undefined
    }
  }
  if (typeof col.field === 'string') {
    return col.field.split('.').reduce<unknown>((v, k) => (v && typeof v === 'object' ? (v as Record<string, unknown>)[k] : undefined), row)
  }
  return undefined
}

/// What a column shows for a row: its cellRenderer (given the same
/// `{ value, data }` it gets in the grid), else its valueFormatter, else the
/// value itself. `null` when there is nothing to show.
function shownOf<T>(col: ColDef<T>, row: T): React.ReactNode {
  const value = valueOf(col, row)
  if (typeof col.cellRenderer === 'function') {
    return React.createElement(col.cellRenderer as React.ComponentType<{ value: unknown; data: T }>, { value, data: row })
  }
  if (value === null || value === undefined || value === '') return null
  if (typeof col.valueFormatter === 'function') {
    const s = (col.valueFormatter as (p: { value: unknown; data: T }) => string)({ value, data: row })
    return s === '' ? null : s
  }
  return String(value)
}

const blank = (v: unknown) => v === null || v === undefined || v === ''

/**
 * Phone width: the same rows as cards. A grid with several columns cannot fit
 * a phone without dropping most of them; a card keeps them all. The first
 * column is the card's title, every other titled column is a label and its
 * value (left out when empty), and untitled columns — a link, a delete
 * button — sit along the card's foot. Columns render exactly as in the grid,
 * so a page needs no second description of its data.
 */
function GridCards<T>({ rows, columns, getRowId, onRowClick, fill }: GridProps<T>) {
  const [title, ...rest] = columns
  const fields = rest.filter((c) => (c.headerName ?? '').trim() !== '')
  const actions = rest.filter((c) => (c.headerName ?? '').trim() === '')
  return (
    <div className={'grid-cards' + (fill ? ' fill' : '')} role="list">
      {rows.map((row, i) => {
        const open = onRowClick ? () => onRowClick(row) : undefined
        return (
          <div
            key={getRowId ? getRowId(row) : i}
            role="listitem"
            className={'grid-card' + (open ? ' clickable' : '')}
            tabIndex={open ? 0 : undefined}
            onClick={(e) => {
              const t = e.target as HTMLElement
              if (t.closest('a,button,input,textarea,select')) return
              open?.()
            }}
            onKeyDown={(e) => {
              if (open && (e.key === 'Enter' || e.key === ' ') && e.target === e.currentTarget) {
                e.preventDefault()
                open()
              }
            }}
          >
            {title && <div className="grid-card-title">{shownOf(title, row) ?? '—'}</div>}
            {fields.length > 0 && (
              <dl className="grid-card-fields">
                {fields.map((c, n) => {
                  // Empty is left out, even where the column would draw a "—":
                  // on a card that is a line saying nothing. A column with no
                  // field or getter can only be judged by what it renders.
                  const sourced = typeof c.valueGetter === 'function' || typeof c.field === 'string'
                  if (sourced && blank(valueOf(c, row))) return null
                  const shown = shownOf(c, row)
                  if (shown === null) return null
                  return (
                    <React.Fragment key={c.colId ?? c.field ?? n}>
                      <dt>{c.headerName}</dt>
                      <dd>{shown}</dd>
                    </React.Fragment>
                  )
                })}
              </dl>
            )}
            {actions.length > 0 && (
              <div className="grid-card-actions">
                {actions.map((c, n) => (
                  <React.Fragment key={c.colId ?? c.field ?? n}>{shownOf(c, row)}</React.Fragment>
                ))}
              </div>
            )}
          </div>
        )
      })}
    </div>
  )
}

export default function Grid<T>(props: GridProps<T>) {
  const narrow = useNarrow()
  if (narrow && !(props.loading && props.rows.length === 0)) return <GridCards {...props} />
  return <TableGrid {...props} />
}

function TableGrid<T>({ rows, columns, getRowId, onRowClick, visibleRows = 12, fill, loading, options }: GridProps<T>) {
  // Sensible for every table we have: sortable, resizable, and text that is
  // too long ellipsises rather than pushing the column wide. Every column
  // flexes by default, so together they always fill the grid exactly, in the
  // proportions each page gives them. (AG Grid's "size to fit" is not used:
  // run on top of flex it flattens every column to an equal share.) Pages
  // drop columns at phone width (`useNarrow`) so what is left always fits.
  const defaultColDef = useMemo<ColDef<T>>(
    () => ({ sortable: true, resizable: true, minWidth: 60, flex: 1, suppressMovable: false }),
    [],
  )

  // Nothing to measure yet: keep the card, show the spinner where the rows
  // will be, and do not flash an empty header row.
  if (loading && rows.length === 0) {
    return (
      <div className={'card pad0 hw-grid-loading' + (fill ? ' fill' : '')}>
        <Spinner />
      </div>
    )
  }

  const height = Math.min(rows.length + 1, visibleRows) * 44 + 8

  return (
    <div className={'card pad0 hw-grid ag-theme-quartz' + (fill ? ' fill' : '')} style={fill ? undefined : { height }}>
      <AgGridReact<T>
        rowData={rows}
        columnDefs={columns}
        defaultColDef={defaultColDef}
        // Columns always fit the width (above); a sideways scrollbar would only
        // ever appear for a sliver of overflow, so it is never shown.
        suppressHorizontalScroll
        getRowId={getRowId ? (p) => getRowId(p.data) : undefined}
        onRowClicked={
          onRowClick
            ? (e) => {
                if (!e.data) return
                const t = (e.event?.target ?? null) as HTMLElement | null
                if (t?.closest?.('a,button,input,textarea,select')) return
                onRowClick(e.data)
              }
            : undefined
        }
        onCellClicked={
          onRowClick
            ? (e) => {
                if (!e.data) return
                const t = (e.event?.target ?? null) as HTMLElement | null
                if (t?.closest?.('a,button,input,textarea,select')) return
                onRowClick(e.data)
              }
            : undefined
        }
        rowClass={onRowClick ? 'clickable' : undefined}
        headerHeight={40}
        rowHeight={44}
        animateRows
        suppressCellFocus
        // The app says "nothing here yet" with its own Empty component, so the
        // grid never needs to.
        suppressNoRowsOverlay
        {...options}
      />
      {loading && (
        <div className="hw-grid-overlay">
          <Spinner />
        </div>
      )}
    </div>
  )
}
