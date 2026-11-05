export function Avatar({ name }: { name: string }) {
  return (
    <span className="avatar" aria-hidden>
      {name.trim().charAt(0).toUpperCase() || "?"}
    </span>
  );
}
