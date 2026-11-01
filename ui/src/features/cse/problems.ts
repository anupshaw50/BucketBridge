/** The two password states, told apart by the message the core sends. */
export function passwordProblem(message: string): string | null {
  if (message.includes("password not found")) {
    return "Password not found: this object is encrypted and no password was given.";
  }
  if (message.includes("password not valid")) {
    return "Password not valid: it does not open this object.";
  }
  return null;
}
