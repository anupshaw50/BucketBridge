import { expect, type Page, test } from "@playwright/test";
import { tr } from "../src/i18n";

const errors: string[] = [];

async function open(page: Page): Promise<void> {
  errors.length = 0;
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") {
      errors.push(message.text());
    }
  });
  await page.goto("/?latency=0");
  await expect(page.getByRole("toolbar", { name: tr("Main toolbar") })).toBeVisible();
}

test.afterEach(() => {
  expect(errors, "no console errors").toEqual([]);
});

test("right-click opens the file menu inside the window and Escape closes it", async ({ page }) => {
  await open(page);
  await page.getByRole("tree", { name: tr("Buckets and folders") }).getByText("demo-photos").click();
  const grid = page.getByRole("grid", { name: tr("Objects") });
  await grid.getByText("readme.md").click({ button: "right" });
  const menu = page.getByRole("menu", { name: tr("File menu") });
  await expect(menu).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: tr("Download…") })).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: tr("Paste") })).toBeDisabled();
  await page.keyboard.press("Escape");
  await expect(menu).toBeHidden();

  // At the bottom-right corner of the list the menu moves back inside the window.
  const size = page.viewportSize();
  const area = await grid.boundingBox();
  expect(size).not.toBeNull();
  expect(area).not.toBeNull();
  if (size === null || area === null) {
    return;
  }
  await grid.click({ button: "right", position: { x: area.width - 6, y: area.height - 6 } });
  const empty = page.getByRole("menu", { name: tr("Objects menu") });
  await expect(empty).toBeVisible();
  const box = await empty.boundingBox();
  expect(box).not.toBeNull();
  if (box !== null) {
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.y).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(size.width);
    expect(box.y + box.height).toBeLessThanOrEqual(size.height);
  }
  await page.keyboard.press("Escape");
});

test("the account menu opens above a dialog, runs an entry, and Escape leaves the dialog open", async ({ page }) => {
  await open(page);
  await page.getByRole("button", { name: tr("Accounts"), exact: true }).click();
  const dialog = page.getByRole("dialog", { name: tr("Accounts") });
  await dialog.getByText("Secure (MFA)").click({ button: "right" });
  const menu = page.getByRole("menu", { name: tr("Account menu") });
  await expect(menu).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(menu).toBeHidden();
  await expect(dialog).toBeVisible();

  await dialog.getByText("Secure (MFA)").click({ button: "right" });
  // Playwright only clicks an element that receives the click, so this proves the menu is above the dialog.
  await menu.getByRole("menuitem", { name: tr("Edit account…") }).click();
  await expect(
    page.getByRole("dialog", { name: tr("Edit account {label}", { label: "Secure (MFA)" }) }),
  ).toBeVisible();
});

test("keyboard navigation and type-ahead run an entry of the bucket menu", async ({ page }) => {
  await open(page);
  await page.getByRole("tree", { name: tr("Buckets and folders") }).getByText("demo-empty").click({ button: "right" });
  const menu = page.getByRole("menu", { name: tr("Bucket menu") });
  await expect(menu).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: tr("Open"), exact: true })).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(menu.getByRole("menuitem", { name: tr("Open in new window") })).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(menu.getByRole("menuitem", { name: tr("New folder…") })).toBeFocused();
  // The first letters of the label in the language of the interface. Playwright
  // cannot type non-Latin letters, so the keys the menu listens for are sent directly.
  const first = tr("Copy bucket name").slice(0, 2);
  await page.evaluate((text: string) => {
    for (const char of text) {
      document.activeElement?.dispatchEvent(new KeyboardEvent("keydown", { key: char, bubbles: true }));
    }
  }, first);
  await expect(menu.getByRole("menuitem", { name: tr("Copy bucket name") })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(menu).toBeHidden();
});

test("Search moves focus to the filter box and the closing menu does not take it back", async ({ page }) => {
  await open(page);
  await page.getByRole("tree", { name: tr("Buckets and folders") }).getByText("demo-photos").click();
  const grid = page.getByRole("grid", { name: tr("Objects") });
  await expect(grid.getByText("readme.md")).toBeVisible();
  const area = await grid.boundingBox();
  expect(area).not.toBeNull();
  if (area === null) {
    return;
  }
  await grid.click({ button: "right", position: { x: area.width - 60, y: area.height - 60 } });
  await page.getByRole("menu", { name: tr("Objects menu") }).getByRole("menuitem", { name: tr("Search") }).click();
  await expect(page.getByRole("textbox", { name: tr("Filter loaded items") })).toBeFocused();
});
