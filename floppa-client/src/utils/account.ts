/** What changing accounts asks of the tunnel. */
export type AccountChange =
  /** Nothing to do: the same account, or the first sign-in of the session. */
  | 'none'
  /** Signed out: forget everything this device holds for that account. */
  | 'forget'
  /** Signed in as somebody else: forget it, and stop answering questions about the old user. */
  | 'switch'

/**
 * What to do when the signed-in account changes.
 *
 * The stored tunnel configs belong to an account: private keys, a VLESS URI carrying a user's
 * UUID, and an autostart bundle the `:vpn` service can bring back on its own. Leaving them
 * behind on a sign-out meant a second account on the same phone that could not reach the server
 * connected under the first account's identity, and that always-on could restore a logged-out
 * account's tunnel with nobody signed in.
 *
 * A first sign-in is deliberately `none`: the common case is the same person signing back in,
 * and the peer sync replaces the configs anyway. There is nothing here that knows whose keys
 * these were, so wiping on every sign-in would cost a working offline connect to prevent
 * nothing.
 */
export function accountChange(
  previous: number | null | undefined,
  next: number | null,
): AccountChange {
  if (previous == null || next === previous) return 'none'
  return next === null ? 'forget' : 'switch'
}
