//! Guest console output made safe for the host terminal (design §Security invariants): a
//! streaming sanitiser that keeps SGR (colour and style), drops every other escape sequence, every
//! C0 control but `\t` and `\n` (and DEL) and every C1 control: U+0080..=U+009F encoded in UTF-8
//! and raw bytes `0x80..=0x9f` outside a valid UTF-8 sequence (8-bit CSI is `0x9b`). State is
//! carried across chunks, so the output does not depend on how the input was split.
//!
//! Untrusted bytes arrive as [`GuestBytes`]; the only way to bytes for a terminal is
//! [`Sanitiser::feed`], which returns [`Printable`]. A sequence still open when the input ends is
//! never shown, and neither is a UTF-8 sequence cut off by the end of the input.

/// The longest SGR sequence kept, in bytes; a longer one is dropped like any other escape.
pub const MAX_SGR: usize = 256;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// Bytes from the guest's console: untrusted, opaque.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GuestBytes(Vec<u8>);

impl GuestBytes {
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

/// Sanitised bytes, safe to write to a terminal. Only [`Sanitiser::feed`] makes them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Printable(Vec<u8>);

impl Printable {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

/// Where the sanitiser is inside the byte stream (ECMA-48 shapes, as Python's patterns read them).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum State {
    #[default]
    Ground,
    /// After ESC.
    Escape,
    /// After ESC and intermediate bytes (`0x20..=0x2f`).
    EscapeIntermediate,
    /// After `ESC [` and parameter bytes; `sgr` while it can still be an SGR worth keeping (the
    /// bytes so far are held).
    Csi { sgr: bool },
    /// After a CSI's intermediate bytes: dropped whatever its final byte.
    CsiIntermediate,
    /// In the body of an OSC, DCS, PM, APC or SOS string.
    Text,
    /// ESC inside such a body: `\` ends the string, anything else starts a new sequence.
    TextEscape,
    /// Inside a multi-byte UTF-8 sequence (bytes so far are held): `left` continuation bytes are
    /// still due and the next one must lie in `lo..=hi`.
    Utf8 { left: u8, lo: u8, hi: u8 },
}

/// What to do with the byte just looked at.
enum Next {
    Consumed,
    /// Handle it again in the state just set.
    Again,
}

/// The streaming sanitiser; one per console.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Sanitiser {
    state: State,
    /// The SGR sequence being read (from its ESC), while `Csi { sgr: true }`.
    held: Vec<u8>,
}

impl Sanitiser {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The printable part of `chunk`, given everything fed before.
    pub fn feed(&mut self, chunk: &GuestBytes) -> Printable {
        let mut out = Vec::with_capacity(chunk.0.len());
        for &byte in &chunk.0 {
            while let Next::Again = self.step(byte, &mut out) {}
        }
        Printable(out)
    }

    fn step(&mut self, byte: u8, out: &mut Vec<u8>) -> Next {
        match self.state {
            State::Ground => {
                match byte {
                    ESC => self.state = State::Escape,
                    b'\t' | b'\n' => out.push(byte),
                    // C0, DEL, and C1 as raw bytes (a valid sequence's continuation byte is never
                    // seen here).
                    0x00..=0x1f | 0x7f..=0x9f => {}
                    0xc2..=0xf4 => return self.lead(byte),
                    _ => out.push(byte),
                }
                Next::Consumed
            }
            State::Escape => match byte {
                b'[' => {
                    self.held.clear();
                    self.held.extend([ESC, b'[']);
                    self.goto(State::Csi { sgr: true })
                }
                b']' | b'P' | b'^' | b'_' | b'X' => self.goto(State::Text),
                0x20..=0x2f => self.goto(State::EscapeIntermediate),
                0x30..=0x7e => self.goto(State::Ground),
                _ => self.again(State::Ground),
            },
            State::EscapeIntermediate => match byte {
                0x20..=0x2f => Next::Consumed,
                0x30..=0x7e => self.goto(State::Ground),
                _ => self.again(State::Ground),
            },
            State::Csi { sgr } => match byte {
                0x30..=0x3f => {
                    let keep = sgr
                        && matches!(byte, b'0'..=b'9' | b';' | b':')
                        && self.held.len() < MAX_SGR.saturating_sub(1);
                    if keep {
                        self.held.push(byte);
                    } else {
                        self.held.clear();
                    }
                    self.goto(State::Csi { sgr: keep })
                }
                0x20..=0x2f => self.goto(State::CsiIntermediate),
                0x40..=0x7e => {
                    if sgr && byte == b'm' {
                        out.append(&mut self.held);
                        out.push(byte);
                    }
                    self.goto(State::Ground)
                }
                _ => self.again(State::Ground),
            },
            State::CsiIntermediate => match byte {
                0x20..=0x2f => Next::Consumed,
                0x40..=0x7e => self.goto(State::Ground),
                _ => self.again(State::Ground),
            },
            State::Text => match byte {
                BEL => self.goto(State::Ground),
                ESC => self.goto(State::TextEscape),
                _ => Next::Consumed,
            },
            State::TextEscape => match byte {
                b'\\' => self.goto(State::Ground),
                _ => self.again(State::Escape),
            },
            State::Utf8 { left, lo, hi } => {
                if !(lo..=hi).contains(&byte) {
                    // Not the sequence the lead promised: the bytes held are stray. A lead or a
                    // continuation above C1 is plain text; a continuation in C1 is dropped.
                    out.extend(self.held.iter().filter(|b| !(0x80..=0x9f).contains(*b)));
                    return self.again(State::Ground);
                }
                self.held.push(byte);
                if left > 1 {
                    self.state = State::Utf8 {
                        left: left.saturating_sub(1),
                        lo: 0x80,
                        hi: 0xbf,
                    };
                    return Next::Consumed;
                }
                // Complete: U+0080..=U+009F is `C2 80..=9F`, the only C1 encoding.
                match self.held.as_slice() {
                    [0xc2, 0x80..=0x9f] => {}
                    _ => out.append(&mut self.held),
                }
                self.goto(State::Ground)
            }
        }
    }

    /// A byte that opens a multi-byte UTF-8 sequence (RFC 3629 ranges: no overlongs, surrogates
    /// or code points above U+10FFFF).
    fn lead(&mut self, byte: u8) -> Next {
        let (left, lo, hi) = match byte {
            0xe0 => (2, 0xa0, 0xbf),
            0xed => (2, 0x80, 0x9f),
            0xf0 => (3, 0x90, 0xbf),
            0xf4 => (3, 0x80, 0x8f),
            0xe1..=0xef => (2, 0x80, 0xbf),
            0xf1..=0xf3 => (3, 0x80, 0xbf),
            _ => (1, 0x80, 0xbf),
        };
        self.held.clear();
        self.held.push(byte);
        self.state = State::Utf8 { left, lo, hi };
        Next::Consumed
    }

    fn goto(&mut self, state: State) -> Next {
        if !matches!(state, State::Csi { sgr: true }) {
            self.held.clear();
        }
        self.state = state;
        Next::Consumed
    }

    fn again(&mut self, state: State) -> Next {
        self.held.clear();
        self.state = state;
        Next::Again
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::testing;

    fn whole(input: &[u8]) -> Vec<u8> {
        Sanitiser::new()
            .feed(&GuestBytes::new(input.to_vec()))
            .into_bytes()
    }

    fn chunked(input: &[u8], cuts: &[usize]) -> Vec<u8> {
        let mut sanitiser = Sanitiser::new();
        let mut out = Vec::new();
        let mut rest = input;
        for &cut in cuts {
            let (head, tail) = rest.split_at(cut.min(rest.len()));
            out.extend_from_slice(sanitiser.feed(&GuestBytes::new(head.to_vec())).as_bytes());
            rest = tail;
        }
        out.extend_from_slice(sanitiser.feed(&GuestBytes::new(rest.to_vec())).as_bytes());
        out
    }

    #[test]
    fn golden_samples_match_the_python_sanitiser() {
        let cases = testing::golden_byte_cases("console", "in", "out").unwrap();
        assert!(cases.len() > 1);
        for case in cases {
            assert_eq!(whole(&case.input), case.output, "{}", case.name);
        }
    }

    #[test]
    fn every_state_and_edge_sanitises_as_specified() {
        let long_sgr = [b"\x1b[".as_slice(), &[b'1'; 253], b"m"].concat();
        let too_long_sgr = [b"\x1b[".as_slice(), &[b'1'; 254], b"m"].concat();
        testing::table(
            &[
                // Ground
                (b"a\tb\nc".to_vec(), b"a\tb\nc".to_vec()),
                (b"\x00\x01\x1f\x7f\r".to_vec(), b"".to_vec()),
                (b" ~\xa0\xff".to_vec(), b" ~\xa0\xff".to_vec()),
                // Ground: raw C1 bytes are dropped (0x9b is 8-bit CSI), their neighbours are not.
                (b"\x7f\x80\x8d\x9b\x9fx".to_vec(), b"x".to_vec()),
                (b"\x9b31mx\x9d0;t\x07".to_vec(), b"31mx0;t".to_vec()),
                (
                    b"\x80\x9f\xa0\xbf\xc0\xc1\xf5\xff".to_vec(),
                    b"\xa0\xbf\xc0\xc1\xf5\xff".to_vec(),
                ),
                // Escape
                (b"\x1b7x".to_vec(), b"x".to_vec()),
                (b"\x1b~x".to_vec(), b"x".to_vec()),
                (b"\x1b\x7fx".to_vec(), b"x".to_vec()),
                (b"\x1b\nx".to_vec(), b"\nx".to_vec()),
                (b"\x1b\x1b[1mx".to_vec(), b"\x1b[1mx".to_vec()),
                (b"\x1b\x80x".to_vec(), b"x".to_vec()),
                (b"\x1b\xa0x".to_vec(), b"\xa0x".to_vec()),
                // EscapeIntermediate
                (b"\x1b( Bx".to_vec(), b"x".to_vec()),
                (b"\x1b#\x07x".to_vec(), b"x".to_vec()),
                // Csi
                (b"\x1b[mx".to_vec(), b"\x1b[mx".to_vec()),
                (b"\x1b[0;31:2mx".to_vec(), b"\x1b[0;31:2mx".to_vec()),
                (b"\x1b[?1mx".to_vec(), b"x".to_vec()),
                (b"\x1b[1<mx".to_vec(), b"x".to_vec()),
                (b"\x1b[2Jx".to_vec(), b"x".to_vec()),
                (b"\x1b[1\x07x".to_vec(), b"x".to_vec()),
                (b"\x1b[1\x1b[2mx".to_vec(), b"\x1b[2mx".to_vec()),
                (long_sgr.clone(), long_sgr),
                (too_long_sgr, b"".to_vec()),
                // CsiIntermediate
                (b"\x1b[1 mx".to_vec(), b"x".to_vec()),
                (b"\x1b[1 !qx".to_vec(), b"x".to_vec()),
                (b"\x1b[ 1mx".to_vec(), b"1mx".to_vec()),
                // Text
                (b"\x1b]0;t\x07x".to_vec(), b"x".to_vec()),
                (b"\x1bPq\nz\x1b\\x".to_vec(), b"x".to_vec()),
                (b"\x1b^a\x1b_b\x07x".to_vec(), b"x".to_vec()),
                (b"\x1bXs\x1b[3mx".to_vec(), b"\x1b[3mx".to_vec()),
                (b"\x1b]\x1b\x1b\\x".to_vec(), b"x".to_vec()),
                // Utf8: valid sequences are text, U+0080..=U+009F is dropped.
                (
                    "é€𐀀\u{a0}\u{7ff}".as_bytes().to_vec(),
                    "é€𐀀\u{a0}\u{7ff}".as_bytes().to_vec(),
                ),
                (b"\xc2\x80\xc2\x9bx\xc2\x9f".to_vec(), b"x".to_vec()),
                (b"\xc2\xa0\xdf\xbf".to_vec(), b"\xc2\xa0\xdf\xbf".to_vec()),
                (
                    b"\xe2\x82\xac\xe0\xa0\x80\xed\x9f\xbf".to_vec(),
                    b"\xe2\x82\xac\xe0\xa0\x80\xed\x9f\xbf".to_vec(),
                ),
                (
                    b"\xf0\x90\x80\x80\xf4\x8f\xbf\xbf".to_vec(),
                    b"\xf0\x90\x80\x80\xf4\x8f\xbf\xbf".to_vec(),
                ),
                // Utf8: a broken sequence keeps its lead and continuations above C1.
                (b"\xe2\x82x".to_vec(), b"\xe2x".to_vec()),
                (b"\xe1\xa0x".to_vec(), b"\xe1\xa0x".to_vec()),
                (b"\xf0\x90\xa0x".to_vec(), b"\xf0\xa0x".to_vec()),
                (b"\xc2\n".to_vec(), b"\xc2\n".to_vec()),
                (b"\xc3\x1b[1mx".to_vec(), b"\xc3\x1b[1mx".to_vec()),
                (b"\xe2\xc3\xa9".to_vec(), b"\xe2\xc3\xa9".to_vec()),
                (b"\xe0\x80\x80".to_vec(), b"\xe0".to_vec()),
                (b"\xed\xa0\x80".to_vec(), b"\xed\xa0".to_vec()),
                (b"\xf0\x80\x80\x80".to_vec(), b"\xf0".to_vec()),
                (b"\xf4\x90\x80".to_vec(), b"\xf4".to_vec()),
                // An open sequence at the end is never shown.
                (b"x\xc2".to_vec(), b"x".to_vec()),
                (b"x\xe2\x82".to_vec(), b"x".to_vec()),
                (b"x\xf0\x9f\x98".to_vec(), b"x".to_vec()),
                (b"x\x1b[31".to_vec(), b"x".to_vec()),
                (b"x\x1b]52;c;Zm9v".to_vec(), b"x".to_vec()),
                (b"x\x1b".to_vec(), b"x".to_vec()),
            ],
            |input| whole(input),
        );
    }

    #[test]
    fn a_sequence_split_across_chunks_is_judged_whole() {
        assert_eq!(chunked(b"a\x1b[3", &[]), b"a");
        let mut sanitiser = Sanitiser::new();
        let parts: Vec<Vec<u8>> = [b"a\x1b".as_slice(), b"[3", b"1", b"mb\x1b]0;", b"t\x07c"]
            .iter()
            .map(|part| sanitiser.feed(&GuestBytes::new(part.to_vec())).into_bytes())
            .collect();
        assert_eq!(
            parts,
            [
                b"a".to_vec(),
                vec![],
                vec![],
                b"\x1b[31mb".to_vec(),
                b"c".to_vec()
            ]
        );
    }

    #[test]
    fn a_utf8_sequence_split_across_chunks_is_judged_whole() {
        let mut sanitiser = Sanitiser::new();
        let parts: Vec<Vec<u8>> = [
            b"\xc2".as_slice(),
            b"\x9b",
            b"\xe2\x82",
            b"\xacx\xc2",
            b"\xa9",
        ]
        .iter()
        .map(|part| sanitiser.feed(&GuestBytes::new(part.to_vec())).into_bytes())
        .collect();
        assert_eq!(
            parts,
            [
                vec![],
                vec![],
                vec![],
                b"\xe2\x82\xacx".to_vec(),
                b"\xc2\xa9".to_vec()
            ]
        );
    }

    /// Bytes that matter to the state machine, plus anything.
    fn console_bytes() -> impl Strategy<Value = Vec<u8>> {
        let byte = prop_oneof![
            4 => Just(ESC),
            3 => prop::sample::select(b"[]P^_X\\m;:0123456789?\x07\n\t\r !~".to_vec()),
            3 => any::<u8>(),
            2 => prop::sample::select(b"abc".to_vec()),
            // UTF-8 leads, C1 and its neighbours, continuation edges.
            4 => prop::sample::select(
                b"\xc2\xc3\xe0\xe2\xed\xf0\xf4\x80\x8f\x90\x9b\x9f\xa0\xa9\xbf".to_vec()
            ),
        ];
        prop::collection::vec(byte, 0..120)
    }

    /// No C1 control: no `U+0080..=U+009F` in the valid UTF-8 of `out` and no raw `0x80..=0x9f`
    /// byte outside it (the bytes of an invalid or cut-off sequence count as raw).
    fn no_c1(out: &[u8]) -> bool {
        let mut rest = out;
        loop {
            match std::str::from_utf8(rest) {
                Ok(text) => return !text.chars().any(|c| ('\u{80}'..='\u{9f}').contains(&c)),
                Err(error) => {
                    let (valid, bad) = rest.split_at(error.valid_up_to());
                    let skip = error.error_len().unwrap_or(bad.len());
                    let (broken, tail) = bad.split_at(skip);
                    let clean = no_c1(valid) && !broken.iter().any(|b| (0x80..=0x9f).contains(b));
                    if !clean {
                        return false;
                    }
                    rest = tail;
                }
            }
        }
    }

    /// Every ESC in `out` starts `ESC [ [0-9;:]* m`; no other C0 control but `\t`, `\n`; no DEL.
    fn only_sgr_and_text(out: &[u8]) -> bool {
        let mut rest = out;
        while let Some((&byte, tail)) = rest.split_first() {
            rest = tail;
            match byte {
                ESC => {
                    let Some(body) = rest.strip_prefix(b"[") else {
                        return false;
                    };
                    let params = body
                        .iter()
                        .take_while(|b| matches!(b, b'0'..=b'9' | b';' | b':'))
                        .count();
                    let Some((b'm', after)) = body.get(params..).and_then(<[u8]>::split_first)
                    else {
                        return false;
                    };
                    rest = after;
                }
                b'\t' | b'\n' => {}
                0x00..=0x1f | 0x7f => return false,
                _ => {}
            }
        }
        true
    }

    proptest! {
        #[test]
        fn output_for_any_chunking_equals_output_for_the_whole_input(
            input in console_bytes(),
            cuts in prop::collection::vec(0..40_usize, 0..8),
        ) {
            prop_assert_eq!(chunked(&input, &cuts), whole(&input));
        }

        #[test]
        fn output_has_no_escape_outside_sgr_and_no_control_but_tab_and_newline_and_no_c1(
            input in console_bytes(),
        ) {
            let out = whole(&input);
            prop_assert!(only_sgr_and_text(&out), "{:?} -> {:?}", input, out);
            prop_assert!(no_c1(&out), "{:?} -> {:?}", input, out);
        }
    }

    #[test]
    fn the_output_check_refuses_what_must_not_reach_a_terminal() {
        testing::table(
            &[
                (b"a\x1b[1;2:3mb\t\n".as_slice(), true),
                (b"\x1b[m", true),
                (b"\x1b", false),
                (b"\x1b]0;x\x07", false),
                (b"\x1b[1J", false),
                (b"\x1b[1", false),
                (b"\r", false),
                (b"\x7f", false),
            ],
            |out| only_sgr_and_text(out),
        );
    }

    #[test]
    fn the_c1_check_refuses_raw_and_encoded_c1_controls() {
        testing::table(
            &[
                (b"a\xc3\xa9\xe2\x82\xac\xa0".as_slice(), true),
                (b"\xc2\xa0\xc2\x7f", true),
                (b"\x9b", false),
                (b"\x80", false),
                (b"a\xc2\x80", false),
                (b"\xc2\x9f\xc3\xa9", false),
                (b"\xe2\x82", false),
                (b"\xe2\x82\xac\x85", false),
                (b"\xe2x", true),
            ],
            |out| no_c1(out),
        );
    }
}
