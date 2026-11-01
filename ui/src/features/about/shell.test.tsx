import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { openBucket, renderApp } from "../../test/renderApp";

describe("crash reporter", () => {
  it("shows the report, copies it, reveals the file and dismisses it", async () => {
    const { user, mock, platform } = await renderApp({ mock: { crashReport: true } });
    const dialog = await screen.findByRole("dialog", { name: "BucketBridge closed unexpectedly" });
    const report = await mock.last_crash_report();
    if (report === null) throw new Error("fixture missing");
    expect(within(dialog).getByLabelText("Crash report contents")).toHaveTextContent("panicked at");
    expect(within(dialog).getByRole("textbox", { name: "Report file" })).toHaveValue(report.path);
    expect(within(dialog).getByText(/has not been sent anywhere/)).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Copy report" }));
    expect(platform.copied).toContain(report.contents);
    await user.click(within(dialog).getByRole("button", { name: "Show in folder" }));
    expect(platform.revealed).toEqual([report.path]);
    await user.click(within(dialog).getByRole("button", { name: "Dismiss report" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "BucketBridge closed unexpectedly" })).not.toBeInTheDocument(),
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
    await user.click(await screen.findByRole("button", { name: "About" }));
    const dialog = await screen.findByRole("dialog", { name: "About BucketBridge" });
    expect(await within(dialog).findByText(/Version 0\.1\.0/)).toBeInTheDocument();
    expect(within(dialog).getByText(/no activation, registration or license key is required/)).toBeInTheDocument();
    expect(within(dialog).getByText(/Permission is hereby granted/)).toBeInTheDocument();
    for (const name of ["Source code", "Online help", "Contact and issues", "Check for updates"]) {
      await user.click(within(dialog).getByRole("button", { name }));
    }
    expect(platform.opened).toEqual([
      "https://github.com/anupshaw50/BucketBridge",
      "https://github.com/anupshaw50/BucketBridge/tree/main/docs",
      "https://github.com/anupshaw50/BucketBridge/issues",
      "https://github.com/anupshaw50/BucketBridge/releases",
    ]);
  });
});

describe("bucket dialogs", () => {
  it("creates a bucket after checking the S3 naming rules", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: "New bucket" }));
    const dialog = await screen.findByRole("dialog", { name: "New bucket" });
    const name = within(dialog).getByRole("textbox", { name: /Bucket name/ });
    await user.type(name, "Bad_Name");
    expect(within(dialog).getByText("Use only lowercase letters, digits, dots (.) and hyphens (-).")).toBeInTheDocument();
    await user.clear(name);
    await user.type(name, "demo-photos");
    await user.click(within(dialog).getByRole("button", { name: "Create bucket" }));
    expect(await within(dialog).findByText(/already own a bucket/)).toBeInTheDocument();
    await user.clear(name);
    await user.type(name, "fresh-bucket");
    await user.type(within(dialog).getByRole("textbox", { name: /Region/ }), "eu-west-1");
    await user.click(within(dialog).getByRole("button", { name: "Create bucket" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "New bucket" })).not.toBeInTheDocument());
    expect(await screen.findByText("This folder is empty")).toBeInTheDocument();
    const buckets = await mock.list_buckets({ account_id: (await mock.list_accounts()).default_account_id ?? "" });
    expect(buckets.find((bucket) => bucket.name === "fresh-bucket")?.region).toBe("eu-west-1");
  });

  it("requires the typed name and reports a non-empty bucket", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(screen.getByRole("button", { name: "Bucket actions" }));
    await user.click(await screen.findByRole("menuitem", { name: "Delete bucket demo-photos..." }));
    const dialog = await screen.findByRole("dialog", { name: "Delete bucket" });
    const button = within(dialog).getByRole("button", { name: "Delete bucket" });
    expect(button).toBeDisabled();
    await user.type(within(dialog).getByRole("textbox"), "demo-photos");
    await user.click(button);
    expect(await within(dialog).findByText(/not empty.*Delete all objects in the bucket first/)).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));

    await user.click(screen.getByText("demo-empty"));
    await screen.findByText("This folder is empty");
    await user.click(screen.getByRole("button", { name: "Bucket actions" }));
    await user.click(await screen.findByRole("menuitem", { name: "Delete bucket demo-empty..." }));
    const second = await screen.findByRole("dialog", { name: "Delete bucket" });
    await user.type(within(second).getByRole("textbox"), "demo-empty");
    await user.click(within(second).getByRole("button", { name: "Delete bucket" }));
    await waitFor(() => expect(screen.queryByText("demo-empty")).not.toBeInTheDocument());
    expect(screen.getByText("No bucket open")).toBeInTheDocument();
  });
});
