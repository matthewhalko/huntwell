import React from 'react'
import { Link } from 'react-router-dom'
import { SitePage, useCta } from '../components/Site'
import LiveDemo from '../components/LiveDemo'
import { useAuthConfig } from './Auth'

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

function Section({ title, lede, children }: { title: React.ReactNode; lede?: React.ReactNode; children?: React.ReactNode }) {
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
      lede="Clients use Huntwell to collect information from the internet in real time. Describe what you need and it searches the live web, reads the pages and returns clean, structured results. Put it on a schedule and it keeps collecting as new information appears."
    >
      <Section title="See one run" lede="Pick an example and watch Huntwell search, read and fill in the results.">
        <LiveDemo />
      </Section>
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

/// The rate as people read a price: "$1", "$1.50".
function dollars(n: number) {
  return Number.isInteger(n) ? `$${n}` : `$${n.toFixed(2)}`
}

const UPSIDES: Card[] = [
  {
    title: 'No subscription',
    body: 'Nothing to pay each month just to keep an account. A month you do not search costs nothing.',
    icon: (
      <>
        <rect x="3" y="5" width="18" height="16" rx="2" />
        <path d="M3 10h18M8 3v4M16 3v4" />
      </>
    ),
  },
  {
    title: 'No seat fees',
    body: 'Invite your whole team. More people in the workspace never raises the price. Only the searching does.',
    icon: (
      <>
        <circle cx="8" cy="9" r="3" />
        <circle cx="17" cy="10" r="2.5" />
        <path d="M2 20a6 6 0 0 1 12 0" />
        <path d="M15 20a5 5 0 0 1 7-4.6" />
      </>
    ),
  },
  {
    title: 'No tiers to outgrow',
    body: 'One price for a quick list or a deep weekly search. No plan to upgrade when you need more, and no usage cliff.',
    icon: (
      <>
        <path d="M4 12h16" />
        <path d="M4 7h16M4 17h16" />
      </>
    ),
  },
  {
    title: 'Costs follow the work',
    body: 'A small search spends little and a big one spends more. A scheduled search can check for anything new first and skip the work when nothing has changed.',
    icon: (
      <>
        <path d="M3 3v18h18" />
        <path d="M7 14l4-4 3 3 5-6" />
      </>
    ),
  },
  {
    title: 'A cap on every run',
    body: 'Set a limit before a search starts. It stops there, keeps what it found, and never spends past your credit.',
    icon: (
      <>
        <rect x="5" y="11" width="14" height="10" rx="2" />
        <path d="M8 11V7a4 4 0 0 1 8 0v4" />
      </>
    ),
  },
  {
    title: 'See every cent',
    body: 'Each run shows what it used and what that came to, and what each new result cost, so you can tell a good search from an expensive one.',
    icon: (
      <>
        <circle cx="12" cy="12" r="9" />
        <path d="M12 7v10M9.5 9.5a2.5 2 0 0 1 5 0c0 2.5-5 2-5 4.5a2.5 2 0 0 0 5 0" />
      </>
    ),
  },
]

export function Pricing() {
  // The rate billing actually uses; $1 until the server says otherwise.
  const rate = dollars(useAuthConfig()?.usd_per_mtoken ?? 1)
  return (
    <SitePage
      eyebrow="Pricing"
      title={
        <>
          {rate} per million <span className="grad-text">AI tokens</span>.
        </>
      }
      lede="One flat rate. You pay for what you use and nothing else. No subscription, no seat fees, no tiers."
    >
      <div className="price-card">
        <div className="price-amount">
          <span className="price-n">{rate}</span>
          <span className="price-per">per million AI tokens</span>
        </div>
        <ul className="price-list">
          <li>Every search, every kind of result, the same rate</li>
          <li>Buy credit up front and each run draws from it as it goes</li>
          <li>Unused credit stays in your workspace</li>
          <li>Your whole team included</li>
        </ul>
      </div>

      <Section
        title="What is an AI token?"
        lede="Tokens are how AI measures the work it does. When Huntwell searches for you, the AI reads web pages and writes your results, and every word it reads or writes is counted in small pieces called tokens. A token is about three quarters of a word, so a million AI tokens is roughly 750,000 words."
      />

      <Section title="Why pay this way" lede="You pay for the searching Huntwell does for you, and for nothing that sits idle.">
        <Cards items={UPSIDES} />
      </Section>

      <Section title="Ready when you are" lede="Add a card, buy credit, and your next search draws from it.">
        <CtaRow />
      </Section>
    </SitePage>
  )
}
