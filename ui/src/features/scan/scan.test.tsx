import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { scanBucket } from "../../mock/scanMock";
import { tr } from "../../i18n";
import { countBySeverity, openableTab, scanRows } from "../../lib/scan";
import { renderApp } from "../../test/renderApp";

describe("security scan dialog", () => {
  it("scans every bucket, lists findings by severity and opens the fixing tab", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Tools") }));
    await user.click(await screen.findByRole("menuitem", { name: tr("Security scan...") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Security scan") });
    expect(within(dialog).queryByRole("table", { name: tr("Findings") })).not.toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: tr("Scan") }));
    const table = await within(dialog).findByRole("table", { name: tr("Findings") });
    const rows = within(table).getAllByRole("row").slice(1);
    expect(rows.length).toBeGreaterThan(0);
    expect(within(dialog).getByLabelText(tr("Summary"))).toHaveTextContent(tr("Critical"));
    expect(within(dialog).getByRole("button", { name: tr("Export CSV") })).toBeInTheDocument();

    const template = tr("Open {label} tab").replace(/\{(\w+)\}/g, "§§§");
    const fixButton = new RegExp(template.replace(/[.*+?^${}()|[\]\\]/g, "\\$&").replace(/§§§/g, ".*"));
    await user.click(within(table).getAllByRole("button", { name: fixButton })[0]!);
    await waitFor(() => expect(screen.getAllByRole("dialog").length).toBeGreaterThan(1));
  });
});

describe("scan helpers", () => {
  it("sorts findings by severity and counts them", () => {
    const report = {
      generated_at: "2026-01-01T00:00:00Z",
      buckets: [scanBucket("plain-a"), scanBucket("public-b"), scanBucket("tidy")],
    };
    const rows = scanRows(report);
    expect(rows[0]?.finding.severity).toBe("critical");
    expect(rows.at(-1)?.finding.severity).toBe("low");
    expect(countBySeverity(report, "critical")).toBe(2);
    expect(openableTab("policy")).toBe("policy");
    expect(openableTab("encryption")).toBeNull();
  });
});
