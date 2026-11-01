import { Modal, type ModalProps } from "@mantine/core";
import { use, useEffect, useId, useState, useSyncExternalStore } from "react";
import { MODAL_BASE_Z_INDEX, ModalLayers, ModalLayersContext } from "./modalLayers";

const EMPTY: readonly string[] = [];
const noopSubscribe = () => () => undefined;
const emptySnapshot = () => EMPTY;

export function ModalLayersProvider({ children }: { children: React.ReactNode }) {
  const [layers] = useState(() => new ModalLayers());
  return <ModalLayersContext value={layers}>{children}</ModalLayersContext>;
}

/**
 * Mantine modal that stacks correctly when several are open: the top-most one
 * owns Escape and outside clicks, the others stay visible underneath.
 */
export function AppModal({ opened, closeOnEscape = true, closeOnClickOutside = true, ...props }: ModalProps) {
  const id = useId();
  const layers = use(ModalLayersContext);
  useEffect(() => {
    if (!opened || layers === null) {
      return undefined;
    }
    layers.push(id);
    return () => layers.remove(id);
  }, [opened, layers, id]);
  const stack = useSyncExternalStore(
    layers?.subscribe ?? noopSubscribe,
    layers?.getSnapshot ?? emptySnapshot,
    layers?.getSnapshot ?? emptySnapshot,
  );
  const index = stack.indexOf(id);
  const isTop = index === -1 || index === stack.length - 1;
  return (
    <Modal
      opened={opened}
      zIndex={MODAL_BASE_Z_INDEX + Math.max(index, 0) * 2}
      closeOnEscape={isTop && closeOnEscape}
      closeOnClickOutside={isTop && closeOnClickOutside}
      {...props}
    />
  );
}
