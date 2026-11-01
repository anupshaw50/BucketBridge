import type { AppModel } from "../../state/model";

const supported = new WeakMap<AppModel, boolean>();
const asked = new WeakSet<AppModel>();

/**
 * Whether this build of the app can mount buckets as drives (`mount_status().supported`). The answer is asked
 * from the backend when the app starts (`AppModel.start`) or the first time a menu needs it; until it arrives the entry stays hidden, as it does for
 * builds without the `mount` feature and in the browser mock.
 */
export function mountSupported(model: AppModel): boolean {
  if (!asked.has(model)) {
    asked.add(model);
    try {
      model.api
        .mount_status()
        .then((status) => {
          supported.set(model, status.supported);
        })
        .catch(() => {
          supported.set(model, false);
        });
    } catch {
      supported.set(model, false);
    }
  }
  return supported.get(model) ?? false;
}
