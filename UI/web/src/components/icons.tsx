import React from 'react'

// Outlined, monochrome line icons (lucide-style). They draw with currentColor,
// so they always follow the theme — never a coloured emoji.
function I({ size = 16, children }: { size?: number; children: React.ReactNode }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
      style={{ flex: 'none', verticalAlign: '-0.15em' }}
    >
      {children}
    </svg>
  )
}

export const SearchIcon = ({ size }: { size?: number }) => (
  <I size={size}>
    <circle cx="11" cy="11" r="7" />
    <path d="m21 21-4.3-4.3" />
  </I>
)

export const MapIcon = ({ size }: { size?: number }) => (
  <I size={size}>
    <path d="M9 3 3 5v16l6-2 6 2 6-2V3l-6 2-6-2z" />
    <path d="M9 3v16" />
    <path d="M15 5v16" />
  </I>
)

export const ClockIcon = ({ size }: { size?: number }) => (
  <I size={size}>
    <circle cx="12" cy="12" r="9" />
    <path d="M12 7v5l3 2" />
  </I>
)

export const CheckIcon = ({ size }: { size?: number }) => (
  <I size={size}>
    <circle cx="12" cy="12" r="9" />
    <path d="m8 12 2.8 2.8L16.5 9" />
  </I>
)

export const CopyIcon = ({ size }: { size?: number }) => (
  <I size={size}>
    <rect x="9" y="9" width="11" height="11" rx="2" />
    <path d="M5 15H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v1" />
  </I>
)

export const TickIcon = ({ size }: { size?: number }) => (
  <I size={size}>
    <path d="m5 12.5 4.5 4.5L19 7.5" />
  </I>
)

export const SparkIcon = ({ size }: { size?: number }) => (
  <I size={size}>
    <path d="M12 3l1.9 5.8L20 12l-6.1 3.2L12 21l-1.9-5.8L4 12l6.1-3.2z" />
  </I>
)

export const SunIcon = ({ size }: { size?: number }) => (
  <I size={size}>
    <circle cx="12" cy="12" r="4" />
    <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
  </I>
)

export const MoonIcon = ({ size }: { size?: number }) => (
  <I size={size}>
    <path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z" />
  </I>
)

/// Stripe's card.brand, lowercased. Mock cards store "Visa".
export function cardBrandKey(brand?: string | null): string {
  const b = (brand || '').trim().toLowerCase().replace(/[\s-]+/g, '_')
  if (b === 'american_express' || b === 'americanexpress') return 'amex'
  if (b === 'diners_club' || b === 'dinersclub') return 'diners'
  return b
}

export function cardBrandLabel(brand?: string | null): string {
  const k = cardBrandKey(brand)
  if (k === 'visa') return 'Visa'
  if (k === 'mastercard') return 'Mastercard'
  if (k === 'amex') return 'Amex'
  if (k === 'discover') return 'Discover'
  if (k === 'diners') return 'Diners Club'
  if (k === 'jcb') return 'JCB'
  if (k === 'unionpay') return 'UnionPay'
  return brand?.trim() || 'Card'
}

/// A small card face whose mark says the network — Visa, Mastercard, Amex,
/// and the rest Stripe can return. Drawn in currentColor so it follows the
/// theme; the shapes are what tell the networks apart.
export function CardBrandIcon({ brand, size = 38 }: { brand?: string | null; size?: number }) {
  const k = cardBrandKey(brand)
  const label = cardBrandLabel(brand)
  const h = Math.round(size * 0.64)
  return (
    <svg
      className="card-brand-ico"
      width={size}
      height={h}
      viewBox="0 0 40 26"
      role="img"
      aria-label={label}
    >
      <rect x="0.6" y="0.6" width="38.8" height="24.8" rx="3.4" fill="var(--surface-2)" stroke="var(--border-strong)" />
      {k === 'visa' && (
        <text x="20" y="17.2" textAnchor="middle" className="card-brand-word visa">
          VISA
        </text>
      )}
      {k === 'mastercard' && (
        <g>
          <circle cx="16.2" cy="13" r="6.4" fill="currentColor" opacity="0.82" />
          <circle cx="23.8" cy="13" r="6.4" fill="currentColor" opacity="0.38" />
        </g>
      )}
      {k === 'amex' && (
        <text x="20" y="17.2" textAnchor="middle" className="card-brand-word">
          AMEX
        </text>
      )}
      {k === 'discover' && (
        <text x="20" y="17.2" textAnchor="middle" className="card-brand-word">
          DISC
        </text>
      )}
      {k === 'diners' && (
        <g fill="none" stroke="currentColor" strokeWidth="1.6">
          <circle cx="20" cy="13" r="7" />
          <ellipse cx="20" cy="13" rx="3.2" ry="7" />
        </g>
      )}
      {k === 'jcb' && (
        <text x="20" y="17.2" textAnchor="middle" className="card-brand-word">
          JCB
        </text>
      )}
      {k === 'unionpay' && (
        <text x="20" y="17.2" textAnchor="middle" className="card-brand-word">
          UP
        </text>
      )}
      {k !== 'visa' &&
        k !== 'mastercard' &&
        k !== 'amex' &&
        k !== 'discover' &&
        k !== 'diners' &&
        k !== 'jcb' &&
        k !== 'unionpay' && (
          <g fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
            <rect x="7" y="8" width="8" height="6" rx="1" />
            <path d="M18 10.5h12M18 15.5h8" />
          </g>
        )}
    </svg>
  )
}
