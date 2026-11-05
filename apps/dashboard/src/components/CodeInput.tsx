/** Six digits from the owner's authenticator app. */
export function CodeInput({
  value,
  onChange,
  autoFocus,
  invalid,
}: {
  value: string;
  onChange: (code: string) => void;
  autoFocus?: boolean;
  invalid?: boolean;
}) {
  return (
    <input
      className="code"
      inputMode="numeric"
      autoComplete="one-time-code"
      maxLength={6}
      placeholder="000000"
      aria-label="Authenticator code"
      aria-invalid={invalid}
      autoFocus={autoFocus}
      value={value}
      onChange={(e) => onChange(e.target.value.replace(/\D/g, "").slice(0, 6))}
    />
  );
}
