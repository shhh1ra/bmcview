#!/usr/bin/env bash
# Генерирует core/src/video/tables.rs из декомпилированных JPEGData.java и DQTZData.java.
# Таблицы переносятся машинно, чтобы исключить ошибки ручного копирования.
#
# Использование: tools/gen-jpeg-tables.sh <каталог с декомпилированными com/library/decoder>
set -euo pipefail

SRC="${1:?укажите каталог com/library/decoder из декомпиляции}"
OUT="$(dirname "$0")/../core/src/video/tables.rs"

convert() {
    # Строки вида: static final byte[] NAME = new byte[]{-1, -40, ...};
    # Java byte знаковый, поэтому отрицательные значения переводятся в u8 (+256).
    awk '
    /static final byte\[\] [A-Z0-9_]+ = new byte\[\]\{/ {
        name = $0
        sub(/.*byte\[\] /, "", name); sub(/ =.*/, "", name)
        body = $0
        sub(/.*\{/, "", body); sub(/\}.*/, "", body)
        n = split(body, v, /, */)
        printf "pub const %s: [u8; %d] = [", name, n
        for (i = 1; i <= n; i++) {
            x = v[i] + 0
            if (x < 0) x += 256
            printf "%s%d", (i > 1 ? ", " : ""), x
        }
        print "];"
    }' "$1"
}

{
    echo "// @generated tools/gen-jpeg-tables.sh — не редактировать вручную."
    echo "// Источник: com.library.decoder.JPEGData / DQTZData (оригинальный Java-клиент)."
    echo
    convert "$SRC/JPEGData.java"
    echo
    convert "$SRC/DQTZData.java"
    echo
    echo "/// DQT-сегменты (Y, Cb, Cr) по индексу качества 0..=9 (качество 10..=100)."
    printf "pub const DQT_SETS: [[&[u8]; 3]; 10] = [\n"
    for q in 10 20 30 40 50 60 70 80 90 100; do
        printf "    [&HEAD_SYN_DQT_Y_%s, &HEAD_SYN_DQT_U_%s, &HEAD_SYN_DQT_V_%s],\n" $q $q $q
    done
    echo "];"
} > "$OUT"

echo "записано: $OUT"
