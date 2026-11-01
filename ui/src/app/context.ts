import { createContext, use } from "react";
import { useStore } from "zustand";
import type { StoreApi } from "zustand/vanilla";
import type { AppModel } from "../state/model";

export const ModelContext = createContext<AppModel | null>(null);

export function useModel(): AppModel {
  const model = use(ModelContext);
  if (model === null) {
    throw new Error("useModel must be used inside <ModelContext>");
  }
  return model;
}

/** Subscribes to a slice of one of the model's stores. The selector must return a stable value. */
export function useSlice<S, T>(store: StoreApi<S>, selector: (state: S) => T): T {
  return useStore(store, selector);
}
