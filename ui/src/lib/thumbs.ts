/**
 * Stored thumbnails, as `data:` URLs.
 *
 * One module-level cache because a re-render is not a reason to re-encode the
 * same JPEG through IPC, and because the same picture on several posts is one
 * hash. A `null` is remembered too: it means the file was gone when we asked,
 * and asking again on every mount would be a read per frame.
 *
 * Shared by the media grid and the avatars so a picture used in both is fetched
 * once, not once per component.
 */
import { mediaThumb } from './api';

const cache = new Map<string, string | null>();

export function thumbFor(hash: string): Promise<string | null> {
  const hit = cache.get(hash);
  if (hit !== undefined) return Promise.resolve(hit);
  return mediaThumb(hash)
    .then((value) => {
      cache.set(hash, value);
      return value;
    })
    .catch(() => {
      cache.set(hash, null);
      return null;
    });
}
