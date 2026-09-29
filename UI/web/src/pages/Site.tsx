import React from 'react'
import { Link } from 'react-router-dom'
import { SitePage, useCta } from '../components/Site'

// The public site's inner pages. Every claim here maps to something the app
// actually does — plan kinds, schedules, the watch skip, signed keys, the
// guard — so nothing on these pages is a promise the product cannot keep.
// Change the behaviour, change the sentence.

type Card = { title: string; body: React.ReactNode; icon?: React.ReactNode }

function Ico({ children }: { children: React.ReactNode }) {
  return (
    <span className="feat-ico">
      <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
        {children}
      </svg>
    </span>
  )
}

function Cards({ items }: { items: Card[] }) {
  return (
    <div className="feat-grid">
      {items.map((c) => (
        <div className="feat" key={c.title}>
          {c.icon && <Ico>{c.icon}</Ico>}
          <h3>{c.title}</h3>
          <p>{c.body}</p>
        </div>
      ))}
    </div>
  )
}

function Section({ title, lede, children }: { title: React.ReactNode; lede?: React.ReactNode; children: React.ReactNode }) {
  return (
    <section className="site-sec">
      <h2>{title}</h2>
      {lede && <p className="site-sec-lede">{lede}</p>}
      {children}
    </section>
  )
}

function CtaRow() {
  const cta = useCta()
  return (
    <div className="row" style={{ marginTop: '1.4rem' }}>
      <Link to={cta.to} className="btn primary lg">
        {cta.label}
      </Link>
    </div>
  )
}

// ---- Product -----------------------------------------------------------------

const STEPS: { title: string; body: string }[] = [
  {
    title: 'Describe it',
    body: 'Say what you are after in plain words — "independent coffee roasters in Portland and who runs them". No query language, no scraper to configure.',
  },
  {
    title: 'Review the plan',
    body: 'Huntwell drafts a search plan: the sites worth trying, the columns to fill, how hard to look. Change anything before it runs.',
  },
  {
    title: 'It browses for you',
    body: 'A real browser searches, opens pages and follows links, one site at a time, reading pages the way you would. You can watch it live.',
  },
  {
    title: 'Clean results',
    body: 'What it finds lands in your workspace: deduplicated, with blank fields filled from the page itself where the page says so.',
  },
  {
    title: 'Keep it current',
    body: 'Put the plan on a schedule. A scheduled run checks whether anything new has appeared first, and skips the work when nothing has.',
  },
  {
    title: 'Take it with you',
    body: 'Download a CSV, open a result in full, or pull everything into your own systems through the API.',
  },
]

const KINDS: Card[] = [
  {
    title: 'People and companies',
    body: 'Prospects with the details a sales team needs: who they are, where, and how to reach them.',
    icon: (
      <>
        <circle cx="9" cy="8" r="4" />
        <path d="M2 21a7 7 0 0 1 14 0" />
        <path d="M18 8h4M20 6v4" />
      </>
    ),
  },
  {
    title: 'Tables of anything',
    body: 'Name the columns — model, year, price; role, company, pay — and get rows back. Anything with a shape.',
    icon: (
      <>
        <ellipse cx="12" cy="5" rx="8" ry="3" />
        <path d="M4 5v6c0 1.7 3.6 3 8 3s8-1.3 8-3V5" />
        <path d="M4 11v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6" />
      </>
    ),
  },
  {
    title: 'Written reports',
    body: 'Ask a question instead of asking for a list: a brief on the subject, with the sources it read. Print it to PDF.',
    icon: (
      <>
        <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
        <path d="M14 3v5h5" />
        <path d="M9 13h6M9 17h4" />
      </>
    ),
  },
  {
    title: 'Files to keep',
    body: 'Documents it finds about a subject — filings, brochures, spec sheets — collected and stored for you.',
    icon: (
      <>
        <path d="M4 20h16" />
        <path d="M12 4v10" />
        <path d="m8 10 4 4 4-4" />
      </>
    ),
  },
]

export function Product() {
  return (
    <SitePage
      eyebrow="Product"
      title={
        <>
          Search the web the way a <span className="grad-text">person would</span>, at the scale of a machine.
        </>
      }
      lede="Type what you want. Huntwell goes and gets it."
    >
      <Section title="How it works" lede="Six steps, and you only do the first two.">
        <ol className="site-steps">
          {STEPS.map((s, n) => (
            <li key={s.title}>
              <span className="site-step-n">{n + 1}</span>
              <div>
                <h3>{s.title}</h3>
                <p>{s.body}</p>
              </div>
            </li>
          ))}
        </ol>
      </Section>
      <Section title="What comes back" lede="Four shapes, one workspace. Reports and files are new, and switched on workspace by workspace.">
        <Cards items={KINDS} />
      </Section>
      <Section title="Built for working with others">
        <Cards
          items={[
            { title: 'Shared workspaces', body: 'Invite your team. Everyone sees the same plans and results, and anyone can run a search again.' },
            { title: 'See what it did', body: 'Every run keeps its log and a map of the searches it ran, the pages it opened and what came out of each.' },
            { title: 'Know what it cost', body: 'Each run shows what it spent and what each new result cost, so you can tell a good search from an expensive one.' },
          ]}
        />
        <CtaRow />
      </Section>
    </SitePage>
  )
}

// ---- Use cases -----------------------------------------------------------------

const CASES: { title: string; who: string; ask: string; get: string }[] = [
  {
    title: 'Sales prospecting',
    who: 'Sales and business development',
    ask: 'Hotel general managers on the Oregon coast',
    get: 'A list of people and companies with role, company, location and contact details, deduplicated against what you already have.',
  },
  {
    title: 'Market and listing watch',
    who: 'Buyers, analysts and operators',
    ask: 'Used Land Cruisers under $40k in Texas',
    get: 'A table with the columns you name — model, year, price, link — refreshed on a schedule, with new listings added as they appear.',
  },
  {
    title: 'Hiring and job research',
    who: 'Recruiters and job seekers',
    ask: 'Remote React jobs posted this week',
    get: 'Role, company and pay for every posting it can find, gathered across boards instead of tab by tab.',
  },
  {
    title: 'Research briefs',
    who: 'Founders, investors and analysts',
    ask: 'A report on Peru’s payment rails',
    get: 'A written document on the subject, drawn from the pages it read, with those sources listed.',
  },
  {
    title: 'Tenders and opportunities',
    who: 'Bid teams and agencies',
    ask: 'Open government tenders for web development in Ontario',
    get: 'A running list of opportunities with deadline, buyer and link, checked again on the schedule you set.',
  },
  {
    title: 'Document collection',
    who: 'Compliance and diligence',
    ask: 'Annual reports and investor decks for these five companies',
    get: 'The files themselves, found and stored in your workspace, instead of a list of links to go and download.',
  },
]

export function UseCases() {
  return (
    <SitePage
      eyebrow="Use cases"
      title={
        <>
          If it is on the web, <span className="grad-text">ask for it</span>.
        </>
      }
      lede="People use Huntwell wherever the answer is spread across many pages and copying it out by hand would take an afternoon. A few of the common ones:"
    >
      <div className="site-cases">
        {CASES.map((c) => (
          <article className="site-case" key={c.title}>
            <span className="site-eyebrow">{c.who}</span>
            <h3>{c.title}</h3>
            <p className="site-ask">
              <span aria-hidden>“</span>
              {c.ask}
              <span aria-hidden>”</span>
            </p>
            <p>{c.get}</p>
          </article>
        ))}
      </div>
      <Section title="Not on the list?" lede="If you can describe it and it is on public web pages, it is worth a try. The plan shows you what Huntwell intends to do before it spends anything.">
        <CtaRow />
      </Section>
    </SitePage>
  )
}

// ---- Pricing -----------------------------------------------------------------

const PRICING_POINTS: Card[] = [
  {
    title: 'Browsing spends tokens',
    body: 'Every run opens pages in a real browser, reads them and fills in your fields. That work is measured in tokens — the same unit language models use for input and output.',
    icon: (
      <>
        <circle cx="12" cy="12" r="9" />
        <path d="M12 7v5l3 2" />
      </>
    ),
  },
  {
    title: 'You pay for what you use',
    body: 'There is no seat fee and no plan tier. Buy prepaid credits for your workspace; each run debits them as it spends tokens. A quiet run costs less than a deep one.',
    icon: (
      <>
        <path d="M12 2v20" />
        <path d="M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6" />
      </>
    ),
  },
  {
    title: 'Cap any run',
    body: 'Set a token limit before you start. The run stops there, keeps what it found, and never spends past your credits.',
    icon: (
      <>
        <path d="M4 12h16" />
        <path d="M12 4v16" />
        <circle cx="12" cy="12" r="9" />
      </>
    ),
  },
  {
    title: 'See the cost',
    body: 'Every execution shows the tokens it used and what that came to, so you can tell a cheap search from an expensive one.',
    icon: (
      <>
        <path d="M3 3v18h18" />
        <path d="M7 14l4-4 3 3 5-6" />
      </>
    ),
  },
]

export function Pricing() {
  return (
    <SitePage
      eyebrow="Pricing"
      title={
        <>
          Pay for the pages it <span className="grad-text">reads</span>, not a monthly seat.
        </>
      }
      lede="Huntwell charges by the tokens a run spends while browsing and filling results. Use more, pay more; use less, pay less."
    >
      <Section title="How billing works" lede="One meter, tied to the work the browser actually does.">
        <Cards items={PRICING_POINTS} />
      </Section>
      <Section
        title="Ready when you are"
        lede="Add a card, buy credits, and the next run draws from them. Unused credits stay in the workspace."
      >
        <CtaRow />
      </Section>
    </SitePage>
  )
}
