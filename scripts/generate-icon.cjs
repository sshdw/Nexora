const fs = require("fs");
const path = require("path");
const zlib = require("zlib");

const W = 1024, H = 1024;
const rowLen = 1 + W * 3;
const raw = Buffer.alloc(rowLen * H);

for (let y = 0; y < H; y++) {
  const rowStart = y * rowLen;
  raw[rowStart] = 0;
  for (let x = 0; x < W; x++) {
    const px = rowStart + 1 + x * 3;
    raw[px] = 37; raw[px + 1] = 99; raw[px + 2] = 235;
  }
}

const compressed = zlib.deflateSync(raw);

function chunk(type, data) {
  const typeBuf = Buffer.from(type, "latin1");
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const crcInput = Buffer.concat([typeBuf, data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(zlib.crc32(crcInput) >>> 0, 0);
  return Buffer.concat([len, typeBuf, data, crc]);
}

const sig = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(W, 0);
ihdr.writeUInt32BE(H, 4);
ihdr[8] = 8; ihdr[9] = 2; ihdr[10] = 0; ihdr[11] = 0; ihdr[12] = 0;

const png = Buffer.concat([
  sig, chunk("IHDR", ihdr), chunk("IDAT", compressed), chunk("IEND", Buffer.alloc(0))
]);

const outDir = path.join(__dirname, "..", "src-tauri");
fs.mkdirSync(outDir, { recursive: true });
fs.writeFileSync(path.join(outDir, "app-icon.png"), png);
console.log("Generated src-tauri/app-icon.png");
