// Scene links: the whole scene document travels in the URL fragment (`#scene=...`), compressed
// with deflate and base64url-encoded. The fragment never reaches the server, so a static host
// (GitHub Pages) is enough and nothing is stored anywhere.
//
//   import { sceneToLink, sceneFromLocation } from "./share.js";
//   const url = await sceneToLink(editor.scene());       // https://.../#scene=...
//   const scene = await sceneFromLocation();             // null when the URL carries none

const KEY = "scene";

function toBase64Url(bytes) {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  }
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function fromBase64Url(text) {
  const b64 = text.replace(/-/g, "+").replace(/_/g, "/");
  const bin = atob(b64 + "===".slice((b64.length + 3) % 4));
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

async function pipe(bytes, stream) {
  const res = new Response(new Blob([bytes]).stream().pipeThrough(stream));
  return new Uint8Array(await res.arrayBuffer());
}

/** Compress a scene document into the text that goes after `#scene=`. */
export async function encodeScene(scene) {
  const json = new TextEncoder().encode(JSON.stringify(scene));
  return toBase64Url(await pipe(json, new CompressionStream("deflate-raw")));
}

/** Inverse of `encodeScene`. Throws when the text is damaged (e.g. a truncated link). */
export async function decodeScene(text) {
  const json = await pipe(fromBase64Url(text), new DecompressionStream("deflate-raw"));
  return JSON.parse(new TextDecoder().decode(json));
}

/** A link to this page that opens `scene`. */
export async function sceneToLink(scene, base = location.href) {
  const url = new URL(base);
  url.hash = `${KEY}=${await encodeScene(scene)}`;
  return url.toString();
}

/** The scene in the current URL's fragment, `null` when there is none. */
export async function sceneFromLocation(loc = location) {
  const params = new URLSearchParams(loc.hash.replace(/^#/, ""));
  const text = params.get(KEY);
  return text ? decodeScene(text) : null;
}

/** Drop the scene from the address bar so a reload opens the autosaved, edited copy. */
export function clearSceneFromLocation() {
  history.replaceState(null, "", location.pathname + location.search);
}
