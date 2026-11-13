import './Logo.css'

export type LogoProps = {
  className?: string
  animated?: boolean
}

export function Logo({ className, animated = true }: LogoProps) {
  const classes = ['brr-logo', className].filter(Boolean).join(' ')

  return <span className={classes} role="img" aria-label="Brr" data-animated={animated}>
    <span className="brr-logo-word">Brr</span>
    <span className="brr-logo-dot" aria-hidden="true" />
  </span>
}
