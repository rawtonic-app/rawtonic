import clsx from 'clsx';
import type { ButtonHTMLAttributes, ReactNode, Ref } from 'react';

interface ButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'children' | 'size'> {
  children: ReactNode;
  ref?: Ref<HTMLButtonElement>;
  size?: string;
  variant?: string;
}

/** A studio button (studio.css `.studio-button`): the default action is
 * filled, `ghost` is a plain push button, and an action that cannot be undone
 * (Delete Permanently, …) is red. */
const Button = ({ children, className, size: _size, variant, ...props }: ButtonProps) => (
  <button
    className={clsx(
      'studio-button',
      variant === 'destructive' ? 'is-destructive' : variant !== 'ghost' && 'is-primary',
      className,
    )}
    {...props}
  >
    {children}
  </button>
);

export default Button;
