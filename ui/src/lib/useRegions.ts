import { useEffect, useState } from "react";
import type { RegionEntry } from "../api/types";
import { useModel } from "../app/context";

/** The region table from the backend, loaded once per component. */
export function useRegions(): RegionEntry[] {
  const model = useModel();
  const [regions, setRegions] = useState<RegionEntry[]>([]);
  useEffect(() => {
    let alive = true;
    model.api
      .list_regions()
      .then((list) => {
        if (alive) {
          setRegions(list);
        }
      })
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [model]);
  return regions;
}
