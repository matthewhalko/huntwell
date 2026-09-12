import React from 'react'
import { Link } from 'react-router-dom'
import { ApiDocs } from './ApiDocs'

/**
 * The API reference, on its own page.
 *
 * Split from the keys page because they are two different errands: one is
 * "give me a key", done once in ten seconds, and the other is "how do I call
 * this", read for twenty minutes with a terminal open. Stacked together, the
 * reference buried the key list under a screen of endpoints.
 */
export default function ApiReference() {
  return (
    <>
      <div className="page-head">
        <div>
          <h1>API reference</h1>
          <div className="sub">Everything Huntwell does, it does through this. Create a plan, run it, read what it found.</div>
        </div>
        <Link to="/app/api-access" className="btn">
          Keys
        </Link>
      </div>
      <ApiDocs />
    </>
  )
}
