import * as Tooltip from '@radix-ui/react-tooltip';
import type { ButtonHTMLAttributes, ReactNode } from 'react';

export function Button(props: ButtonHTMLAttributes<HTMLButtonElement>) {
  return <button type={props.type ?? 'button'} {...props} />;
}

export function IconButton({
  label,
  children,
  className = 'cfg-icon-btn',
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { label: string; children: ReactNode }) {
  return (
    <button type="button" className={className} title={label} aria-label={label} {...props}>
      {children}
    </button>
  );
}

export function HelpTip({ children, tip }: { children: ReactNode; tip?: string }) {
  if (!tip) return <>{children}</>;
  return (
    <Tooltip.Provider delayDuration={250}>
      <Tooltip.Root>
        <Tooltip.Trigger asChild>{children}</Tooltip.Trigger>
        <Tooltip.Portal>
          <Tooltip.Content className="tooltip" sideOffset={6}>
            {tip}
            <Tooltip.Arrow className="tooltip-arrow" />
          </Tooltip.Content>
        </Tooltip.Portal>
      </Tooltip.Root>
    </Tooltip.Provider>
  );
}

export function Notice({ children, kind }: { children: ReactNode; kind?: 'ok' | 'err' }) {
  return <div className={`setup-status${kind ? ` status-${kind}` : ''}`}>{children}</div>;
}
