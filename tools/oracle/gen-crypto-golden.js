// Генератор эталонов для криптографии: прогоняет ОРИГИНАЛЬНЫЙ com.kvm.AESHandler.
//
//   jjs -cp <extract/pe> tools/oracle/gen-crypto-golden.js -- <выходной файл>
//
// Формат: по строке на случай, `<имя> <hex входов через ':'> <hex результата>`.
// Байтовые перестановки (wordswap) и преобразование BladeThread.convert воспроизведены
// здесь вручную — в оригинале это приватный код внутри KVMApplet/BladeThread.

var AESHandler = Java.type("com.kvm.AESHandler");
var PrintWriter = Java.type("java.io.PrintWriter");
var ByteArray = Java.type("byte[]");
var StringBuilder = Java.type("java.lang.StringBuilder");
var Random = Java.type("java.util.Random");

var out = new PrintWriter(arguments[0], "UTF-8");
var rng = new Random(777);

function randomBytes(n) {
    var a = new ByteArray(n);
    rng.nextBytes(a);
    return a;
}
function hex(a) {
    var s = "";
    for (var i = 0; i < a.length; i++) {
        var v = a[i] & 0xFF;
        s += (v < 16 ? "0" : "") + v.toString(16);
    }
    return s;
}
function str(s) {
    var b = new ByteArray(s.length);
    for (var i = 0; i < s.length; i++) b[i] = s.charCodeAt(i);
    return b;
}
function wordswap(a) {
    for (var i = 0; i + 3 < a.length; i += 4) {
        var t = a[i]; a[i] = a[i + 3]; a[i + 3] = t;
        t = a[i + 1]; a[i + 1] = a[i + 2]; a[i + 2] = t;
    }
    return a;
}
function convert(data, len) { // BladeThread.convert: (char) от знакового байта
    var sb = new StringBuilder(len);
    for (var i = 0; i < len; i++) sb.append(String.fromCharCode(data[i] & 0xFFFF));
    return sb.toString();
}

// 1. encodeKey: wordswap(PBKDF2(verifyValue, salt = user_iv, iter, 24)).
[["PBKDF2WithHmacSHA1", 5000], ["PBKDF2WithHmacSHA256", 5000], ["PBKDF2WithHmacSHA256", 1000]].forEach(function (k) {
    ["0", "7", "123456789", "4294967295"].forEach(function (verify) {
        var iv = randomBytes(16);
        var key = wordswap(AESHandler.getcodekey(verify, 24, iv, k[0], k[1]));
        out.println("encode_key " + k[0] + ":" + k[1] + ":" + hex(str(verify)) + ":" + hex(iv) + " " + hex(key));
    });
});

// 2. kvm_key из сообщения KVM_KEY_SET (48 байт, зашифрованы user_key/user_iv).
[["PBKDF2WithHmacSHA1", 5000], ["PBKDF2WithHmacSHA256", 5000]].forEach(function (k) {
    for (var n = 0; n < 4; n++) {
        var userKey = randomBytes(16), userIv = randomBytes(16);
        var msg = randomBytes(48);
        if (n < 2) { // реалистичный случай: ASCII-пароль; иначе — произвольные байты
            for (var i = 0; i < 32; i++) msg[i] = 0x30 + rng.nextInt(43);
        }
        var cipher = AESHandler.aes_cbc_128_encrypt(msg, userKey, userIv);
        var plain = AESHandler.aes_cbc_128_decrypt(cipher, userKey, userIv);
        var salt = new ByteArray(16);
        java.lang.System.arraycopy(plain, 32, salt, 0, 16);
        var kvmKey = wordswap(AESHandler.getcodekey(convert(plain, 32), 48, salt, k[0], k[1]));
        out.println("kvm_key " + k[0] + ":" + k[1] + ":" + hex(userKey) + ":" + hex(userIv) + ":" + hex(cipher) + " " + hex(kvmKey));
    }
});

// 3. Шифрование ввода и команд сессионным ключом, расшифровка видео.
for (var n = 0; n < 6; n++) {
    var kvmKey = randomBytes(48);
    var len = [4, 6, 8, 16, 1, 15][n];
    var src = randomBytes(len);
    out.println("secure_encry " + hex(kvmKey) + ":" + hex(src) + " " + hex(AESHandler.secure_encry(src, kvmKey, len)));
    out.println("kvm_encry " + hex(kvmKey) + ":" + hex(src) + " " + hex(AESHandler.kvm_encry(src, kvmKey, len)));
    var block = randomBytes(32);
    out.println("decry " + hex(kvmKey) + ":" + hex(block) + " " + hex(AESHandler.decry(block, kvmKey, 32)));
}

// 4. Старый режим (compress=0): ключ из codeKey, IV = 0.
[0, 1, -1, 123456789, -2147483648].forEach(function (codeKey) {
    var src = randomBytes(8);
    out.println("encry " + (codeKey >>> 0).toString(16) + ":" + hex(src) + " " + hex(AESHandler.encry(src, codeKey, 8)));
});

out.close();
print("ok");
