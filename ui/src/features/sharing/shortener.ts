import type { ShortenerProvider } from "../../api/types";

/** Display name of a shortening service. */
export function shortenerName(provider: ShortenerProvider): string {
  switch (provider) {
    case "bitly":
      return "Bitly";
    case "tinyurl":
      return "TinyURL";
    case "none":
      return "";
  }
}
