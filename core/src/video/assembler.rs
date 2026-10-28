//! Сборка видеокадра из фрагментов.
//!
//! Порт `com.kvm.KVMUtil.isComplete / setVar / setPremBuffer / setMinorBuffer / combine / swap`
//! и фильтра «тот же кадр» из `com.kvm.DrawThread.run`.
//!
//! Фрагмент: `fragIdx(2, BE) | frameNo(1) | полезные данные (до 220 байт)`.
//! Нулевой фрагмент — заголовок кадра (смещения от начала фрагмента):
//! ```text
//! [3..7]   totalLen (BE)  — сумма полезных байт фрагментов 1..n
//! [7]      бит7: diff / «кадр не изменился»; биты 6..0 — width[14:8]
//! [8]      width[7:0]
//! [9..11]  height (BE)
//! [12..14] cursorX (BE), [14..16] cursorY (BE)
//! [16]     биты 3..0 — уровень качества (1..=10), бит7 — I-кадр
//! ```
//!
//! Буферов два (A и B), как в оригинале: пока собирается кадр N, может начаться кадр N+1.
//! Номера кадров — знаковые байты Java, сравнения повторены с той же арифметикой.

use std::time::{Duration, Instant};

/// Полезная нагрузка фрагмента, по которой оригинал считает число слотов.
const FRAGMENT_PAYLOAD: usize = 220;
const HEADER_BYTES: usize = 3;

/// Собранный кадр.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssembledFrame {
    pub number: i8,
    pub width: u16,
    pub height: u16,
    pub cursor_x: u16,
    pub cursor_y: u16,
    /// Байт `[16]` заголовка как есть.
    pub info: u8,
    pub diff: bool,
    /// `[diff-флаг] ++ полезные данные фрагментов 1..n` — вход [`super::Decoder`].
    pub data: Vec<u8>,
}

impl AssembledFrame {
    /// Индекс таблицы DQT для JPEG-блоков (`JPEGData.setDqttags(size - 1)`).
    /// `None`, если уровень качества 0 — в оригинале это приводит к исключению.
    pub fn quality_index(&self) -> Option<usize> {
        ((self.info & 0x0F) as usize).checked_sub(1)
    }

    pub fn is_iframe(&self) -> bool {
        self.info & 0x80 != 0
    }
}

/// Результат обработки одного фрагмента.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PushOutcome {
    pub frame: Option<AssembledFrame>,
    /// Пришёл заголовок «кадр не изменился» — для счётчика FPS, данных нет.
    pub unchanged: bool,
    /// Поток рассинхронизировался, нужно запросить I-кадр (`I_REQ 0x08`).
    /// Частоту запросов ограничивает [`IFrameThrottle`].
    pub request_iframe: bool,
}

#[derive(Debug)]
struct Buffer {
    number: i8,
    total_len: usize,
    sum: usize,
    diff: bool,
    slots: Vec<Option<Vec<u8>>>,
}

impl Buffer {
    /// `setVar`: заголовок разбирается так же снисходительно, как в оригинале —
    /// короткий заголовок даёт нулевые поля, а не ошибку.
    fn from_header(fragment: &[u8]) -> Self {
        let total_len = be(fragment, 3, 4) as usize;
        let mut slots_count = total_len / FRAGMENT_PAYLOAD + 1;
        if !total_len.is_multiple_of(FRAGMENT_PAYLOAD) {
            slots_count += 1;
        }
        let mut slots = vec![None; slots_count];
        slots[0] = Some(fragment.to_vec());
        Self {
            number: fragment[2] as i8,
            total_len,
            sum: 0,
            diff: fragment.get(7).is_some_and(|b| b & 0x80 != 0),
            slots,
        }
    }

    /// Кладёт фрагмент в слот. `Err(())` — рассинхронизация (индекс вне диапазона
    /// или повтор с другим содержимым).
    fn store(&mut self, index: usize, fragment: &[u8]) -> Result<(), ()> {
        let slot = self.slots.get_mut(index).ok_or(())?;
        match slot {
            None => {
                self.sum += fragment.len().saturating_sub(HEADER_BYTES);
                *slot = Some(fragment.to_vec());
                Ok(())
            }
            Some(old) if old.as_slice() == fragment => Ok(()),
            Some(_) => Err(()),
        }
    }

    fn is_complete(&self) -> bool {
        self.sum == self.total_len
    }

    /// `combine`. `None`, если сумма сошлась, но какого-то слота нет
    /// (в оригинале — NullPointerException, который роняет поток отрисовки).
    fn combine(&self) -> Option<AssembledFrame> {
        let head = self.slots[0].as_deref()?;
        let mut data = Vec::with_capacity(self.total_len + 1);
        data.push(self.diff as u8);
        for slot in &self.slots[1..] {
            match slot {
                Some(fragment) => data.extend_from_slice(&fragment[HEADER_BYTES..]),
                None if data.len() == self.total_len + 1 => break,
                None => return None,
            }
        }
        Some(AssembledFrame {
            number: self.number,
            width: (((head.get(7).copied().unwrap_or(0) & 0x7F) as u16) << 8)
                | head.get(8).copied().unwrap_or(0) as u16,
            height: be(head, 9, 2) as u16,
            cursor_x: be(head, 12, 2) as u16,
            cursor_y: be(head, 14, 2) as u16,
            info: head.get(16).copied().unwrap_or(0),
            diff: self.diff,
            data,
        })
    }
}

/// `byteToIntCon`: big-endian, при выходе за границы — 0.
fn be(bytes: &[u8], offset: usize, len: usize) -> u32 {
    match bytes.get(offset..offset + len) {
        Some(s) => s.iter().fold(0u32, |acc, &b| (acc << 8) | b as u32),
        None => 0,
    }
}

#[derive(Debug)]
pub struct Assembler {
    a: Option<Buffer>,
    b: Option<Buffer>,
    now_display: i32,
    first_judge: bool,
}

impl Default for Assembler {
    fn default() -> Self {
        Self {
            a: None,
            b: None,
            now_display: 0,
            first_judge: true,
        }
    }
}

impl Assembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Обрабатывает фрагмент (уже расшифрованный, см. [`super::fragment_from_message`]).
    pub fn push(&mut self, fragment: &[u8]) -> PushOutcome {
        let mut out = PushOutcome::default();
        if fragment.len() < HEADER_BYTES {
            return out;
        }
        let index = be(fragment, 0, 2) as usize;
        let number = fragment[2] as i8;

        // DrawThread: при новом алгоритме сжатия заголовок с битом7 в [7] означает
        // «кадр не изменился» и дальше не передаётся.
        if index == 0 && fragment.get(7).is_some_and(|b| b & 0x80 != 0) {
            out.unchanged = true;
            return out;
        }

        if let Some(a) = self.a.as_mut() {
            if a.number == number {
                if a.store(index, fragment).is_err() {
                    self.reset();
                    out.request_iframe = true;
                } else if a.is_complete() {
                    self.complete_primary(&mut out);
                }
            } else {
                self.push_secondary(index, number, fragment, &mut out);
            }
        } else if index == 0 {
            self.a = Some(Buffer::from_header(fragment));
        } else {
            // Осиротевший фрагмент без заголовка: запросить I-кадр, если он «впереди» показанного
            // кадра меньше чем на 128 номеров либо «позади» на 128 и больше (переполнение байта).
            let n = number as i32;
            out.request_iframe = if self.now_display < n {
                n - self.now_display < 128
            } else {
                self.now_display - n >= 128
            };
        }
        out
    }

    /// `setPremBuffer`.
    fn complete_primary(&mut self, out: &mut PushOutcome) {
        let a = self
            .a
            .as_ref()
            .expect("вызывается только при наличии буфера A");
        if a.diff && (self.first_judge || (self.now_display + 1) as i8 != a.number) {
            self.reset();
            out.request_iframe = true;
            return;
        }
        let frame = a.combine();
        let (number, diff) = (a.number, a.diff);
        // swap(): B становится A.
        self.a = self.b.take();
        match frame {
            Some(frame) => {
                self.now_display = number as i32;
                if !diff {
                    self.first_judge = false;
                }
                out.frame = Some(frame);
            }
            None => {
                self.reset();
                out.request_iframe = true;
            }
        }
    }

    /// `setMinorBuffer`: фрагмент относится к другому кадру, чем буфер A.
    fn push_secondary(&mut self, index: usize, number: i8, fragment: &[u8], out: &mut PushOutcome) {
        let Some(b) = self.b.as_mut() else {
            if index == 0 {
                self.b = Some(Buffer::from_header(fragment));
            } else {
                out.request_iframe = true;
            }
            return;
        };
        if b.number != number || b.store(index, fragment).is_err() {
            self.reset();
            out.request_iframe = true;
            return;
        }
        if !b.is_complete() {
            return;
        }
        if b.diff && (self.first_judge || (self.now_display + 1) as i8 != b.number) {
            self.reset();
            out.request_iframe = true;
            return;
        }
        let frame = b.combine();
        let (number, diff) = (b.number, b.diff);
        // В оригинале после кадра из B сбрасываются оба буфера.
        self.reset();
        match frame {
            Some(frame) => {
                self.now_display = number as i32;
                if !diff {
                    self.first_judge = false;
                }
                out.frame = Some(frame);
            }
            None => out.request_iframe = true,
        }
    }

    fn reset(&mut self) {
        self.a = None;
        self.b = None;
    }
}

/// Ограничитель запросов I-кадра (`KVMUtil.sentIFrame`): первый сигнал только запускает
/// таймер, запрос уходит при следующем сигнале не раньше чем через 500 мс.
#[derive(Debug, Default)]
pub struct IFrameThrottle {
    started: Option<Instant>,
}

impl IFrameThrottle {
    pub const INTERVAL: Duration = Duration::from_millis(500);

    pub fn should_send(&mut self, now: Instant) -> bool {
        match self.started {
            None => {
                self.started = Some(now);
                false
            }
            Some(start) if now.duration_since(start) > Self::INTERVAL => {
                self.started = None;
                true
            }
            Some(_) => false,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Нарезает данные кадра на фрагменты так же, как это делает BMC.
    pub(crate) fn fragments(
        number: u8,
        width: u16,
        height: u16,
        info: u8,
        payload: &[u8],
    ) -> Vec<Vec<u8>> {
        let mut header = vec![0, 0, number];
        header.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        header.push(((width >> 8) as u8) & 0x7F);
        header.push(width as u8);
        header.extend_from_slice(&height.to_be_bytes());
        header.push(0);
        header.extend_from_slice(&10u16.to_be_bytes());
        header.extend_from_slice(&20u16.to_be_bytes());
        header.push(info);
        let mut out = vec![header];
        for (i, chunk) in payload.chunks(FRAGMENT_PAYLOAD).enumerate() {
            let idx = (i + 1) as u16;
            let mut f = idx.to_be_bytes().to_vec();
            f.push(number);
            f.extend_from_slice(chunk);
            out.push(f);
        }
        out
    }

    #[test]
    fn assembles_frame_in_order() {
        let payload: Vec<u8> = (0..500u32).map(|i| i as u8).collect();
        let mut asm = Assembler::new();
        let frags = fragments(1, 1024, 768, 0x85, &payload);
        let mut result = None;
        for f in &frags {
            let o = asm.push(f);
            assert!(!o.request_iframe);
            if o.frame.is_some() {
                result = o.frame;
            }
        }
        let frame = result.expect("кадр собран");
        assert_eq!((frame.width, frame.height), (1024, 768));
        assert_eq!((frame.cursor_x, frame.cursor_y), (10, 20));
        assert_eq!(frame.quality_index(), Some(4));
        assert!(frame.is_iframe());
        assert_eq!(frame.data[0], 0);
        assert_eq!(&frame.data[1..], &payload[..]);
    }

    #[test]
    fn assembles_frame_out_of_order() {
        let payload = vec![7u8; 700];
        let mut frags = fragments(3, 800, 600, 5, &payload);
        let header = frags.remove(0);
        frags.reverse();
        let mut asm = Assembler::new();
        assert!(asm.push(&header).frame.is_none());
        let frames: Vec<_> = frags.iter().filter_map(|f| asm.push(f).frame).collect();
        assert_eq!(frames.len(), 1);
        assert_eq!(&frames[0].data[1..], &payload[..]);
    }

    #[test]
    fn unchanged_header_is_reported_and_skipped() {
        let mut header = fragments(4, 800, 600, 5, &[1, 2, 3]).remove(0);
        header[7] |= 0x80;
        let o = Assembler::new().push(&header);
        assert!(o.unchanged);
        assert!(o.frame.is_none());
    }

    #[test]
    fn conflicting_duplicate_requests_iframe() {
        let frags = fragments(1, 800, 600, 5, &vec![1u8; 300]);
        let mut asm = Assembler::new();
        asm.push(&frags[0]);
        asm.push(&frags[1]);
        let mut bad = frags[1].clone();
        bad[5] ^= 0xFF;
        assert!(asm.push(&bad).request_iframe);
    }

    #[test]
    fn fragment_index_out_of_range_requests_iframe() {
        let frags = fragments(1, 800, 600, 5, &[1u8; 10]);
        let mut asm = Assembler::new();
        asm.push(&frags[0]);
        assert!(asm.push(&[0, 9, 1, 0]).request_iframe);
    }

    #[test]
    fn interleaved_next_frame_goes_to_secondary_buffer() {
        let f1 = fragments(1, 800, 600, 5, &vec![1u8; 300]);
        let f2 = fragments(2, 800, 600, 5, &[2u8; 100]);
        let mut asm = Assembler::new();
        asm.push(&f1[0]);
        asm.push(&f1[1]);
        asm.push(&f2[0]); // начало кадра 2, пока кадр 1 не собран
        let first = asm.push(&f1[2]).frame.expect("кадр 1");
        assert_eq!(first.number, 1);
        let second = asm.push(&f2[1]).frame.expect("кадр 2 из бывшего буфера B");
        assert_eq!(second.number, 2);
        assert_eq!(&second.data[1..], &[2u8; 100][..]);
    }

    #[test]
    fn diff_frame_before_reference_requests_iframe() {
        // Без фильтра «не изменился» (бит7 в [7]) diff-кадр попал бы сюда; проверяем ветку
        // setPremBuffer напрямую через буфер.
        let mut asm = Assembler::new();
        let mut buf = Buffer::from_header(&fragments(1, 64, 64, 5, &[0; 4]).remove(0));
        buf.diff = true;
        buf.sum = buf.total_len;
        asm.a = Some(buf);
        let mut out = PushOutcome::default();
        asm.complete_primary(&mut out);
        assert!(out.request_iframe);
        assert!(out.frame.is_none());
    }

    #[test]
    fn throttle_matches_original_timing() {
        let t0 = Instant::now();
        let mut t = IFrameThrottle::default();
        assert!(!t.should_send(t0));
        assert!(!t.should_send(t0 + Duration::from_millis(400)));
        assert!(t.should_send(t0 + Duration::from_millis(501)));
        assert!(!t.should_send(t0 + Duration::from_millis(502)));
    }
}
