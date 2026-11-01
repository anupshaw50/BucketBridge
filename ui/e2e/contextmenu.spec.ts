import { expect, type Page, test } from "@playwright/test";

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
  await expect(page.getByRole("toolbar", { name: "Main toolbar" })).toBeVisible();
}

test.afterEach(() => {
  expect(errors, "no console errors").toEqual([]);
});

test("right-click opens the file menu inside the window and Escape closes it", async ({ page }) => {
  await open(page);
  await page.getByRole("tree", { name: "Buckets and folders" }).getByText("demo-photos").click();
  const grid = page.getByRole("grid", { name: "Objects" });
  await grid.getByText("readme.md").click({ button: "right" });
  const menu = page.getByRole("menu", { name: "File menu" });
  await expect(menu).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "Download…" })).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "Paste" })).toBeDisabled();
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
  const empty = page.getByRole("menu", { name: "Objects menu" });
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
  await page.getByRole("button", { name: "Accounts" }).click();
  const dialog = page.getByRole("dialog", { name: "Accounts" });
  await dialog.getByText("Secure (MFA)").click({ button: "right" });
  const menu = page.getByRole("menu", { name: "Account menu" });
  await expect(menu).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(menu).toBeHidden();
  await expect(dialog).toBeVisible();

  await dialog.getByText("Secure (MFA)").click({ button: "right" });
  // Playwright only clicks an element that receives the click, so this proves the menu is above the dialog.
  await menu.getByRole("menuitem", { name: "Edit account…" }).click();
  await expect(page.getByRole("dialog", { name: "Edit account Secure (MFA)" })).toBeVisible();
});

test("keyboard navigation and type-ahead run an entry of the bucket menu", async ({ page }) => {
  await open(page);
  await page.getByRole("tree", { name: "Buckets and folders" }).getByText("demo-empty").click({ button: "right" });
  const menu = page.getByRole("menu", { name: "Bucket menu" });
  await expect(menu).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "Open" })).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(menu.getByRole("menuitem", { name: "New folder…" })).toBeFocused();
  await page.keyboard.type("copy b");
  await expect(menu.getByRole("menuitem", { name: "Copy bucket name" })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(menu).toBeHidden();
});

test("Search moves focus to the filter box and the closing menu does not take it back", async ({ page }) => {
  await open(page);
  await page.getByRole("tree", { name: "Buckets and folders" }).getByText("demo-photos").click();
  const grid = page.getByRole("grid", { name: "Objects" });
  await expect(grid.getByText("readme.md")).toBeVisible();
  const area = await grid.boundingBox();
  expect(area).not.toBeNull();
  if (area === null) {
    return;
  }
  await grid.click({ button: "right", position: { x: area.width - 60, y: area.height - 60 } });
  await page.getByRole("menu", { name: "Objects menu" }).getByRole("menuitem", { name: "Search" }).click();
  await expect(page.getByRole("textbox", { name: "Filter loaded items" })).toBeFocused();
});
