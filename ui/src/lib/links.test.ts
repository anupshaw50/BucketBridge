import { describe, expect, it } from "vitest";
import { checkHost, encodeKeyPath, htmlSnippet, isImageKey, markdownSnippet, publicLink } from "./links";

describe("public links", () => {
  it("encodes each segment of the key and keeps the slashes", () => {
    expect(encodeKeyPath("photos/2026/my cat (1).jpg")).toBe("photos/2026/my%20cat%20%281%29.jpg");
    expect(encodeKeyPath("a//b?c#d")).toBe("a//b%3Fc%23d");
    expect(encodeKeyPath("é/ü")).toBe("%C3%A9/%C3%BC");
  });

  it("builds the link on the host", () => {
    expect(publicLink("d111111abcdef8.cloudfront.net", "img/a b.png")).toBe("https://d111111abcdef8.cloudfront.net/img/a%20b.png");
  });

  it("reads a host name and refuses what is not one", () => {
    expect(checkHost(" https://CDN.Example.com/ ")).toEqual({ ok: true, host: "cdn.example.com" });
    expect(checkHost("cdn.example.com:8443")).toEqual({ ok: true, host: "cdn.example.com:8443" });
    expect(checkHost("")).toEqual({ ok: false, problem: "empty" });
    for (const bad of ["cdn.example.com/path", "a b.com", "user@cdn.example.com", "-bad.example.com", "cdn..example.com", "cdn.example.com:0", "cdn.example.com:70000", "http://cdn.example.com", "x?y"]) {
      expect(checkHost(bad), bad).toEqual({ ok: false, problem: "invalid" });
    }
  });

  it("knows images by extension", () => {
    expect(isImageKey("a/b.PNG")).toBe(true);
    expect(isImageKey("a/b.pdf")).toBe(false);
    expect(isImageKey("png")).toBe(false);
  });

  it("writes HTML snippets with escaped text", () => {
    expect(htmlSnippet("https://h/a%20b.png?x=1&y=2", "dir/a b.png")).toBe('<img src="https://h/a%20b.png?x=1&amp;y=2" alt="a b.png">');
    expect(htmlSnippet("https://h/r.pdf", 'x/"r" <1>.pdf')).toBe('<a href="https://h/r.pdf">&quot;r&quot; &lt;1&gt;.pdf</a>');
  });

  it("writes Markdown snippets", () => {
    expect(markdownSnippet("https://h/a.png", "dir/a.png")).toBe("![a.png](https://h/a.png)");
    expect(markdownSnippet("https://h/r(1).pdf", "[r].pdf")).toBe("[\\[r\\].pdf](https://h/r%281%29.pdf)");
  });
});
