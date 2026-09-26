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

export default function Grid<T>({ rows, columns, getRowId, onRowClick, visibleRows = 12, fill, loading, options }: GridProps<T>) {
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
