import { describe, expect, it, vi } from "vitest";
import { estimateStrength, generatePassword, randomBelow, type RandomSource } from "./password";

describe("password generator", () => {
  it("uses crypto.getRandomValues and honours length and symbols", () => {
    const spy = vi.spyOn(crypto, "getRandomValues");
    const withSymbols = generatePassword({ length: 32, symbols: true });
    expect(spy).toHaveBeenCalled();
    expect(withSymbols).toHaveLength(32);
    expect(withSymbols).toMatch(/[a-z]/);
    expect(withSymbols).toMatch(/[A-Z]/);
    expect(withSymbols).toMatch(/[0-9]/);
    expect(withSymbols).toMatch(/[^A-Za-z0-9]/);
    const plain = generatePassword({ length: 16, symbols: false });
    expect(plain).toMatch(/^[A-Za-z0-9]{16}$/);
  });

  it("clamps the length to 8..128", () => {
    expect(generatePassword({ length: 3, symbols: false })).toHaveLength(8);
    expect(generatePassword({ length: 1000, symbols: false })).toHaveLength(128);
  });

  it("produces different passwords", () => {
    const seen = new Set(Array.from({ length: 50 }, () => generatePassword({ length: 20, symbols: true })));
    expect(seen.size).toBe(50);
  });

  it("rejects random values above the unbiased limit", () => {
    // 2^32 is not a multiple of 3, so 0xffffffff must be rejected and the next value used.
    const values = [0xffffffff, 7];
    const source: RandomSource = (buffer) => {
      buffer[0] = values.shift() ?? 0;
      return buffer;
    };
    expect(randomBelow(3, source)).toBe(1);
    expect(() => randomBelow(0)).toThrow(RangeError);
  });
});

describe("strength estimate", () => {
  it("rates common, short and long mixed passwords", () => {
    expect(estimateStrength("").score).toBe(0);
    expect(estimateStrength("password").score).toBe(0);
    expect(estimateStrength("aaaaaaaaaaaa").score).toBeLessThan(2);
    const weak = estimateStrength("abc12345");
    expect(weak.score).toBeLessThan(3);
    expect(weak.hint).toMatch(/12 characters/);
    const strong = estimateStrength("tW9!qL2#vR7$xM4%");
    expect(strong.score).toBe(4);
    expect(strong.hint).toBeNull();
  });
});
