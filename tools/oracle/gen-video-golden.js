// Генератор эталонов для декодера видео.
//
// Строит случайные (детерминированные по seed) кадры из блоков всех типов, прогоняет их
// через ОРИГИНАЛЬНЫЙ com.library.decoder.ImageDecoder.decodeRLEorJPEG1 и записывает
// CRC32 пикселей каждого блока. Rust-тест core/tests/golden_video.rs декодирует те же кадры
// и сравнивает.
//
// Запуск (Nashorn из JRE оригинального клиента, классы — из распакованной копии jar):
//   jjs -cp <extract/pe> tools/oracle/gen-video-golden.js -- <выходной файл> [seed]
//
// Формат вывода (текст):
//   size W H          — новая последовательность, новый декодер
//   frame Q S HEX     — кадр: индекс качества, статус (0 ок / 1 исключение в оригинале), данные
//   block I CRC       — после успешного кадра: CRC32 пикселей блока I (RGB, big-endian u32)
//   reset             — после ошибки: оба декодера пересоздаются

var ImageDecoder = Java.type("com.library.decoder.ImageDecoder");
var JPEGData = Java.type("com.library.decoder.JPEGData");
var CRC32 = Java.type("java.util.zip.CRC32");
var PrintWriter = Java.type("java.io.PrintWriter");
var IntArray = Java.type("int[]");
var ByteArray = Java.type("byte[]");

var out = new PrintWriter(arguments[0], "UTF-8");
var Random = Java.type("java.util.Random");
var rng = new Random(arguments.length > 1 ? parseInt(arguments[1]) : 12345);

// Nashorn — ES5, детерминированность обеспечивает java.util.Random с фиксированным seed.
function rand() { return rng.nextDouble(); }
function randInt(n) { return Math.floor(rand() * n); }
function colour() { return [randInt(256), randInt(256), randInt(256)]; }

// Энтропийные данные JPEG-блока с нулевыми коэффициентами (ровный серый 128),
// тот же блок, что в Rust-тесте jpeg::tests::flat_grey_entropy.
var FLAT_JPEG = (function () {
    var bits = "", mcu = "00101000000000", bytes = [];
    for (var i = 0; i < 64; i++) bits += mcu;
    for (var j = 0; j < bits.length; j += 8) bytes.push(parseInt(bits.substr(j, 8), 2));
    return bytes;
})();

function u16(n) { return [(n >> 8) & 0xFF, n & 0xFF]; }

// Серии, суммирующиеся в 4096 пикселей.
function runs(maxRun) {
    var left = 4096, r = [];
    while (left > 0) {
        var n = Math.min(left, 1 + randInt(maxRun));
        r.push(n);
        left -= n;
    }
    return r;
}

function twoColourBits() {
    var bits = "";
    runs(64).forEach(function (n) {
        var s = (n - 1).toString(2);
        bits += "000000".substr(s.length) + s;
        if (n === 64) bits += randInt(2) ? "1" : "0";
    });
    while (bits.length % 8) bits += "0";
    var bytes = [];
    for (var j = 0; j < bits.length; j += 8) bytes.push(parseInt(bits.substr(j, 8), 2));
    return bytes;
}

function paletteRuns(colours) {
    var bytes = [];
    runs(64).forEach(function (n) { bytes.push(((n - 1) << 2) | randInt(colours)); });
    return bytes;
}

// Тело RLE-блока заданного типа: с палитрой (inline) или без (наследуется).
function rleBody(type, inline) {
    var b = [];
    if (type === 0) {
        return inline ? colour() : [randInt(256), randInt(256), randInt(256)]; // 3 байта всегда
    }
    var data = type === 1 ? twoColourBits() : paletteRuns(type === 2 ? 3 : 4);
    b = b.concat(u16(data.length));
    if (inline) {
        var n = type === 1 ? 2 : (type === 2 ? 3 : 4);
        for (var i = 0; i < n; i++) b = b.concat(colour());
    }
    return b.concat(data);
}

// meta[i] = { rle, pal } — что оригинал будет знать о блоке (тип RLE и наличие палитры).
function genFrame(cols, count, meta) {
    var bytes = [0], next = meta.slice();
    for (var i = 0; i < count; i++) {
        var left = i >= 1 ? next[i - 1] : null;
        var above = i >= cols ? next[i - cols] : null;
        var options = ["solid", "two", "pal3", "pal4", "jpeg"];
        if (meta[i]) options.push("same", "same");
        if (left) options.push("copyL", "inhL");
        if (above) options.push("copyA", "inhA");
        if (left && left.pal) options.push("swapL");
        if (above && above.pal) options.push("swapA");
        if (rand() < 0.01) options = ["bad"];
        var kind = options[randInt(options.length)];
        var edge = randInt(2); // zipType 0/1 и 2/3 различаются только обрезкой при отрисовке
        switch (kind) {
            case "solid": bytes = bytes.concat([edge << 5 | 0 << 2], rleBody(0, true)); next[i] = { rle: 0, pal: true }; break;
            case "two":   bytes = bytes.concat([edge << 5 | 1 << 2], rleBody(1, true)); next[i] = { rle: 1, pal: true }; break;
            case "pal3":  bytes = bytes.concat([edge << 5 | 2 << 2], rleBody(2, true)); next[i] = { rle: 2, pal: true }; break;
            case "pal4":  bytes = bytes.concat([edge << 5 | 3 << 2], rleBody(3, true)); next[i] = { rle: 3, pal: true }; break;
            case "jpeg":  bytes = bytes.concat([(2 + edge) << 5], u16(FLAT_JPEG.length), FLAT_JPEG); next[i] = { rle: 0, pal: false }; break;
            case "same":  bytes.push(4 << 5); break;
            case "copyA": bytes.push(5 << 5); next[i] = { rle: above.rle, pal: false }; break;
            case "copyL": bytes.push(6 << 5); next[i] = { rle: left.rle, pal: false }; break;
            case "inhL": case "inhA": {
                var src = kind === "inhL" ? left : above;
                bytes.push(edge << 5 | (kind === "inhL" ? 4 : 6) << 2);
                if (src.rle === 0) { next[i] = { rle: 0, pal: false }; break; } // копия изображения
                bytes = bytes.concat(rleBody(src.rle, false));
                next[i] = { rle: src.rle, pal: true };
                break;
            }
            case "swapL": case "swapA": {
                var s = kind === "swapL" ? left : above;
                bytes.push(edge << 5 | (kind === "swapL" ? 5 : 7) << 2);
                bytes = bytes.concat(rleBody(s.rle, false));
                next[i] = { rle: s.rle, pal: true };
                break;
            }
            case "bad": bytes.push(7 << 5); break;
        }
    }
    return { bytes: bytes, meta: next };
}

function hex(bytes) {
    var s = "";
    for (var i = 0; i < bytes.length; i++) s += (bytes[i] < 16 ? "0" : "") + bytes[i].toString(16);
    return s;
}

function toJava(bytes) {
    var a = new ByteArray(bytes.length);
    for (var i = 0; i < bytes.length; i++) a[i] = (bytes[i] << 24) >> 24;
    return a;
}

function blockCrc(image) {
    var px = new IntArray(4096);
    image.getRGB(0, 0, 64, 64, px, 0, 64);
    var crc = new CRC32(), buf = new ByteArray(4);
    for (var i = 0; i < px.length; i++) {
        var v = px[i] & 0xFFFFFF;
        buf[0] = 0; buf[1] = (v >> 16) << 24 >> 24; buf[2] = (v >> 8) << 24 >> 24; buf[3] = v << 24 >> 24;
        crc.update(buf, 0, 4);
    }
    return ("0000000" + Number(crc.getValue()).toString(16)).slice(-8);
}

// Небольшие экраны: покрывают обрезку краёв и соседство, а файл эталонов остаётся компактным.
var SIZES = [[64, 64], [128, 64], [200, 150], [320, 200], [450, 130]];
var FRAMES = 15;
var stats = { ok: 0, err: 0 };

SIZES.forEach(function (size) {
    var w = size[0], h = size[1];
    var cols = Math.ceil(w / 64), count = cols * Math.ceil(h / 64);
    out.println("size " + w + " " + h);
    var dec = new ImageDecoder(), meta = [];
    for (var f = 0; f < FRAMES; f++) {
        var q = randInt(10);
        JPEGData.setDqttags(q);
        var frame = genFrame(cols, count, meta);
        var result = null;
        try {
            result = dec.decodeRLEorJPEG1(toJava(frame.bytes), w, h);
        } catch (e) {
            result = null;
        }
        out.println("frame " + q + " " + (result ? 0 : 1) + " " + hex(frame.bytes));
        if (result) {
            stats.ok++;
            meta = frame.meta;
            for (var i = 0; i < result.length; i++) {
                if (result[i] && result[i].getImage()) out.println("block " + i + " " + blockCrc(result[i].getImage()));
            }
        } else {
            stats.err++;
            out.println("reset");
            dec = new ImageDecoder();
            meta = [];
        }
    }
});

out.close();
print("кадров: успешно " + stats.ok + ", с ошибкой " + stats.err);
