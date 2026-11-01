import { expect, type Page, test } from "@playwright/test";

const errors: string[] = [];

async function open(page: Page, query = "?latency=0"): Promise<void> {
  errors.length = 0;
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") {
      errors.push(message.text());
    }
  });
  await page.goto(`/${query}`);
  await expect(page.getByRole("toolbar", { name: "Main toolbar" })).toBeVisible();
}

test.afterEach(() => {
  expect(errors, "no console errors").toEqual([]);
});

test("loads the app with the demo account", async ({ page }) => {
  await open(page);
  await expect(page).toHaveTitle("BucketBridge");
  await expect(page.getByRole("button", { name: "Account: Demo" })).toBeVisible();
  const tree = page.getByRole("tree", { name: "Buckets and folders" });
  await expect(tree.getByText("demo-photos")).toBeVisible();
  await expect(tree.getByText("CloudFront")).toBeVisible();
  await expect(page.getByRole("region", { name: "Transfer queue" })).toBeVisible();
  await expect(page.getByRole("contentinfo", { name: "Status bar" })).toContainText("Demo (us-east-1)");
});

test("adds an account with the wizard", async ({ page }) => {
  await open(page);
  await page.getByRole("button", { name: "Account: Demo" }).click();
  await page.getByRole("menuitem", { name: "Add account..." }).click();
  const dialog = page.getByRole("dialog", { name: "Add account" });
  await dialog.getByRole("textbox", { name: /Label/ }).fill("Smoke test");
  await dialog.getByRole("button", { name: "Next" }).click();
  await dialog.getByRole("textbox", { name: /Access key ID/ }).fill("MOCKNEWACCESSKEY0004");
  await dialog.getByLabel(/Secret access key/).fill("mock-new-secret-0004");
  await dialog.getByRole("button", { name: "Next" }).click();
  await expect(dialog.getByRole("group", { name: "Review" })).toContainText("Smoke test");
  await dialog.getByRole("button", { name: "Add account" }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByRole("button", { name: "Account: Smoke test" })).toBeVisible();
  await expect(page.getByText("This account has no buckets yet.")).toBeVisible();
});

test("opens a bucket and navigates into a folder", async ({ page }) => {
  await open(page);
  await page.getByRole("tree", { name: "Buckets and folders" }).getByText("demo-photos").click();
  const grid = page.getByRole("grid", { name: "Objects" });
  await expect(grid.getByText("readme.md")).toBeVisible();
  await grid.getByText("photos", { exact: true }).dblclick();
  await expect(grid.getByText("2024", { exact: true })).toBeVisible();
  await expect(page.getByRole("navigation", { name: "Location" })).toContainText("photos");
});

test("enqueues an upload through the file chooser and it completes", async ({ page }) => {
  await open(page);
  await page.getByRole("tree", { name: "Buckets and folders" }).getByText("demo-backups").click();
  await expect(page.getByRole("grid", { name: "Objects" })).toBeVisible();
  await page.getByRole("button", { name: "Upload", exact: true }).click();
  await page.getByRole("menuitem", { name: "Upload files..." }).click();
  const chooser = page.getByRole("dialog", { name: /Choose files to upload/ });
  await chooser.getByRole("listitem").filter({ hasText: "Documents" }).click();
  await chooser.getByRole("listitem").filter({ hasText: "report.pdf" }).click();
  await chooser.getByRole("button", { name: "Open" }).click();
  await expect(chooser).toBeHidden();
  const transfers = page.getByRole("list", { name: "Transfers" });
  const row = transfers.getByRole("listitem").filter({ hasText: "report.pdf" });
  await expect(row).toBeVisible();
  await expect(row.getByText("Completed")).toBeVisible();
  await expect(page.getByRole("grid", { name: "Objects" }).getByText("report.pdf")).toBeVisible();
});

test("changes a setting and the theme follows", async ({ page }) => {
  await open(page);
  await page.getByRole("button", { name: "Settings" }).click();
  const dialog = page.getByRole("dialog", { name: "Settings" });
  await dialog.getByRole("button", { name: "Interface" }).click();
  await dialog.getByText("Dark", { exact: true }).click();
  await expect(page.locator("html")).toHaveAttribute("data-mantine-color-scheme", "dark");
  await dialog.getByRole("button", { name: "OK" }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByRole("contentinfo", { name: "Status bar" })).toContainText("Settings saved.");
  await page.getByRole("button", { name: "Settings" }).click();
  await page.getByRole("dialog", { name: "Settings" }).getByRole("button", { name: "Interface" }).click();
  await expect(page.getByRole("dialog", { name: "Settings" }).getByRole("radio", { name: "Dark" })).toBeChecked();
});
