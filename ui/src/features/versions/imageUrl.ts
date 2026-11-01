/** An address an `<img>` can show, and how to release it. */
export interface ImageSource {
  url: string;
  revoke: () => void;
}

const IMAGE_TYPE = /^image\/[a-z0-9.+-]+$/i;

/**
 * Turns the base64 bytes of an image into a `blob:` URL (the web view's content
 * security policy allows `blob:` and `data:` images). Where blob URLs are not
 * available it falls back to a `data:` URL. Only `image/*` types are accepted, so
 * object content can never become any other kind of document.
 */
export function imageSource(base64: string, mediaType: string): ImageSource | null {
  if (!IMAGE_TYPE.test(mediaType)) {
    return null;
  }
  try {
    if (typeof URL.createObjectURL === "function") {
      const bytes = Uint8Array.from(atob(base64), (char) => char.charCodeAt(0));
      const url = URL.createObjectURL(new Blob([bytes], { type: mediaType }));
      return { url, revoke: () => URL.revokeObjectURL(url) };
    }
  } catch {
    // Fall through to a data URL.
  }
  return { url: `data:${mediaType};base64,${base64}`, revoke: () => undefined };
}
