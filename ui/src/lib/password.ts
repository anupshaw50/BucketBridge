const LOWER = "abcdefghijklmnopqrstuvwxyz";
const UPPER = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const DIGITS = "0123456789";
const SYMBOLS = "!#$%&()*+,-./:;<=>?@[]^_{}~";

export const PASSWORD_MIN_LENGTH = 8;
export const PASSWORD_MAX_LENGTH = 128;

export interface GeneratorOptions {
  length: number;
  symbols: boolean;
}

/** Fills the array with cryptographically strong random values. */
export type RandomSource = (buffer: Uint32Array<ArrayBuffer>) => Uint32Array<ArrayBuffer>;

const cryptoSource: RandomSource = (buffer) => crypto.getRandomValues(buffer);

/** Uniform random integer in [0, max) using rejection sampling (no modulo bias). */
export function randomBelow(max: number, random: RandomSource = cryptoSource): number {
  if (!Number.isInteger(max) || max <= 0 || max > 2 ** 32) {
    throw new RangeError(`max must be an integer between 1 and 2^32, got ${max}`);
  }
  const limit = Math.floor(2 ** 32 / max) * max;
  const buffer = new Uint32Array(1);
  for (;;) {
    const value = random(buffer)[0] ?? 0;
    if (value < limit) {
      return value % max;
    }
  }
}

/**
 * Generates a password with crypto.getRandomValues. Every enabled character
 * class (lower, upper, digits, symbols) appears at least once.
 */
export function generatePassword(options: GeneratorOptions, random: RandomSource = cryptoSource): string {
  const length = Math.max(PASSWORD_MIN_LENGTH, Math.min(PASSWORD_MAX_LENGTH, Math.floor(options.length)));
  const classes = options.symbols ? [LOWER, UPPER, DIGITS, SYMBOLS] : [LOWER, UPPER, DIGITS];
  const alphabet = classes.join("");
  const chars: string[] = classes.map((set) => set.charAt(randomBelow(set.length, random)));
  while (chars.length < length) {
    chars.push(alphabet.charAt(randomBelow(alphabet.length, random)));
  }
  // Fisher-Yates shuffle so the guaranteed characters are not at the start.
  for (let index = chars.length - 1; index > 0; index -= 1) {
    const swap = randomBelow(index + 1, random);
    const current = chars[index] ?? "";
    chars[index] = chars[swap] ?? "";
    chars[swap] = current;
  }
  return chars.join("");
}

export type StrengthScore = 0 | 1 | 2 | 3 | 4;

export interface Strength {
  score: StrengthScore;
  label: string;
  bits: number;
  hint: string | null;
}

const COMMON = new Set([
  "password",
  "password1",
  "passw0rd",
  "123456",
  "12345678",
  "123456789",
  "1234567890",
  "qwerty",
  "qwertyuiop",
  "letmein",
  "welcome",
  "admin",
  "iloveyou",
  "monkey",
  "dragon",
  "abc123",
  "111111",
  "000000",
  "changeme",
  "secret",
]);

const LABELS: Record<StrengthScore, string> = {
  0: "Very weak",
  1: "Weak",
  2: "Fair",
  3: "Strong",
  4: "Very strong",
};

/** Rough entropy estimate used for the strength hint next to password fields. */
export function estimateStrength(password: string): Strength {
  if (password.length === 0) {
    return { score: 0, label: LABELS[0], bits: 0, hint: "Enter a password." };
  }
  const characters = Array.from(password);
  let pool = 0;
  if (/[a-z]/.test(password)) pool += 26;
  if (/[A-Z]/.test(password)) pool += 26;
  if (/[0-9]/.test(password)) pool += 10;
  if (/[^a-zA-Z0-9]/.test(password)) pool += 33;
  const unique = new Set(characters).size;
  // Repeated characters add little; count each distinct character fully and repeats at a quarter.
  const effectiveLength = unique + (characters.length - unique) * 0.25;
  let bits = effectiveLength * Math.log2(Math.max(pool, 2));
  if (COMMON.has(password.toLowerCase())) {
    bits = Math.min(bits, 10);
  }
  let score: StrengthScore;
  if (bits < 28) score = 0;
  else if (bits < 40) score = 1;
  else if (bits < 60) score = 2;
  else if (bits < 80) score = 3;
  else score = 4;
  let hint: string | null = null;
  if (score < 3) {
    hint =
      characters.length < 12
        ? "Use at least 12 characters, or generate a password."
        : "Mix upper and lower case letters, digits and symbols, or generate a password.";
  }
  return { score, label: LABELS[score], bits: Math.round(bits), hint };
}
