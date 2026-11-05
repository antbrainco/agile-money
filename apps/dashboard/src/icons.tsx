// Agile Money's own glyphs: a 24 grid, 1.6 stroke, round caps, drawn to sit together.
// Colour comes from `currentColor`, size from the `.icon` class.

const PATHS = {
  // Rounded tile with a tick: something waiting for your yes.
  approvals: (
    <>
      <rect x="3.5" y="3.5" width="17" height="17" rx="5" />
      <path d="M8.5 12.2l2.4 2.4 4.6-5" />
    </>
  ),
  // Money out and money in.
  activity: <path d="M5 8.5h12.5M14.5 5.5l3 3-3 3M19 15.5H6.5M9.5 12.5l-3 3 3 3" />,
  // Arrow dropping into a tray.
  requests: (
    <>
      <path d="M12 3.5v9.5M8.5 9.5L12 13l3.5-3.5" />
      <path d="M4 13.5v3a3 3 0 003 3h10a3 3 0 003-3v-3" />
    </>
  ),
  contacts: (
    <>
      <circle cx="12" cy="8.5" r="3.5" />
      <path d="M5 19.5c.8-3.2 3.6-5 7-5s6.2 1.8 7 5" />
    </>
  ),
  // A gauge with its needle short of the end.
  limits: (
    <>
      <path d="M4.2 16.5a8 8 0 1115.6 0" />
      <path d="M12 14.5l3.5-4" />
      <circle cx="12" cy="15" r="1" />
    </>
  ),
  // Shield with a keyhole.
  security: (
    <>
      <path d="M12 3.5l7 2.6v5.2c0 4.3-2.9 7.7-7 9.2-4.1-1.5-7-4.9-7-9.2V6.1z" />
      <circle cx="12" cy="10.8" r="1.6" />
      <path d="M12 12.4v2.8" />
    </>
  ),
  logout: (
    <>
      <path d="M10 4.5H7a2.5 2.5 0 00-2.5 2.5v10A2.5 2.5 0 007 19.5h3" />
      <path d="M14 8l4 4-4 4M18 12H9.5" />
    </>
  ),
  pause: <path d="M9 6.5v11M15 6.5v11" />,
  play: <path d="M8 5.8v12.4a.6.6 0 00.9.5l9.6-6.2a.6.6 0 000-1L8.9 5.3a.6.6 0 00-.9.5z" />,
  check: <path d="M5.5 12.5l4 4 9-9.5" />,
  close: <path d="M6.5 6.5l11 11M17.5 6.5l-11 11" />,
  // A shop front: awning, door.
  store: (
    <>
      <path d="M4 9l1.6-4.5h12.8L20 9" />
      <path d="M4 9a2.7 2.7 0 005.3 0 2.7 2.7 0 005.4 0A2.7 2.7 0 0020 9" />
      <path d="M5.5 11.5v8h13v-8M10 19.5v-4.5h4v4.5" />
    </>
  ),
  copy: (
    <>
      <rect x="8.5" y="8.5" width="11" height="11" rx="3" />
      <path d="M15.5 5.5A2 2 0 0013.5 4.5h-6a3 3 0 00-3 3v6a2 2 0 001 1.7" />
    </>
  ),
  eye: (
    <>
      <path d="M2.8 12C4.7 8 8 6 12 6s7.3 2 9.2 6c-1.9 4-5.2 6-9.2 6s-7.3-2-9.2-6z" />
      <circle cx="12" cy="12" r="2.8" />
    </>
  ),
  eyeOff: (
    <>
      <path d="M9.9 6.3A9 9 0 0112 6c4 0 7.3 2 9.2 6a12 12 0 01-2.4 3.3M14 17.7a9 9 0 01-2 .3c-4 0-7.3-2-9.2-6a12 12 0 013.3-4" />
      <path d="M4 4l16 16" />
    </>
  ),
  info: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M12 11v5M12 7.8v.2" />
    </>
  ),
  clock: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M12 7.5V12l3 2" />
    </>
  ),
  key: (
    <>
      <circle cx="8" cy="15.5" r="3.5" />
      <path d="M10.5 13l8-8M15.5 8l2.5 2.5M17.5 6l1.5 1.5" />
    </>
  ),
  profile: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <circle cx="12" cy="10" r="2.8" />
      <path d="M6.8 18.2c1.2-2 3-3 5.2-3s4 1 5.2 3" />
    </>
  ),
  // A billfold with its clasp.
  wallet: (
    <>
      <path d="M17.5 7.5V6a2 2 0 00-2-2H6.5a3 3 0 00-3 3" />
      <rect x="3.5" y="7.5" width="17" height="12" rx="3" />
      <path d="M20.5 11.5h-3.5a2 2 0 000 4h3.5" />
    </>
  ),
  // A paper plane: messages.
  send: (
    <>
      <path d="M20.5 3.5L3.5 10.3l6.4 2.6 2.6 6.6z" />
      <path d="M20.5 3.5l-10.6 9.4" />
    </>
  ),
  plus: <path d="M12 5.5v13M5.5 12h13" />,
} as const;

export type IconName = keyof typeof PATHS;

export function Icon({ name }: { name: IconName }) {
  return (
    <svg
      className="icon"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      {PATHS[name]}
    </svg>
  );
}
