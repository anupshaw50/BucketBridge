import { fireEvent, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import { estimateRestore, MOCK_IT_ARCHIVE, MOCK_IT_DEEP_ARCHIVE } from "../../mock/restoreEstimate";
import { findObjectRow, openBucket, renderApp } from "../../test/renderApp";
import { formatUsd, onlyIntelligentTiering, tierEstimateText } from "./restoreText";

const GiB = 1024 ** 3;

describe("restore estimate text", () => {
  it("formats dollars", () => {
    expect(formatUsd(0)).toBe("$0.00");
    expect(formatUsd(1.234)).toBe("$1.23");
    expect(formatUsd(0.0025)).toBe("$0.0025");
    expect(formatUsd(0.000025)).toBe("<$0.0001");
  });

  it("prices Glacier and says what a tier cannot do", () => {
    const estimate = estimateRestore([{ class: "GLACIER", size: 100 * GiB }, { class: "DEEP_ARCHIVE", size: GiB }, null]);
    expect(estimate.objects).toBe(2);
    expect(estimate.summary.unreadable).toBe(1);
    const expedited = estimate.tiers.find((entry) => entry.tier === "expedited");
    const standard = estimate.tiers.find((entry) => entry.tier === "standard");
    expect(expedited?.unsupported).toBe(1);
    expect(tierEstimateText(expedited!)).toContain(tr("{text} ({n} objects cannot use this tier)", { text: "", n: 1 }).trim().slice(1));
    expect(tierEstimateText(standard!)).toBe(tr("{tier}: about {cost}", { tier: tr("Standard"), cost: formatUsd(standard!.total_usd) }));
    expect(estimate.note).toContain("check your AWS bill");
  });

  it("recognises a selection that holds only Intelligent-Tiering archives", () => {
    expect(onlyIntelligentTiering(estimateRestore([{ class: MOCK_IT_ARCHIVE, size: 1 }, { class: MOCK_IT_DEEP_ARCHIVE, size: 1 }]))).toBe(true);
    expect(onlyIntelligentTiering(estimateRestore([{ class: MOCK_IT_ARCHIVE, size: 1 }, { class: "GLACIER", size: 1 }]))).toBe(false);
    expect(onlyIntelligentTiering(estimateRestore([{ class: "STANDARD", size: 1 }]))).toBe(false);
  });
});

describe("restore dialog", () => {
  it("shows the size, the count and the estimate per tier, then requests the restore", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-backups");
    await user.dblClick(await findObjectRow("archive"));
    fireEvent.contextMenu(await findObjectRow("2019.tar"));
    await user.click(await screen.findByRole("menuitem", { name: tr("Restore from Glacier…") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Restore from Glacier") });
    expect(await within(dialog).findByTestId("restore-totals")).toHaveTextContent(
      tr("{n} archived objects, {size} in total.", { n: "1", size: "4.0 GiB" }),
    );
    // 4 GiB in Glacier Flexible Retrieval: Expedited 4 * 0.03 + 0.01, Standard 4 * 0.01 + 0.00005, Bulk 0.000025.
    expect(within(dialog).getByTestId("restore-estimate-expedited")).toHaveTextContent("$0.13");
    expect(within(dialog).getByTestId("restore-estimate-standard")).toHaveTextContent("$0.04");
    expect(within(dialog).getByTestId("restore-estimate-bulk")).toHaveTextContent("<$0.0001");
    expect(within(dialog).getByTestId("restore-estimate-note")).toHaveTextContent("2025-06-01");
    await user.click(within(dialog).getByRole("button", { name: tr("Request restore") }));
    await within(document.body).findByText(tr("Restore requested for 1 object."));
    const state = await mock.get_restore_state({
      account_id: (await mock.list_accounts()).default_account_id ?? "",
      bucket: "demo-backups",
      key: "archive/2019.tar",
      version_id: null,
    });
    expect(state.restore?.in_progress).toBe(true);
  });
});
