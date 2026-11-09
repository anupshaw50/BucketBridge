import "./locale";
import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach, beforeEach } from "vitest";
import { setLocale } from "../i18n";
import { disposeRenderedApps } from "./renderApp";

// The tests run in English, the default language (see ./locale, which is
// imported before the application modules); tr() reads the locale, and this
// keeps a test that switches language from leaking into the next one.
beforeEach(() => {
  setLocale("en");
});

afterEach(() => {
  cleanup();
  disposeRenderedApps();
});

// jsdom lacks the layout and media APIs Mantine and the virtualised lists use.
Object.defineProperty(window, "matchMedia", {
  writable: true,
  configurable: true,
  value: (query: string): MediaQueryList => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => undefined,
    removeListener: () => undefined,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
    dispatchEvent: () => false,
  }),
});

class ResizeObserverStub implements ResizeObserver {
  observe(): void {
    // jsdom has no layout, so nothing ever resizes.
  }
  unobserve(): void {
    // See observe().
  }
  disconnect(): void {
    // See observe().
  }
}
window.ResizeObserver = ResizeObserverStub;

Element.prototype.scrollIntoView = function scrollIntoView() {
  // No layout in jsdom.
};
Element.prototype.scrollTo = function scrollTo() {
  // No layout in jsdom.
};

// Give scroll containers a size so @tanstack/react-virtual renders rows.
Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get: () => 800 });
Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, get: () => 1200 });
