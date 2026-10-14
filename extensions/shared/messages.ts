// Keep browser-provided sender metadata separate from untrusted message bodies.
export function ownPage(
  sender: { id?: string; url?: string },
  id: string,
  prefix: string,
): boolean {
  return sender.id === id && Boolean(sender.url?.startsWith(prefix));
}
export function checkedReply(result: unknown): unknown {
  if (
    result &&
    typeof result === "object" &&
    "error" in result &&
    result.error !== null
  )
    throw new Error(String(result.error));
  return result;
}
