const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

/** Wrap a small PNG frame in the ICO container without re-encoding the pixels. */
export function wrapPngAsIco(png) {
  if (!Buffer.isBuffer(png) || png.length < 33 || !png.subarray(0, 8).equals(PNG_SIGNATURE)) {
    throw new Error("icon source must be a complete PNG image");
  }
  if (png.toString("ascii", 12, 16) !== "IHDR") throw new Error("PNG has no IHDR header");
  const width = png.readUInt32BE(16);
  const height = png.readUInt32BE(20);
  if (width < 1 || height < 1 || width > 256 || height > 256) {
    throw new Error(`PNG dimensions must be between 1 and 256 pixels (received ${width}x${height})`);
  }

  const directory = Buffer.alloc(22);
  directory.writeUInt16LE(0, 0); // Reserved.
  directory.writeUInt16LE(1, 2); // Image type: icon.
  directory.writeUInt16LE(1, 4); // One PNG frame.
  directory.writeUInt8(width === 256 ? 0 : width, 6);
  directory.writeUInt8(height === 256 ? 0 : height, 7);
  directory.writeUInt8(0, 8); // Palette size.
  directory.writeUInt8(0, 9); // Reserved.
  directory.writeUInt16LE(1, 10); // Color planes.
  directory.writeUInt16LE(32, 12); // Bits per pixel.
  directory.writeUInt32LE(png.length, 14);
  directory.writeUInt32LE(directory.length, 18);
  return Buffer.concat([directory, png]);
}
