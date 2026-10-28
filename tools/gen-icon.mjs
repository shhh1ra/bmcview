// Рисует исходную иконку приложения assets/icon.png (1024×1024, RGBA) без внешних зависимостей:
// скруглённый квадрат, контур монитора с подставкой и «курсор» внутри экрана.
// Набор иконок для сборки получается командой `npm run icon` (tauri icon).
import { deflateSync } from "node:zlib";
import { mkdirSync, writeFileSync } from "node:fs";

const SIZE = 1024;
const px = new Uint8Array(SIZE * SIZE * 4);

// Знаковые расстояния (отрицательные внутри фигуры).
const roundRect = (x, y, cx, cy, hw, hh, r) => {
  const qx = Math.abs(x - cx) - hw + r;
  const qy = Math.abs(y - cy) - hh + r;
  return Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) - r;
};
const coverage = (d) => Math.min(1, Math.max(0, 0.5 - d)); // сглаживание в 1 px

const layers = [
  // фон
  { colour: [31, 41, 55], shape: (x, y) => roundRect(x, y, 512, 512, 464, 464, 200) },
  // корпус монитора (кольцо)
  {
    colour: [233, 238, 245],
    shape: (x, y) => {
      const outer = roundRect(x, y, 512, 452, 330, 230, 48);
      const inner = roundRect(x, y, 512, 452, 282, 182, 18);
      return Math.max(outer, -inner);
    },
  },
  // подставка
  { colour: [233, 238, 245], shape: (x, y) => roundRect(x, y, 512, 716, 40, 40, 6) },
  { colour: [233, 238, 245], shape: (x, y) => roundRect(x, y, 512, 772, 170, 24, 24) },
  // курсор-приглашение на «экране»
  { colour: [88, 166, 255], shape: (x, y) => roundRect(x, y, 418, 452, 70, 22, 22) },
  { colour: [88, 166, 255], shape: (x, y) => roundRect(x, y, 548, 452, 22, 22, 22) },
];

for (let y = 0; y < SIZE; y++) {
  for (let x = 0; x < SIZE; x++) {
    let [r, g, b, a] = [0, 0, 0, 0];
    for (const { colour, shape } of layers) {
      const c = coverage(shape(x + 0.5, y + 0.5));
      if (c === 0) continue;
      const na = c + a * (1 - c);
      r = (colour[0] * c + r * a * (1 - c)) / na;
      g = (colour[1] * c + g * a * (1 - c)) / na;
      b = (colour[2] * c + b * a * (1 - c)) / na;
      a = na;
    }
    const i = (y * SIZE + x) * 4;
    px[i] = Math.round(r);
    px[i + 1] = Math.round(g);
    px[i + 2] = Math.round(b);
    px[i + 3] = Math.round(a * 255);
  }
}

// PNG: сигнатура, IHDR, IDAT (фильтр 0 на каждой строке), IEND.
const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc32 = (buf) => {
  let c = 0xffffffff;
  for (const byte of buf) c = crcTable[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};
const chunk = (type, data) => {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([len, body, crc]);
};

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0);
ihdr.writeUInt32BE(SIZE, 4);
ihdr[8] = 8; // бит на канал
ihdr[9] = 6; // RGBA

const raw = Buffer.alloc((SIZE * 4 + 1) * SIZE);
for (let y = 0; y < SIZE; y++) {
  raw[y * (SIZE * 4 + 1)] = 0;
  Buffer.from(px.buffer, y * SIZE * 4, SIZE * 4).copy(raw, y * (SIZE * 4 + 1) + 1);
}

const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
]);

mkdirSync("assets", { recursive: true });
writeFileSync("assets/icon.png", png);
console.log(`assets/icon.png: ${png.length} байт`);
