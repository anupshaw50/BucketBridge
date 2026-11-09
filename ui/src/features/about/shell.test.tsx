import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import { openBucket, renderApp } from "../../test/renderApp";

describe("crash reporter", () => {
  it("shows the report, copies it, reveals the file and dismisses it", async () => {
    const { user, mock, platform } = await renderApp({ mock: { crashReport: true } });
    const dialog = await screen.findByRole("dialog", { name: tr("BucketBridge closed unexpectedly") });
    const report = await mock.last_crash_report();
    if (report === null) throw new Error("fixture missing");
    expect(within(dialog).getByLabelText(tr("Crash report contents"))).toHaveTextContent("panicked at");
    expect(within(dialog).getByRole("textbox", { name: tr("Report file") })).toHaveValue(report.path);
    expect(
      within(dialog).getByText(
        new RegExp("(.|\\n)*" + tr("A report was saved on this computer. It has not been sent anywhere; if you want to report the problem, copy it into an issue yourself.")),
      ),
    ).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Copy report") }));
    expect(platform.copied).toContain(report.contents);
    await user.click(within(dialog).getByRole("button", { name: tr("Show in folder") }));
    expect(platform.revealed).toEqual([report.path]);
    await user.click(within(dialog).getByRole("button", { name: tr("Dismiss report") }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: tr("BucketBridge closed unexpectedly") })).not.toBeInTheDocument(),
    );
    expect(await mock.last_crash_report()).toBeNull();
  });

  it("does not show a dialog when there is no report", async () => {
    await renderApp();
    await screen.findByText("demo-photos");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });
});

describe("about dialog", () => {
  it("shows version and license and opens only project links", async () => {
    const { user, platform } = await renderApp();
    await user.click(await screen.findByRole("button", { name: tr("About") }));
    const dialog = await screen.findByRole("dialog", { name: tr("About BucketBridge") });
    expect(
      await within(dialog).findByText(
        tr("Version {version} · {license} license", { version: "0.3.1", license: "GPL-3.0-only" }),
      ),
    ).toBeInTheDocument();
    expect(
      within(dialog).getByText(
        tr("An open source S3 client for Windows, macOS and Linux. It is free to use: no activation, registration or license key is required, and it never contacts any server on its own. Checking for updates only opens the releases page in your browser."),
      ),
    ).toBeInTheDocument();
    // Both the copyright notice and the full GPL text mention it.
    expect(within(dialog).getAllByText(/GNU General Public License as published/).length).toBeGreaterThanOrEqual(2);
    expect(within(dialog).getByText(tr("Created and maintained by {name}", { name: "Anup Shaw" }))).toBeInTheDocument();
    // The full licence text, not only the notice.
    expect(within(dialog).getByLabelText("GNU General Public License version 3")).toHaveTextContent(/TERMS AND CONDITIONS/);
    expect(within(dialog).getByLabelText("GNU General Public License version 3")).toHaveTextContent(/END OF TERMS AND CONDITIONS/);
    for (const name of [
      tr("Source code"),
      tr("Online help"),
      tr("Contact and issues"),
      tr("Check for updates"),
      tr("{name} on GitHub", { name: "Anup Shaw" }),
      "LinkedIn",
      tr("Website"),
      tr("Sponsor"),
    ]) {
      await user.click(within(dialog).getByRole("button", { name }));
    }
    expect(platform.opened).toEqual([
      "https://github.com/anupshaw50/BucketBridge",
      "https://github.com/anupshaw50/BucketBridge/tree/main/docs",
      "https://github.com/anupshaw50/BucketBridge/issues",
      "https://github.com/anupshaw50/BucketBridge/releases",
      "https://github.com/anupshaw50",
      "https://www.linkedin.com/in/anup-kumar-shaw/",
      "https://anupshaw50.github.io/BucketBridge/",
      "https://buymeacoffee.com/anupshaw50b",
    ]);
  });
});

describe("bucket dialogs", () => {
  it("creates a bucket after checking the S3 naming rules", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: tr("New bucket") }));
    const dialog = await screen.findByRole("dialog", { name: tr("New bucket") });
    const name = within(dialog).getByRole("textbox", { name: new RegExp(tr("Bucket name")) });
    await user.type(name, "Bad_Name");
    expect(within(dialog).getByText(tr("Use only lowercase letters, digits, dots (.) and hyphens (-)."))).toBeInTheDocument();
    await user.clear(name);
    await user.type(name, "demo-photos");
    await user.click(within(dialog).getByRole("button", { name: tr("Create bucket") }));
    expect(await within(dialog).findByText(/already own a bucket/)).toBeInTheDocument();
    await user.clear(name);
    await user.type(name, "fresh-bucket");
    await user.type(within(dialog).getByRole("textbox", { name: tr("Region (optional)") }), "eu-west-1");
    await user.click(within(dialog).getByRole("button", { name: tr("Create bucket") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("New bucket") })).not.toBeInTheDocument());
    expect(await screen.findByText(tr("This folder is empty"))).toBeInTheDocument();
    const buckets = await mock.list_buckets({ account_id: (await mock.list_accounts()).default_account_id ?? "" });
    expect(buckets.find((bucket) => bucket.name === "fresh-bucket")?.region).toBe("eu-west-1");
  });

  it("requires the typed name and reports a non-empty bucket", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Bucket actions") }));
    await user.click(await screen.findByRole("menuitem", { name: tr("Delete bucket {name}...", { name: "demo-photos" }) }));
    const dialog = await screen.findByRole("dialog", { name: tr("Delete bucket") });
    const button = within(dialog).getByRole("button", { name: tr("Delete bucket") });
    expect(button).toBeDisabled();
    await user.type(within(dialog).getByRole("textbox"), "demo-photos");
    await user.click(button);
    expect(await within(dialog).findByText(new RegExp("not empty.*" + tr("Delete all objects in the bucket first, then try again.")))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Cancel") }));

    await user.click(screen.getByText("demo-empty"));
    await screen.findByText(tr("This folder is empty"));
    await user.click(screen.getByRole("button", { name: tr("Bucket actions") }));
    await user.click(await screen.findByRole("menuitem", { name: tr("Delete bucket {name}...", { name: "demo-empty" }) }));
    const second = await screen.findByRole("dialog", { name: tr("Delete bucket") });
    await user.type(within(second).getByRole("textbox"), "demo-empty");
    await user.click(within(second).getByRole("button", { name: tr("Delete bucket") }));
    await waitFor(() => expect(screen.queryByText("demo-empty")).not.toBeInTheDocument());
    expect(screen.getByText(tr("No bucket open"))).toBeInTheDocument();
  });
});
