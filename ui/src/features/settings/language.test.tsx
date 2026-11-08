import { screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { getLocale, setLocale, tr } from "../../i18n";
import { renderApp } from "../../test/renderApp";

describe("language switch", () => {
  it("offers both languages in the settings and switches the rendered text", async () => {
    const { user } = await renderApp();
    await user.click(await screen.findByRole("button", { name: tr("Settings") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Settings") });
    await user.click(within(dialog).getByRole("button", { name: tr("Interface") }));
    expect(within(dialog).getByRole("radio", { name: "简体中文" })).toBeChecked();
    expect(within(dialog).getByRole("radio", { name: "English" })).not.toBeChecked();

    // The control itself reloads the page in the application (module-level
    // label tables are built once); here the switch is made directly.
    setLocale("en");
    expect(getLocale()).toBe("en");
    expect(await screen.findByRole("button", { name: "Settings" })).toBeInTheDocument();

    setLocale("zh-CN");
    expect(getLocale()).toBe("zh-CN");
    expect(await screen.findByRole("button", { name: tr("Settings") })).toBeInTheDocument();
  });
});
