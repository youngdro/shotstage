// The browser half of "Export video": WebCodecs turns the editor's RGBA frames into H.264 (or
// VP9 where H.264 cannot be encoded); the Rust side (src/mp4.rs) writes the MP4 around them.

const AVC = ["avc1.640034", "avc1.640033", "avc1.640028", "avc1.4d0028", "avc1.42e028", "avc1.42e01f"];
const VP9 = ["vp09.00.50.08", "vp09.00.41.08", "vp09.00.40.08", "vp09.00.31.08", "vp09.00.10.08"];

async function pick(width, height, fps, bitrate) {
  for (const [kind, codecs] of [["avc", AVC], ["vp9", VP9]]) {
    for (const codec of codecs) {
      const config = { codec, width, height, bitrate, framerate: fps, latencyMode: "quality" };
      if (kind === "avc") config.avc = { format: "avc" };
      try {
        const s = await VideoEncoder.isConfigSupported(config);
        if (s.supported) return [kind, s.config];
      } catch (_) {
        // not this one
      }
    }
  }
  return null;
}

/** An encoder for `width`×`height` RGBA frames, or an Error saying why there is none. */
export async function openVideo(width, height, fps, bitrate) {
  if (typeof VideoEncoder === "undefined" || typeof VideoFrame === "undefined") {
    throw new Error("this browser cannot encode video (no WebCodecs): use a current Chrome, Edge or Safari");
  }
  const picked = await pick(width, height, fps, bitrate);
  if (!picked) throw new Error(`this browser cannot encode ${width}×${height} video: try a smaller size`);
  const [kind, config] = picked;
  const v = { kind, fps, width, height, chunks: [], description: null, error: null };
  v.encoder = new VideoEncoder({
    output(chunk, meta) {
      const data = new Uint8Array(chunk.byteLength);
      chunk.copyTo(data);
      v.chunks.push({ data, key: chunk.type === "key" });
      const d = meta && meta.decoderConfig && meta.decoderConfig.description;
      if (d && !v.description) {
        v.description = ArrayBuffer.isView(d)
          ? new Uint8Array(d.buffer.slice(d.byteOffset, d.byteOffset + d.byteLength))
          : new Uint8Array(d.slice(0));
      }
    },
    error(e) {
      v.error = e;
    },
  });
  v.encoder.configure(config);
  return v;
}

/** Encode frame `index` (tightly packed RGBA bytes). Waits while the encoder is busy. */
export async function videoAdd(v, rgba, index) {
  if (v.error) throw v.error;
  while (v.encoder.encodeQueueSize > 3) {
    await new Promise((r) => setTimeout(r, 4));
    if (v.error) throw v.error;
  }
  const us = 1e6 / v.fps;
  const frame = new VideoFrame(rgba, {
    format: "RGBA",
    codedWidth: v.width,
    codedHeight: v.height,
    timestamp: Math.round(index * us),
    duration: Math.round(us),
  });
  // a key frame every two seconds, so players can seek
  v.encoder.encode(frame, { keyFrame: index % Math.max(1, Math.round(v.fps * 2)) === 0 });
  frame.close();
}

/** Every encoded frame: `{kind: "avc" | "vp9", description, chunks: [{data, key}]}`. */
export async function videoFinish(v) {
  await v.encoder.flush();
  v.encoder.close();
  if (v.error) throw v.error;
  return { kind: v.kind, description: v.description, chunks: v.chunks };
}

/** Stop without a file. */
export function videoCancel(v) {
  try {
    v.encoder.close();
  } catch (_) {
    // already closed
  }
}
