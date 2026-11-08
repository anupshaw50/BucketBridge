import { afterEach, describe, expect, it } from "vitest";
import { en } from "./en";
import { hasTranslation, setLocale, tr } from "./index";
import { zhMenu } from "./zh/menu";

afterEach(() => {
  setLocale("zh-CN");
});

describe("tr", () => {
  it("answers in Chinese in the language of the interface", () => {
    expect(tr("Open")).toBe(zhMenu["action.open"]);
    expect(tr("BucketBridge could not start: {message}", { message: "boom" })).toBe(
      "BucketBridge 启动失败：boom",
    );
  });

  it("keeps the English source when the interface is English", () => {
    setLocale("en");
    expect(tr("Open")).toBe("Open");
    expect(tr("BucketBridge could not start: {message}", { message: "boom" })).toBe(
      "BucketBridge could not start: boom",
    );
  });

  it("returns the source itself when a sentence has no translation yet", () => {
    setLocale("en");
    expect(tr("Not translated at all")).toBe("Not translated at all");
    setLocale("zh-CN");
    expect(tr("Not translated at all")).toBe("Not translated at all");
  });

  it("fills the {name} placeholders and leaves unknown ones alone", () => {
    setLocale("en");
    expect(tr("{a} of {b}", { a: 1, b: 2 })).toBe("1 of 2");
    expect(tr("{a} of {b}", { a: 1 })).toBe("1 of {b}");
  });

  it("registers the menu labels, so they are translatable by their English text", () => {
    for (const [key, text] of Object.entries(en)) {
      expect(hasTranslation(text), key).toBe(true);
    }
  });
});
