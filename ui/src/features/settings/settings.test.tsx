import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { BackendError } from "../../api/errors";
import { App } from "../../app/App";
import { MockBackend } from "../../mock/mockBackend";
import { immediateScheduler } from "../../state/scheduler";
import { settle } from "../../test/helpers";
import { tr } from "../../i18n";
import { renderApp, statusBar, TestPlatform } from "../../test/renderApp";

function scheme(): string | null {
  return document.documentElement.getAttribute("data-mantine-color-scheme");
}

/** An accessible-name matcher that accepts the label as a prefix (labels with a description are longer). */
function labelIn(label: string): (accessibleName: string) => boolean {
  return (accessibleName) => accessibleName.includes(label);
}

describe("settings toggles before the settings are loaded", () => {
  it("keeps the toolbar toggles disabled, so stored settings are never overwritten", async () => {
    const mock = new MockBackend({ autoRun: false, latencyMs: 0, logObjects: 30, manyFiles: 0 });
    const stored = await mock.get_settings();
    stored.proxy.enabled = true;
    stored.proxy.host = "proxy.example.com";
    stored.advanced.list_page_size = 200;
    await mock.save_settings({ settings: stored });
    const original = mock.get_settings.bind(mock);
    let failures = 1;
    mock.get_settings = () => {
      if (failures > 0) {
        failures -= 1;
        return Promise.reject(new BackendError("io", "settings.json: resource busy"));
      }
      return original();
    };
    const user = userEvent.setup();
    const view = render(
      <App
        services={{ backend: mock, platform: new TestPlatform() }}
        env="test"
        notify={() => undefined}
        scheduler={immediateScheduler}
      />,
    );
    await act(async () => {
      await settle();
    });
    await screen.findByText("demo-photos");
    for (const name of [tr("Event log"), tr("Transfer queue"), tr("Flat browsing")]) {
      expect(screen.getByRole("button", { name })).toBeDisabled();
    }
    expect(screen.getByRole("button", { name: tr("Hide transfers") })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: tr("Settings") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Settings") });
    await user.click(within(dialog).getByRole("button", { name: tr("Try again") }));
    await waitFor(() => expect(screen.getByRole("button", { name: tr("Event log") })).toBeEnabled());
    await user.keyboard("{Escape}");
    await user.click(screen.getByRole("button", { name: tr("Event log") }));
    await waitFor(async () => expect((await original()).interface.show_log_panel).toBe(true));
    const after = await original();
    expect(after.proxy.host).toBe("proxy.example.com");
    expect(after.advanced.list_page_size).toBe(200);
    view.unmount();
    mock.dispose();
  });
});

describe("settings dialog", () => {
  it("validates ranges, jumps to the invalid page and saves with OK", async () => {
    const { user, mock } = await renderApp();
    await user.click(await screen.findByRole("button", { name: tr("Settings") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Settings") });
    await user.click(within(dialog).getByRole("button", { name: tr("Advanced") }));
    const pageSize = within(dialog).getByRole("textbox", { name: tr("Objects per listing page") });
    await user.clear(pageSize);
    await user.type(pageSize, "5000");
    await user.click(within(dialog).getByRole("button", { name: tr("General") }));
    await user.click(within(dialog).getByRole("button", { name: tr("OK") }));
    expect(
      await within(dialog).findByText(tr("Enter a whole number from {min} to {max}.", { min: "1", max: "1,000" })),
    ).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("Advanced") })).toHaveAttribute("aria-current", "page");
    expect((await mock.get_settings()).advanced.list_page_size).toBe(1000);

    await user.clear(within(dialog).getByRole("textbox", { name: tr("Objects per listing page") }));
    await user.type(within(dialog).getByRole("textbox", { name: tr("Objects per listing page") }), "250");
    await user.click(within(dialog).getByRole("button", { name: tr("Transfers") }));
    await user.click(within(dialog).getByRole("switch", { name: labelIn(tr("Limit bandwidth")) }));
    const limit = within(dialog).getByRole("textbox", { name: tr("Bandwidth limit") });
    await user.clear(limit);
    await user.type(limit, "2048");
    await user.click(within(dialog).getByRole("button", { name: tr("OK") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Settings") })).not.toBeInTheDocument());
    const saved = await mock.get_settings();
    expect(saved.advanced.list_page_size).toBe(250);
    expect(saved.transfer.bandwidth_limit_kibps).toBe(2048);
    expect(statusBar().getByText(tr("Settings saved."))).toBeInTheDocument();
  });

  it("previews the theme immediately, reverts on Cancel and keeps it after Apply", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    expect(scheme()).toBe("light");
    await user.click(screen.getByRole("button", { name: tr("Settings") }));
    let dialog = await screen.findByRole("dialog", { name: tr("Settings") });
    await user.click(within(dialog).getByRole("button", { name: tr("Interface") }));
    await user.click(within(dialog).getByRole("radio", { name: tr("Dark") }));
    await waitFor(() => expect(scheme()).toBe("dark"));
    await user.click(within(dialog).getByRole("button", { name: tr("Cancel") }));
    await waitFor(() => expect(scheme()).toBe("light"));
    expect((await mock.get_settings()).interface.theme).toBe("system");

    await user.click(screen.getByRole("button", { name: tr("Settings") }));
    dialog = await screen.findByRole("dialog", { name: tr("Settings") });
    await user.click(within(dialog).getByRole("button", { name: tr("Interface") }));
    await user.click(within(dialog).getByRole("radio", { name: tr("Dark") }));
    await user.click(within(dialog).getByRole("switch", { name: tr("Show the event log") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Apply") }));
    await waitFor(async () => expect((await mock.get_settings()).interface.theme).toBe("dark"));
    expect(screen.getByRole("dialog", { name: tr("Settings") })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: tr("Event log") })).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Cancel") }));
    expect(scheme()).toBe("dark");
  });

  it("stores the proxy password without ever showing it, and validates the proxy", async () => {
    const { user, mock } = await renderApp();
    await user.click(await screen.findByRole("button", { name: tr("Settings") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Settings") });
    await user.click(within(dialog).getByRole("button", { name: tr("Proxy") }));
    await user.click(within(dialog).getByRole("switch", { name: tr("Connect through a proxy") }));
    await user.type(within(dialog).getByRole("textbox", { name: tr("Bypass the proxy for") }), "*.example.com\nnot valid");
    await user.click(within(dialog).getByRole("button", { name: tr("OK") }));
    expect(await within(dialog).findByText(tr("Enter the proxy host."))).toBeInTheDocument();
    expect(
      within(dialog).getByText(
        tr("\"{entry}\" is not a host name, \"*.suffix\" pattern, IP address or CIDR range.", { entry: "not valid" }),
      ),
    ).toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: tr("Set password...") }));
    await user.type(within(dialog).getByLabelText(tr("New proxy password")), "proxy-secret-1");
    await user.click(within(dialog).getByRole("button", { name: tr("Save password") }));
    expect(
      await within(dialog).findByText(new RegExp(tr("A password is stored in the credential store."))),
    ).toBeInTheDocument();
    expect(mock.storedProxyPassword()).toBe("proxy-secret-1");
    expect(within(dialog).queryByDisplayValue("proxy-secret-1")).not.toBeInTheDocument();

    await user.type(within(dialog).getByRole("textbox", { name: tr("Host") }), "proxy.example.com");
    const bypass = within(dialog).getByRole("textbox", { name: tr("Bypass the proxy for") });
    await user.clear(bypass);
    await user.type(bypass, "*.example.com\n10.0.0.0/8");
    await user.click(within(dialog).getByRole("button", { name: tr("OK") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Settings") })).not.toBeInTheDocument());
    const saved = await mock.get_settings();
    expect(saved.proxy).toMatchObject({
      enabled: true,
      host: "proxy.example.com",
      bypass: ["*.example.com", "10.0.0.0/8"],
      password_ref: "bucketbridge/proxy/password",
    });
  });

  it("chooses a default download folder", async () => {
    const { user, mock, platform } = await renderApp();
    platform.folderAnswers.push("/home/demo/Downloads");
    await user.click(await screen.findByRole("button", { name: tr("Settings") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Settings") });
    await user.click(within(dialog).getByRole("radio", { name: tr("Always download to this folder") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Apply") }));
    expect(await within(dialog).findByText(tr("Choose a folder or select ask every time."))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Browse...") }));
    expect(within(dialog).getByRole("textbox", { name: tr("Default download folder") })).toHaveValue("/home/demo/Downloads");
    await user.click(within(dialog).getByRole("button", { name: tr("OK") }));
    await waitFor(async () => expect((await mock.get_settings()).general.default_download_dir).toBe("/home/demo/Downloads"));
  });
});
