import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import { findUploadRule, globToRegExp, headerLines, newRule, parseHeaderLines } from "../../lib/uploadRules";
import { DEFAULT_SETTINGS } from "../../lib/settings";
import { MockBackend } from "../../mock/mockBackend";
import { renderApp } from "../../test/renderApp";

/** Regex that matches the given text with anything in the place of a {placeholder}. */
function pattern(template: string, vars?: Record<string, string | number>): RegExp {
  const text = tr(template, vars).replace(/\{(\w+)\}/g, "§§§");
  return new RegExp(text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&").replace(/§§§/g, ".*"));
}

describe("upload rule matching", () => {
  it("matches file names, whole keys and case like the core", () => {
    const settings = { enabled: true, rules: [{ ...newRule("*.jpg") }, { ...newRule("assets/*.css") }, { ...newRule("**/*.min.js") }] };
    expect(findUploadRule(settings, "a/b/Photo.JPG").index).toBe(0);
    expect(findUploadRule(settings, "assets/site.css").index).toBe(1);
    expect(findUploadRule(settings, "other/assets/site.css").index).toBeNull();
    expect(findUploadRule(settings, "x/y/app.min.js").index).toBe(2);
    const strict = { enabled: true, rules: [{ ...newRule("*.jpg"), case_insensitive: false }] };
    expect(findUploadRule(strict, "A.JPG").index).toBeNull();
    expect(findUploadRule(strict, "a.jpg").index).toBe(0);
  });

  it("skips disabled rules and keeps their position numbers", () => {
    const settings = { enabled: true, rules: [{ ...newRule("*"), enabled: false }, newRule("*")] };
    expect(findUploadRule(settings, "a").index).toBe(1);
  });

  it("supports classes and alternatives, and rejects broken patterns", () => {
    expect(globToRegExp("file[0-9].{png,gif}", true)?.test("file7.GIF")).toBe(true);
    expect(globToRegExp("[a", true)).toBeNull();
    expect(globToRegExp("{a,b", true)).toBeNull();
  });

  it("reads and writes header lines", () => {
    expect(parseHeaderLines("X-Amz-Meta-Team: web\n\nContent-Language: en")).toEqual({
      headers: { "X-Amz-Meta-Team": "web", "Content-Language": "en" },
      badLine: null,
    });
    expect(parseHeaderLines("ok: 1\nbroken").badLine).toBe(2);
    expect(headerLines({ A: "1", B: "2" })).toBe("A: 1\nB: 2");
  });
});

describe("upload rules mock and settings", () => {
  it("tests a name against the given rules, and keeps the rules in the settings", async () => {
    const mock = new MockBackend();
    const rules = { enabled: false, rules: [{ ...newRule("*.png"), cache_control: "max-age=60" }, newRule("*.txt")] };
    const hit = await mock.test_upload_rules({ rules, name: "dir/x.TXT" });
    expect(hit.index).toBe(1);
    expect((await mock.test_upload_rules({ rules, name: "x.bin" })).rule).toBeNull();
    await expect(mock.test_upload_rules({ rules: { enabled: true, rules: [newRule("")] }, name: "x" })).rejects.toMatchObject({ code: "usage" });

    const settings = await mock.get_settings();
    expect(settings.upload_rules).toEqual(DEFAULT_SETTINGS.upload_rules);
    settings.upload_rules = rules;
    await mock.save_settings({ settings });
    expect((await mock.get_settings()).upload_rules.rules).toHaveLength(2);
    settings.upload_rules = { enabled: true, rules: [newRule("  ")] };
    await expect(mock.save_settings({ settings })).rejects.toMatchObject({ code: "usage" });
  });
});

describe("upload rules page", () => {
  it("adds, edits, orders, tests and saves a rule", async () => {
    const { user, mock } = await renderApp();
    await user.click(await screen.findByRole("button", { name: tr("Settings") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Settings") });
    await user.click(within(dialog).getByRole("button", { name: tr("Upload rules") }));
    expect(within(dialog).getByText(pattern("No rules yet. Add a rule such as *.html to set a content type or a cache policy."))).toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: tr("Add rule") }));
    await user.type(within(dialog).getByRole("textbox", { name: tr("File pattern") }), "*.html");
    await user.type(within(dialog).getByRole("textbox", { name: tr("Cache-Control") }), "max-age=300");
    await user.click(within(dialog).getByRole("button", { name: tr("Add rule") }));
    await user.type(within(dialog).getByRole("textbox", { name: tr("File pattern") }), "*");
    await user.type(within(dialog).getByRole("textbox", { name: tr("Cache-Control") }), "no-store");

    // Move the catch-all rule above the other one: it then wins for everything.
    await user.click(within(dialog).getByRole("button", { name: tr("Move rule {number} up", { number: 2 }) }));
    await user.type(within(dialog).getByRole("textbox", { name: tr("Test a file name") }), "index.html");
    expect(await within(dialog).findByText(pattern("Rule {number} matches: {pattern}", { number: 1, pattern: "*" }))).toBeInTheDocument();
    expect(within(dialog).getByText("Cache-Control: no-store")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("checkbox", { name: tr("Rule {number} is on", { number: 1 }) }));
    expect(await within(dialog).findByText(pattern("Rule {number} matches: {pattern}", { number: 2, pattern: "*.html" }))).toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: tr("OK") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Settings") })).toBeNull());
    const saved = (await mock.get_settings()).upload_rules;
    expect(saved.rules.map((rule) => [rule.pattern, rule.enabled, rule.cache_control])).toEqual([
      ["*", false, "no-store"],
      ["*.html", true, "max-age=300"],
    ]);
  });

  it("refuses to save a rule without a pattern", async () => {
    const { user } = await renderApp();
    await user.click(await screen.findByRole("button", { name: tr("Settings") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Settings") });
    await user.click(within(dialog).getByRole("button", { name: tr("Upload rules") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Add rule") }));
    await user.click(within(dialog).getByRole("button", { name: tr("OK") }));
    expect(await within(dialog).findAllByText(tr("Rule {number} has no file pattern.", { number: 1 }))).not.toHaveLength(0);
  });
});
