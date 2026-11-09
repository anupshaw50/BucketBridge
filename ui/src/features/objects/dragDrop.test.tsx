import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import { findObjectRow, openBucket, renderApp, statusBar, TestPlatform } from "../../test/renderApp";

const original: unknown = Reflect.get(document, "elementFromPoint");

afterEach(() => {
  Reflect.set(document, "elementFromPoint", original);
});

function pointAt(element: Element | null): void {
  Reflect.set(document, "elementFromPoint", () => element);
}

interface Modifiers {
  shiftKey?: boolean;
}

function drag(row: HTMLElement, to: { x: number; y: number }, modifiers: Modifiers = {}): void {
  fireEvent.pointerDown(row, { button: 0, pointerId: 1, clientX: 10, clientY: 10 });
  fireEvent.pointerMove(window, { pointerId: 1, clientX: 40, clientY: 40, ...modifiers });
  fireEvent.pointerMove(window, { pointerId: 1, clientX: to.x, clientY: to.y, ...modifiers });
}

function release(at: { x: number; y: number }, modifiers: Modifiers = {}): void {
  fireEvent.pointerUp(window, { pointerId: 1, clientX: at.x, clientY: at.y, ...modifiers });
}

const queued = (what: string) => tr("{what}: 1 task added to the transfer queue.", { what: tr(what) });

describe("dragging objects", () => {
  it("copies a row into a folder row and shows the feedback while over it", async () => {
    const { user, advance, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    const row = await findObjectRow("readme.md");
    const folder = await findObjectRow("archive");
    pointAt(folder);
    drag(row, { x: 100, y: 100 });
    expect(folder).toHaveAttribute("data-drop-over");
    expect(document.querySelector(".bb-drag-feedback")?.textContent).toContain("archive/");
    release({ x: 100, y: 100 });
    expect(await statusBar().findByText(queued("Copy"))).toBeInTheDocument();
    expect(document.querySelector(".bb-drag-feedback")).toBeNull();
    await advance(500);
    await advance(1000);
    expect(mock.objectKeys("demo-photos")).toEqual(expect.arrayContaining(["readme.md", "archive/readme.md"]));
  });

  it("moves with the move key held", async () => {
    const { user, advance, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    const row = await findObjectRow("readme.md");
    const folder = await findObjectRow("archive");
    pointAt(folder);
    drag(row, { x: 100, y: 100 }, { shiftKey: true });
    release({ x: 100, y: 100 }, { shiftKey: true });
    expect(await statusBar().findByText(queued("Move"))).toBeInTheDocument();
    await advance(1500);
    expect(mock.objectKeys("demo-photos")).toContain("archive/readme.md");
    expect(mock.objectKeys("demo-photos")).not.toContain("readme.md");
  });

  it("does nothing when dropped on the folder the items are already in", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    const row = await findObjectRow("readme.md");
    pointAt(await screen.findByRole("grid", { name: tr("Objects") }));
    drag(row, { x: 100, y: 100 });
    expect(document.querySelector(".bb-drag-feedback")).toBeNull();
    release({ x: 100, y: 100 });
    expect(statusBar().queryByText(/task added|已添加/)).toBeNull();
    expect(mock.objectKeys("demo-photos")).toContain("readme.md");
  });

  it("is not a drag below the threshold", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    const row = await findObjectRow("readme.md");
    fireEvent.pointerDown(row, { button: 0, pointerId: 1, clientX: 10, clientY: 10 });
    fireEvent.pointerMove(window, { pointerId: 1, clientX: 12, clientY: 11 });
    expect(document.querySelector(".bb-drag-feedback")).toBeNull();
    release({ x: 12, y: 11 });
  });

  it("cancels with Escape", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    const row = await findObjectRow("readme.md");
    const folder = await findObjectRow("archive");
    pointAt(folder);
    drag(row, { x: 100, y: 100 });
    fireEvent.keyDown(window, { key: "Escape" });
    expect(folder).not.toHaveAttribute("data-drop-over");
    release({ x: 100, y: 100 });
    expect(statusBar().queryByText(queued("Copy"))).toBeNull();
  });

  it("relays a drag that leaves the window and a drop outside it", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    const row = await findObjectRow("readme.md");
    drag(row, { x: -20, y: 100 });
    await waitFor(() => expect(mock.relayedDrags.map((call) => call.phase)).toContain("over"));
    expect(mock.relayedDrags[0]?.items).toMatchObject({ bucket: "demo-photos", keys: ["readme.md"] });
    release({ x: -20, y: 100 });
    await waitFor(() => expect(mock.relayedDrags.map((call) => call.phase)).toContain("drop"));
  });
});

describe("drags from other windows and the file manager", () => {
  it("carries out a drop relayed from another window", async () => {
    const { user, advance, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    const folder = await findObjectRow("archive");
    pointAt(folder);
    const { accounts } = await mock.list_accounts();
    const items = { account_id: accounts[0]?.id ?? "", bucket: "demo-photos", keys: ["readme.md"] };
    act(() => mock.deliverDrag({ phase: "over", x: 50, y: 50, items, move: false }));
    await waitFor(() => expect(folder).toHaveAttribute("data-drop-over"));
    act(() => mock.deliverDrag({ phase: "leave", x: 0, y: 0, items: null, move: false }));
    await waitFor(() => expect(folder).not.toHaveAttribute("data-drop-over"));
    act(() => mock.deliverDrag({ phase: "drop", x: 50, y: 50, items, move: false }));
    expect(await statusBar().findByText(queued("Copy"))).toBeInTheDocument();
    await advance(1500);
    expect(mock.objectKeys("demo-photos")).toContain("archive/readme.md");
  });

  it("uploads files dropped from the file manager into the folder under the pointer", async () => {
    const platform = new TestPlatform();
    const { user } = await renderApp({ platform });
    await openBucket(user, "demo-photos");
    const folder = await findObjectRow("archive");
    pointAt(folder);
    act(() => platform.dropFiles({ phase: "over", x: 5, y: 5, paths: [] }));
    expect(folder).toHaveAttribute("data-drop-over");
    act(() => platform.dropFiles({ phase: "drop", x: 5, y: 5, paths: ["/home/demo/Documents/report.pdf"] }));
    expect(await statusBar().findByText(queued("Upload"))).toBeInTheDocument();
    expect(folder).not.toHaveAttribute("data-drop-over");
  });

  it("says what to do when files are dropped outside any folder list", async () => {
    const platform = new TestPlatform();
    await renderApp({ platform });
    pointAt(document.body);
    act(() => platform.dropFiles({ phase: "drop", x: 5, y: 5, paths: ["/home/demo/a.txt"] }));
    expect(
      await statusBar().findByText(tr("Open a bucket and drop the files on its list to upload them.")),
    ).toBeInTheDocument();
  });
});
